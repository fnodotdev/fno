//! The scheduled-spawn store: rows the daemon fires through the normal spawn
//! gate at a time (`--when`), on an interval (`--every`) or when a command
//! exits 0 (`--on`).
//!
//! Rows live in a `scheduled_spawns` table beside the registry tables in
//! graph.db, through the same store_conn door, so a row survives a daemon
//! restart with no new state-root row (docs/state-root-inventory.md db/).
//! Time parsing accepts the machine's local zone by default, a named IANA
//! zone or a US abbreviation, and UTC; stored instants are UTC only.

use crate::paths::AgentsHome;
use chrono::{DateTime, Local, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DDL: &str = "CREATE TABLE IF NOT EXISTS scheduled_spawns (
    id TEXT PRIMARY KEY,
    fire_at_utc TEXT,
    trigger_kind TEXT NOT NULL,
    interval_secs INTEGER,
    on_command TEXT,
    grace_secs INTEGER NOT NULL,
    force INTEGER NOT NULL,
    node TEXT,
    argv_json TEXT NOT NULL,
    cwd TEXT NOT NULL,
    scheduler_session TEXT NOT NULL,
    state TEXT NOT NULL,
    last_gate_reading TEXT,
    created_at_utc TEXT NOT NULL,
    last_attempt_utc TEXT
);
CREATE INDEX IF NOT EXISTS scheduled_spawns_due
    ON scheduled_spawns(state, fire_at_utc);";

/// Default grace for a timed row: refused fires retry every 5 min for an
/// hour, then the row reads `missed`.
pub const DEFAULT_GRACE_SECS: i64 = 3600;
/// Default grace for an `--on` row, counted from creation: a command that
/// never exits 0 releases the idle hold after a week.
pub const DEFAULT_ON_GRACE_SECS: i64 = 7 * 24 * 3600;
/// The cadence a refused fire retries inside the grace window.
pub const RETRY_SECS: i64 = 300;

// ---------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------

/// Why the daemon fires a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    /// One fire at `fire_at_utc`.
    At,
    /// Re-fires every `interval_secs` after each fire.
    Every { interval_secs: i64 },
    /// Fires when `command` exits 0, polled each arm tick.
    On { command: String },
}

/// Where a row stands in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowState {
    Pending,
    Fired,
    Missed,
    Skipped,
    Cancelled,
}

impl RowState {
    pub fn as_str(self) -> &'static str {
        match self {
            RowState::Pending => "pending",
            RowState::Fired => "fired",
            RowState::Missed => "missed",
            RowState::Skipped => "skipped",
            RowState::Cancelled => "cancelled",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "pending" => RowState::Pending,
            "fired" => RowState::Fired,
            "missed" => RowState::Missed,
            "skipped" => RowState::Skipped,
            "cancelled" => RowState::Cancelled,
            _ => return None,
        })
    }
}

/// One queued spawn the daemon owns.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledSpawn {
    pub id: String,
    /// Absent for an `On` row, which polls instead of waiting for a clock.
    pub fire_at_utc: Option<DateTime<Utc>>,
    pub trigger: Trigger,
    pub grace_secs: i64,
    pub force: bool,
    pub node: Option<String>,
    /// The recorded spawn argv after the schedule flags, re-entered through
    /// the front door at fire time (the pr_watch fire_argv shape).
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// The session that scheduled the row; the fire passes it as
    /// `--spawned-by` so lead share counts against it while it lives.
    pub scheduler_session: String,
    pub state: RowState,
    /// The last gate verdict the fire read (including one a forced fire
    /// overrode).
    pub last_gate_reading: Option<String>,
    pub created_at_utc: DateTime<Utc>,
    pub last_attempt_utc: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule store at {path}: {source}")]
    Store { path: PathBuf, source: String },
    #[error(
        "scheduled spawn {id}: no such row; run `fno agent spawn --list-scheduled` to see live ids"
    )]
    UnknownId { id: String },
    #[error("scheduled spawn {id}: stored row is corrupt: {detail}")]
    CorruptRow { id: String, detail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WhenParseError {
    #[error("cannot read time {input}: {reason}")]
    Malformed { input: String, reason: String },
    #[error(
        "{input} is ambiguous on this date (the clocks fall back); it means either {first} or {second} UTC. Name the UTC instant, e.g. --when 06:30Z"
    )]
    Ambiguous {
        input: String,
        first: String,
        second: String,
    },
    #[error("{input} falls in a DST gap on this date; the next valid local time is {next}")]
    Gap { input: String, next: String },
}

fn store_err(path: &Path, source: impl std::fmt::Display) -> ScheduleError {
    ScheduleError::Store {
        path: path.to_path_buf(),
        source: source.to_string(),
    }
}

// ---------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------

/// graph.db, resolved the way the registry resolves it: the agents home's
/// registry anchor's parent is the state root, whose graph anchor carries
/// the .db twin.
pub fn database_path(home: &AgentsHome) -> Result<PathBuf, ScheduleError> {
    let registry = home.registry_json();
    crate::runtime::registry_read::database_path(&registry).ok_or_else(|| ScheduleError::Store {
        path: registry,
        source: "no state root to anchor the schedule store".into(),
    })
}

fn open(home: &AgentsHome) -> Result<Connection, ScheduleError> {
    let database = database_path(home)?;
    let connection = crate::backlog::store_conn::open_write(&database)
        .map_err(|source| store_err(&database, source))?;
    connection
        .execute_batch(DDL)
        .map_err(|e| store_err(&database, e))?;
    Ok(connection)
}

fn read_only(home: &AgentsHome) -> Result<Option<Connection>, ScheduleError> {
    let database = database_path(home)?;
    if !database.exists() {
        // A fresh install has no graph.db yet: the schedule store answers
        // empty rather than erroring, so --list-scheduled and the daemon
        // arm degrade to "no rows" on an unconfigured machine.
        return Ok(None);
    }
    Ok(Some(
        crate::backlog::store_conn::open_read(&database)
            .map_err(|source| store_err(&database, source))?,
    ))
}

fn mint_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    format!("ss-{:08x}", rng.gen::<u32>())
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::format::SecondsFormat::Micros, true)
}

fn parse_ts(id: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, ScheduleError> {
    raw.map(|s| {
        DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| ScheduleError::CorruptRow {
                id: id.to_string(),
                detail: format!("timestamp {s:?}: {e}"),
            })
    })
    .transpose()
}

fn row_from(row: &rusqlite::Row) -> Result<ScheduledSpawn, ScheduleError> {
    let id: String = row.get("id")?;
    let state_raw: String = row.get("state")?;
    let state = RowState::parse(&state_raw).ok_or_else(|| ScheduleError::CorruptRow {
        id: id.clone(),
        detail: format!("unknown state {state_raw:?}"),
    })?;
    let kind: String = row.get("trigger_kind")?;
    let interval_secs: Option<i64> = row.get("interval_secs")?;
    let on_command: Option<String> = row.get("on_command")?;
    let trigger = match kind.as_str() {
        "at" => Trigger::At,
        "every" => Trigger::Every {
            interval_secs: interval_secs.ok_or_else(|| ScheduleError::CorruptRow {
                id: id.clone(),
                detail: "every row carries no interval_secs".into(),
            })?,
        },
        "on" => Trigger::On {
            command: on_command.ok_or_else(|| ScheduleError::CorruptRow {
                id: id.clone(),
                detail: "on row carries no command".into(),
            })?,
        },
        other => {
            return Err(ScheduleError::CorruptRow {
                id: id.clone(),
                detail: format!("unknown trigger kind {other:?}"),
            })
        }
    };
    let argv_json: String = row.get("argv_json")?;
    let argv: Vec<String> =
        serde_json::from_str(&argv_json).map_err(|e| ScheduleError::CorruptRow {
            id: id.clone(),
            detail: format!("argv_json: {e}"),
        })?;
    Ok(ScheduledSpawn {
        fire_at_utc: parse_ts(&id, row.get::<_, Option<String>>("fire_at_utc")?.as_deref())?,
        trigger,
        grace_secs: row.get("grace_secs")?,
        force: row.get::<_, i64>("force")? != 0,
        node: row.get("node")?,
        argv,
        cwd: PathBuf::from(row.get::<_, String>("cwd")?),
        scheduler_session: row.get("scheduler_session")?,
        state,
        last_gate_reading: row.get("last_gate_reading")?,
        created_at_utc: parse_ts(&id, Some(row.get::<_, String>("created_at_utc")?.as_str()))?
            .ok_or_else(|| ScheduleError::CorruptRow {
                id: id.clone(),
                detail: "created_at_utc is never absent".into(),
            })?,
        last_attempt_utc: parse_ts(
            &id,
            row.get::<_, Option<String>>("last_attempt_utc")?.as_deref(),
        )?,
        id,
    })
}

const SELECT_COLUMNS: &str = "SELECT id, fire_at_utc, trigger_kind, interval_secs, on_command, \
     grace_secs, force, node, argv_json, cwd, scheduler_session, state, last_gate_reading, \
     created_at_utc, last_attempt_utc \
     FROM scheduled_spawns";

/// Queues one row. The id is minted here and returned through `row`.
pub fn insert(home: &AgentsHome, row: &mut ScheduledSpawn) -> Result<(), ScheduleError> {
    let connection = open(home)?;
    let database = database_path(home)?;
    for _ in 0..5 {
        row.id = mint_id();
        // A minted id colliding with a live row is the one error the five
        // tries exist to absorb; any other insert failure is a real store
        // fault. Unique violations return Err, so the retry reads the code,
        // not a row count.
        let outcome = connection.execute(
            "INSERT INTO scheduled_spawns (id, fire_at_utc, trigger_kind, interval_secs, \
                 on_command, grace_secs, force, node, argv_json, cwd, scheduler_session, state, \
                 last_gate_reading, created_at_utc, last_attempt_utc) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                row.id,
                row.fire_at_utc.map(rfc3339),
                trigger_kind(&row.trigger),
                match &row.trigger {
                    Trigger::Every { interval_secs } => Some(*interval_secs),
                    _ => None,
                },
                match &row.trigger {
                    Trigger::On { command } => Some(command.as_str()),
                    _ => None,
                },
                row.grace_secs,
                i64::from(row.force),
                row.node.as_deref(),
                serde_json::to_string(&row.argv).map_err(|e| store_err(&database, e))?,
                row.cwd.display().to_string(),
                row.scheduler_session,
                row.state.as_str(),
                row.last_gate_reading.as_deref(),
                rfc3339(row.created_at_utc),
                row.last_attempt_utc.map(rfc3339),
            ],
        );
        match outcome {
            Ok(1) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) => {}
            Err(e) => return Err(store_err(&database, e)),
        }
    }
    Err(store_err(
        &database,
        "could not mint a unique row id in five tries",
    ))
}

fn trigger_kind(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::At => "at",
        Trigger::Every { .. } => "every",
        Trigger::On { .. } => "on",
    }
}

/// Every row, oldest first. `--list-scheduled` reads through here.
pub fn list(home: &AgentsHome) -> Result<Vec<ScheduledSpawn>, ScheduleError> {
    let Some(connection) = read_only(home)? else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare(&format!("{SELECT_COLUMNS} ORDER BY created_at_utc"))
        .map_err(|e| store_err(&database_path(home)?, e))?;
    let rows = statement
        .query_map([], row_from)
        .map_err(|e| store_err(&database_path(home)?, e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| store_err(&database_path(home)?, e))
}

/// Rows still waiting to fire.
pub fn pending(home: &AgentsHome) -> Result<Vec<ScheduledSpawn>, ScheduleError> {
    let Some(connection) = read_only(home)? else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare(&format!(
            "{SELECT_COLUMNS} WHERE state = 'pending' ORDER BY fire_at_utc, id"
        ))
        .map_err(|e| store_err(&database_path(home)?, e))?;
    let rows = statement
        .query_map([], row_from)
        .map_err(|e| store_err(&database_path(home)?, e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| store_err(&database_path(home)?, e))
}

/// Pending rows whose fire time has passed. `--on` rows never appear here;
/// the arm polls their commands instead.
pub fn due(home: &AgentsHome, now: DateTime<Utc>) -> Result<Vec<ScheduledSpawn>, ScheduleError> {
    Ok(pending(home)?
        .into_iter()
        .filter(|row| row.fire_at_utc.is_some_and(|t| t <= now)))
}

/// Reads one row by id, for `--cancel-scheduled` and the fire arm.
pub fn get(home: &AgentsHome, id: &str) -> Result<ScheduledSpawn, ScheduleError> {
    let Some(connection) = read_only(home)? else {
        // No store yet: no such row, which is what --cancel-scheduled and
        // the fire arm need to hear, not a store error.
        return Err(ScheduleError::UnknownId { id: id.to_string() });
    };
    connection
        .query_row(
            &format!("{SELECT_COLUMNS} WHERE id = ?1"),
            params![id],
            row_from,
        )
        .optional()
        .map_err(|e| store_err(&database_path(home)?, e))?
        .ok_or_else(|| ScheduleError::UnknownId { id: id.to_string() })
}

/// Moves one row to a terminal state, recording the gate reading that
/// produced it.
pub fn set_state(
    home: &AgentsHome,
    id: &str,
    state: RowState,
    reading: Option<&str>,
) -> Result<(), ScheduleError> {
    get(home, id)?;
    let connection = open(home)?;
    let database = database_path(home)?;
    connection
        .execute(
            "UPDATE scheduled_spawns SET state = ?1, last_gate_reading = ?2 WHERE id = ?3",
            params![state.as_str(), reading, id],
        )
        .map_err(|e| store_err(&database, e))?;
    Ok(())
}

/// Records a retry: the gate reading the refused fire saw and the attempt
/// time, leaving the row pending.
pub fn record_attempt(
    home: &AgentsHome,
    id: &str,
    reading: Option<&str>,
    at: DateTime<Utc>,
) -> Result<(), ScheduleError> {
    get(home, id)?;
    let connection = open(home)?;
    let database = database_path(home)?;
    connection
        .execute(
            "UPDATE scheduled_spawns SET last_gate_reading = ?1, last_attempt_utc = ?2 \
             WHERE id = ?3",
            params![reading, rfc3339(at), id],
        )
        .map_err(|e| store_err(&database, e))?;
    Ok(())
}

/// Re-queues an `--every` row: the next fire time, pending again.
pub fn requeue(home: &AgentsHome, id: &str, next_fire: DateTime<Utc>) -> Result<(), ScheduleError> {
    let row = get(home, id)?;
    if !matches!(row.trigger, Trigger::Every { .. }) {
        return Err(store_err(
            &database_path(home)?,
            format!("row {id} is not an --every row; requeue applies to it alone"),
        ));
    }
    let connection = open(home)?;
    let database = database_path(home)?;
    connection
        .execute(
            "UPDATE scheduled_spawns SET fire_at_utc = ?1, state = 'pending' WHERE id = ?2",
            params![rfc3339(next_fire), id],
        )
        .map_err(|e| store_err(&database, e))?;
    Ok(())
}

/// Drops one row: reads `cancelled`, returns the row as it stood.
pub fn cancel(home: &AgentsHome, id: &str) -> Result<ScheduledSpawn, ScheduleError> {
    let row = get(home, id)?;
    set_state(home, id, RowState::Cancelled, None)?;
    Ok(row)
}

// ---------------------------------------------------------------------
// Time parsing and rendering
// ---------------------------------------------------------------------

/// US zone abbreviations map to IANA names; a full IANA name parses
/// directly.
const ZONE_ABBREVIATIONS: &[(&str, &str)] = &[
    ("PT", "America/Los_Angeles"),
    ("PST", "America/Los_Angeles"),
    ("PDT", "America/Los_Angeles"),
    ("ET", "America/New_York"),
    ("EST", "America/New_York"),
    ("EDT", "America/New_York"),
    ("CT", "America/Chicago"),
    ("CST", "America/Chicago"),
    ("CDT", "America/Chicago"),
    ("MT", "America/Denver"),
    ("MST", "America/Denver"),
    ("MDT", "America/Denver"),
];

fn parse_zone(name: &str) -> Result<Tz, WhenParseError> {
    let trimmed = name.trim();
    let upper = trimmed.to_ascii_uppercase();
    if upper == "UTC" || upper == "Z" {
        return Ok(chrono_tz::UTC);
    }
    if let Some((_, iana)) = ZONE_ABBREVIATIONS.iter().find(|(abbr, _)| *abbr == upper) {
        return iana.parse::<Tz>().map_err(|_| WhenParseError::Malformed {
            input: name.to_string(),
            reason: "unknown zone".into(),
        });
    }
    trimmed
        .parse::<Tz>()
        .map_err(|_| WhenParseError::Malformed {
            input: name.to_string(),
            reason: "not an IANA zone name or a known abbreviation (PT, ET, CT, MT, UTC)".into(),
        })
}

fn parse_hhmm(input: &str, whole: &str) -> Result<NaiveTime, WhenParseError> {
    let parts: Vec<&str> = input.split(':').collect();
    if parts.len() != 2 {
        return Err(WhenParseError::Malformed {
            input: whole.to_string(),
            reason: "want HH:MM".into(),
        });
    }
    let (h, m) = (parts[0], parts[1]);
    let hour: u32 = h.parse().map_err(|_| WhenParseError::Malformed {
        input: whole.to_string(),
        reason: "hour is not a number".into(),
    })?;
    let minute: u32 = m.parse().map_err(|_| WhenParseError::Malformed {
        input: whole.to_string(),
        reason: "minute is not a number".into(),
    })?;
    NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(|| WhenParseError::Malformed {
        input: whole.to_string(),
        reason: "hour or minute out of range".into(),
    })
}

/// Resolves one wall-clock time on today's local date in `tz`, refusing the
/// DST-ambiguous and gap cases by naming the UTC instants. An instant at or
/// before `now` rolls to tomorrow (the echo says so).
fn resolve_in_zone(
    whole: &str,
    time: NaiveTime,
    today: chrono::NaiveDate,
    tz: impl TimeZone,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, WhenParseError> {
    let naive = today.and_time(time);
    let resolved = match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(a, b) => {
            return Err(WhenParseError::Ambiguous {
                input: whole.to_string(),
                first: rfc3339(a.with_timezone(&Utc)),
                second: rfc3339(b.with_timezone(&Utc)),
            });
        }
        LocalResult::None => {
            let next = (1..=1440)
                .find_map(|mins| {
                    let candidate = naive + chrono::Duration::minutes(mins);
                    match tz.from_local_datetime(&candidate) {
                        LocalResult::Single(dt) => Some(candidate),
                        _ => None,
                    }
                })
                .unwrap_or_else(|| naive + chrono::Duration::hours(24));
            return Err(WhenParseError::Gap {
                input: whole.to_string(),
                next: next.format("%Y-%m-%d %H:%M").to_string(),
            });
        }
    };
    if resolved <= now {
        Ok(resolved + chrono::Duration::hours(24))
    } else {
        Ok(resolved)
    }
}

/// Parses a `--when` value: RFC 3339, `HH:MMZ` (UTC), `HH:MM <zone>` (IANA
/// name or a US abbreviation), or bare `HH:MM` in the machine zone. A time
/// already past rolls to tomorrow.
pub fn parse_when(
    input: &str,
    now: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> Result<DateTime<Utc>, WhenParseError> {
    let trimmed = input.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Some(stripped) = trimmed
        .strip_suffix('Z')
        .or_else(|| trimmed.strip_suffix('z'))
    {
        let time = parse_hhmm(stripped, trimmed)?;
        return resolve_in_zone(trimmed, time, now_local.date_naive(), chrono_tz::UTC, now);
    }
    if let Some((time_part, zone_part)) = trimmed.split_once(' ') {
        let time = parse_hhmm(time_part, trimmed)?;
        let tz = parse_zone(zone_part)?;
        return resolve_in_zone(trimmed, time, now_local.date_naive(), tz, now);
    }
    let time = parse_hhmm(trimmed, trimmed)?;
    resolve_in_zone(trimmed, time, now_local.date_naive(), Local, now)
}

/// Parses an `--every` interval: `hourly`, `daily`, `weekly`, or a duration
/// like `30m`, `2h`, `7d`.
pub fn parse_interval(raw: &str) -> Result<i64, WhenParseError> {
    let secs = match raw.trim() {
        "hourly" => 3600,
        "daily" => 24 * 3600,
        "weekly" => 7 * 24 * 3600,
        other => crate::duration::parse_duration_secs(other).ok_or_else(|| {
            WhenParseError::Malformed {
                input: raw.to_string(),
                reason: "want hourly, daily, weekly or a duration like 30m, 2h, 7d".into(),
            }
        })? as i64,
    };
    if secs <= 0 {
        return Err(WhenParseError::Malformed {
            input: raw.to_string(),
            reason: "interval must be positive".into(),
        });
    }
    Ok(secs)
}

/// Every echo, list row and mail shows the fire time in the machine zone
/// and UTC, e.g. `2026-11-04 12:00 PST (2026-11-04 20:00 UTC)`.
pub fn render_dual(utc: DateTime<Utc>) -> String {
    format!(
        "{} ({})",
        utc.with_timezone(&Local).format("%Y-%m-%d %H:%M %Z"),
        utc.format("%Y-%m-%d %H:%M UTC")
    )
}

/// `Some("tomorrow")` when a bare-time roll landed a fire on tomorrow's
/// local date; the echo says so.
pub fn tomorrow_note(fire: DateTime<Utc>, now_local: DateTime<Local>) -> Option<&'static str> {
    let fire_local = fire.with_timezone(&Local);
    let today = now_local.date_naive();
    let tomorrow = today.succ_opt()?;
    (fire_local.date_naive() == tomorrow).then_some("tomorrow")
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn now_utc() -> DateTime<Utc> {
        // One hour before the sample instants below, so a same-day 20:00Z
        // candidate stays in the future on every machine zone.
        Utc.with_ymd_and_hms(2026, 11, 4, 19, 0, 0).unwrap()
    }

    fn now_local_fixed() -> DateTime<Local> {
        // A fixed-offset local so the bare-time tests do not depend on the
        // machine zone: UTC+1, 2026-11-04 21:00 local.
        Local.with_ymd_and_hms(2026, 11, 4, 21, 0, 0).unwrap()
    }

    fn parse(input: &str) -> Result<DateTime<Utc>, WhenParseError> {
        parse_when(input, now_utc(), now_local_fixed())
    }

    #[test]
    fn rfc3339_parses_to_utc() {
        let t = parse("2026-11-05T06:00:00Z").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 5, 6, 0, 0).unwrap());
    }

    #[test]
    fn utc_suffix_parses_and_rolls_past_times_to_tomorrow() {
        // 21:00Z is one hour in the future: stays today.
        let t = parse("21:00Z").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 4, 21, 0, 0).unwrap());
        // 20:00Z is now: rolls to tomorrow.
        let t = parse("20:00Z").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 5, 20, 0, 0).unwrap());
    }

    #[test]
    fn iana_zone_resolves_through_its_offset() {
        // 12:00 PT on 2026-11-04 is 20:00 UTC (PST, UTC-8).
        let t = parse("12:00 America/Los_Angeles").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 4, 20, 0, 0).unwrap());
    }

    #[test]
    fn abbreviation_maps_to_iana() {
        let t = parse("12:00 PT").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 4, 20, 0, 0).unwrap());
    }

    #[test]
    fn bare_time_resolves_in_the_machine_zone() {
        // The machine zone decides the instant, so the test pins the shape,
        // not the absolute time: a bare time lands at that wall clock on the
        // local date, today or rolled to tomorrow when already past.
        let t = parse("12:00").unwrap();
        let local = t.with_timezone(&Local);
        assert_eq!(local.format("%H:%M").to_string(), "12:00");
        let today_or_tomorrow = now_local_fixed().date_naive();
        assert!(
            local.date_naive() == today_or_tomorrow
                || local.date_naive() == today_or_tomorrow.succ_opt().unwrap(),
            "bare time landed on {local}, not today or tomorrow"
        );
    }

    #[test]
    fn fall_back_ambiguity_refuses_naming_both_instants() {
        // America/New_York fell back on 2026-11-01 at 02:00 local; 01:30
        // happens twice. The plan pins 2026-11-04, so resolve on the real
        // fall-back date through a custom now.
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 5, 0, 0).unwrap();
        let local = Local.with_ymd_and_hms(2026, 11, 1, 1, 0, 0).unwrap();
        let err = parse_when("01:30 America/New_York", now, local).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("ambiguous"), "got: {text}");
        assert!(
            text.contains("05:30") && text.contains("06:30"),
            "got: {text}"
        );
    }

    #[test]
    fn dst_gap_refuses_naming_the_next_valid_time() {
        // America/New_York sprang forward 2027-03-14 at 02:00 local; 02:30
        // does not exist. Pin the date through the local argument.
        let now = Utc.with_ymd_and_hms(2027, 3, 14, 6, 0, 0).unwrap();
        let local = Local.with_ymd_and_hms(2027, 3, 14, 2, 0, 0).unwrap();
        let err = parse_when("02:30 America/New_York", now, local).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("DST gap"), "got: {text}");
        assert!(text.contains("03:"), "got: {text}");
    }

    #[test]
    fn malformed_time_refuses() {
        assert!(parse("25:00").is_err());
        assert!(parse("abc").is_err());
        assert!(parse("12:00 Nowhere/None").is_err());
    }

    #[test]
    fn intervals_accept_words_and_durations_and_refuse_junk() {
        assert_eq!(parse_interval("weekly").unwrap(), 7 * 24 * 3600);
        assert_eq!(parse_interval("daily").unwrap(), 24 * 3600);
        assert_eq!(parse_interval("2h").unwrap(), 7200);
        assert!(parse_interval("fortnight").is_err());
        assert!(parse_interval("0m").is_err());
    }

    #[test]
    fn tomorrow_note_fires_only_on_the_next_local_date() {
        let now_local = now_local_fixed();
        let fire_tomorrow = Utc.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap();
        let fire_later = Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap();
        assert_eq!(tomorrow_note(fire_tomorrow, now_local), Some("tomorrow"));
        assert_eq!(tomorrow_note(fire_later, now_local), None);
    }

    #[test]
    fn store_round_trips_a_row_in_a_sandbox_home() {
        let home = AgentsHome::at(
            std::env::temp_dir().join(format!("x5b38-sched-test-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(home.root());
        std::fs::create_dir_all(home.root()).unwrap();
        let mut row = ScheduledSpawn {
            id: String::new(),
            fire_at_utc: Some(now_utc() + chrono::Duration::hours(2)),
            trigger: Trigger::At,
            grace_secs: DEFAULT_GRACE_SECS,
            force: true,
            node: Some("x-1d77".into()),
            argv: vec![
                "w1".into(),
                "/fno:target x-1d77".into(),
                "--at".into(),
                "3".into(),
            ],
            cwd: PathBuf::from("/tmp/proj"),
            scheduler_session: "t-scheduler".into(),
            state: RowState::Pending,
            last_gate_reading: None,
            created_at_utc: now_utc(),
            last_attempt_utc: None,
        };
        insert(&home, &mut row).unwrap();
        assert!(row.id.starts_with("ss-"));

        let listed = list(&home).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], row);

        let due = due(&home, now_utc()).unwrap();
        assert!(due.is_empty());
        let due = due(&home, now_utc() + chrono::Duration::hours(3)).unwrap();
        assert_eq!(due.len(), 1);

        record_attempt(&home, &row.id, Some("seat cap full"), now_utc()).unwrap();
        let after = get(&home, &row.id).unwrap();
        assert_eq!(after.last_gate_reading.as_deref(), Some("seat cap full"));
        assert_eq!(after.state, RowState::Pending);

        requeue(&home, &row.id, now_utc() + chrono::Duration::hours(5)).unwrap();
        let requeued = get(&home, &row.id).unwrap();
        assert_eq!(requeued.state, RowState::Pending);
        // requeue refuses a non-every row
        assert!(requeue(&home, &row.id, now_utc()).is_err());

        let cancelled = cancel(&home, &row.id).unwrap();
        assert_eq!(cancelled.state, RowState::Pending);
        assert_eq!(get(&home, &row.id).unwrap().state, RowState::Cancelled);
        let _ = std::fs::remove_dir_all(home.root());
    }

    #[test]
    fn cancel_unknown_id_names_the_list_command() {
        let home = AgentsHome::at(
            std::env::temp_dir().join(format!("x5b38-sched-miss-{}", std::process::id())),
        );
        std::fs::create_dir_all(home.root()).unwrap();
        let err = cancel(&home, "ss-absent").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("ss-absent"), "got: {text}");
        assert!(text.contains("--list-scheduled"), "got: {text}");
        let _ = std::fs::remove_dir_all(home.root());
    }
}
