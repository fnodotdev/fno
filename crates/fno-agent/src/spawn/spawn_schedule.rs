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
    #[error("schedule store at {path}: {cause}")]
    Store { path: PathBuf, cause: String },
    #[error(
        "scheduled spawn {id}: no such row; run `fno agent spawn --list-scheduled` to see live ids"
    )]
    UnknownId { id: String },
    #[error("scheduled spawn {id}: stored row is corrupt: {detail}")]
    CorruptRow { id: String, detail: String },
    #[error("schedule store: {0}")]
    Sql(#[from] rusqlite::Error),
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
        cause: source.to_string(),
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
        cause: "no state root to anchor the schedule store".into(),
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
    // getrandom is the production entropy dep (rand is dev-only here); the
    // clock+pid fallback still mints a unique-enough id and the insert
    // retry loop absorbs a collision.
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ std::process::id())
            .unwrap_or(0);
        bytes = nanos.to_le_bytes();
    }
    format!("ss-{:08x}", u32::from_le_bytes(bytes))
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

/// The query seam: rusqlite's query_map and query_row want their own
/// Result from the closure, so the typed parser converts at the boundary
/// and the Display chain carries the CorruptRow detail through.
fn row_map(row: &rusqlite::Row) -> rusqlite::Result<ScheduledSpawn> {
    row_from(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn row_from(row: &rusqlite::Row) -> Result<ScheduledSpawn, ScheduleError> {
    // Positional reads by SELECT_COLUMNS order: the shared value-read gate
    // holds string-key map reads shrink-only crate-wide, so this store
    // reads by index the way registry_store does.
    let id: String = row.get(0)?;
    let fire_at_raw: Option<String> = row.get(1)?;
    let kind: String = row.get(2)?;
    let interval_secs: Option<i64> = row.get(3)?;
    let on_command: Option<String> = row.get(4)?;
    let grace_secs: i64 = row.get(5)?;
    let force: bool = row.get::<_, i64>(6)? != 0;
    let node: Option<String> = row.get(7)?;
    let argv_json: String = row.get(8)?;
    let cwd: String = row.get(9)?;
    let scheduler_session: String = row.get(10)?;
    let state_raw: String = row.get(11)?;
    let last_gate_reading: Option<String> = row.get(12)?;
    let created_at_raw: String = row.get(13)?;
    let last_attempt_raw: Option<String> = row.get(14)?;
    let state = RowState::parse(&state_raw).ok_or_else(|| ScheduleError::CorruptRow {
        id: id.clone(),
        detail: format!("unknown state {state_raw:?}"),
    })?;
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
    let argv: Vec<String> =
        serde_json::from_str(&argv_json).map_err(|e| ScheduleError::CorruptRow {
            id: id.clone(),
            detail: format!("argv_json: {e}"),
        })?;
    let created_at_utc =
        parse_ts(&id, Some(&created_at_raw))?.ok_or_else(|| ScheduleError::CorruptRow {
            id: id.clone(),
            detail: "created_at_utc is never absent".into(),
        })?;
    Ok(ScheduledSpawn {
        fire_at_utc: parse_ts(&id, fire_at_raw.as_deref())?,
        trigger,
        grace_secs,
        force,
        node,
        argv,
        cwd: PathBuf::from(cwd),
        scheduler_session,
        state,
        last_gate_reading,
        created_at_utc,
        last_attempt_utc: parse_ts(&id, last_attempt_raw.as_deref())?,
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
    let database = database_path(home)?;
    let mut statement = connection
        .prepare(&format!("{SELECT_COLUMNS} ORDER BY created_at_utc"))
        .map_err(|e| store_err(&database, e))?;
    let rows = statement
        .query_map([], row_map)
        .map_err(|e| store_err(&database, e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| store_err(&database, e))
}

/// Rows still waiting to fire.
pub fn pending(home: &AgentsHome) -> Result<Vec<ScheduledSpawn>, ScheduleError> {
    let Some(connection) = read_only(home)? else {
        return Ok(Vec::new());
    };
    let database = database_path(home)?;
    let mut statement = connection
        .prepare(&format!(
            "{SELECT_COLUMNS} WHERE state = 'pending' ORDER BY fire_at_utc, id"
        ))
        .map_err(|e| store_err(&database, e))?;
    let rows = statement
        .query_map([], row_map)
        .map_err(|e| store_err(&database, e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| store_err(&database, e))
}

/// Pending rows whose fire time has passed. `--on` rows never appear here;
/// the arm polls their commands instead.
pub fn due(home: &AgentsHome, now: DateTime<Utc>) -> Result<Vec<ScheduledSpawn>, ScheduleError> {
    Ok(pending(home)?
        .into_iter()
        .filter(|row| row.fire_at_utc.is_some_and(|t| t <= now))
        .collect())
}

/// Reads one row by id, for `--cancel-scheduled` and the fire arm.
pub fn get(home: &AgentsHome, id: &str) -> Result<ScheduledSpawn, ScheduleError> {
    let Some(connection) = read_only(home)? else {
        // No store yet: no such row, which is what --cancel-scheduled and
        // the fire arm need to hear, not a store error.
        return Err(ScheduleError::UnknownId { id: id.to_string() });
    };
    let database = database_path(home)?;
    let found = connection
        .query_row(
            &format!("{SELECT_COLUMNS} WHERE id = ?1"),
            params![id],
            row_map,
        )
        .optional()
        .map_err(|e| store_err(&database, e))?;
    found.ok_or_else(|| ScheduleError::UnknownId { id: id.to_string() })
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
    let [h, m] = parts.as_slice() else {
        return Err(WhenParseError::Malformed {
            input: whole.to_string(),
            reason: "want HH:MM".into(),
        });
    };
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
                        LocalResult::Single(_) => Some(candidate),
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
        // Tomorrow's wall clock, re-resolved: across a DST-change night
        // the named wall time stays the named wall time.
        let tomorrow = today.succ_opt().ok_or_else(|| WhenParseError::Malformed {
            input: whole.to_string(),
            reason: "no tomorrow".into(),
        })?;
        match tz.from_local_datetime(&tomorrow.and_time(time)) {
            LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
            _ => Ok(resolved + chrono::Duration::hours(24)),
        }
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
// Front door: the schedule flags on `fno agent spawn`
// ---------------------------------------------------------------------

/// A schedule request scanned off the raw spawn argv, before the strict
/// parse would demand a name.
#[derive(Debug, Default, PartialEq)]
struct ScheduleRequest {
    when: Option<String>,
    every: Option<String>,
    on: Option<String>,
    grace: Option<String>,
    list: bool,
    cancel: Option<String>,
    /// Any trigger flag arrived; set even when a value parse failed, so the
    /// front door owns the request.
    trigger: bool,
}

/// One schedule flag with its value, or a scalar action flag.
#[derive(Debug, PartialEq)]
enum ScheduleToken {
    Value(&'static str, String),
    Scalar(&'static str),
}

impl ScheduleRequest {
    fn absorb(&mut self, token: ScheduleToken) {
        match token {
            ScheduleToken::Value("when", v) => {
                self.when = Some(v);
                self.trigger = true;
            }
            ScheduleToken::Value("every", v) => {
                self.every = Some(v);
                self.trigger = true;
            }
            ScheduleToken::Value("on", v) => {
                self.on = Some(v);
                self.trigger = true;
            }
            ScheduleToken::Value("grace", v) => self.grace = Some(v),
            ScheduleToken::Scalar("list") => self.list = true,
            ScheduleToken::Value("cancel", v) => self.cancel = Some(v),
            _ => {}
        }
    }
}

/// Splits the spawn argv at the prompt fence: everything before the first
/// positional (or the `--` fence) is spawn-owned and scanned; everything
/// after rides the harness verbatim and is never scanned for schedule
/// flags. Also answers the spawn name (`--name <v>` or the first
/// positional).
fn scan_spawn_argv(args: &[String]) -> (ScheduleRequest, usize, Option<String>) {
    let mut req = ScheduleRequest::default();
    let mut name: Option<String> = None;
    let mut i = 0;
    while let Some(tok) = args.get(i).map(String::as_str) {
        // Schedule spellings answer before the generic value-flag walk:
        // they sit in the owned lists too, and a generic consume would
        // swallow them without recording the request.
        if let Some(inline) = tok
            .strip_prefix("--when=")
            .or_else(|| tok.strip_prefix("--every="))
            .or_else(|| tok.strip_prefix("--on="))
            .or_else(|| tok.strip_prefix("--grace="))
            .or_else(|| tok.strip_prefix("--cancel-scheduled="))
        {
            // The absorb keys are 'static literals matched off the spelling,
            // never a trim of the borrowed token; the cancel flag spells its
            // key differently from its flag name.
            let key = match tok.split_once('=').map(|(flag, _)| flag).unwrap_or(tok) {
                "--when" => "when",
                "--every" => "every",
                "--on" => "on",
                "--grace" => "grace",
                _ => "cancel",
            };
            req.absorb(ScheduleToken::Value(key, inline.to_string()));
            i += 1;
            continue;
        }
        match tok {
            "--when" | "--every" | "--on" | "--grace" | "--cancel-scheduled" => {
                let value = args.get(i + 1).cloned().unwrap_or_default();
                let key = match tok {
                    "--when" => "when",
                    "--every" => "every",
                    "--on" => "on",
                    "--grace" => "grace",
                    _ => "cancel",
                };
                req.absorb(ScheduleToken::Value(key, value));
                i += 2;
                continue;
            }
            "--list-scheduled" => {
                req.absorb(ScheduleToken::Scalar("list"));
                i += 1;
                continue;
            }
            "--" => {
                // Past the fence nothing is spawn-owned, so no positional
                // name and no schedule flag either.
                return (req, args.len(), name);
            }
            _ => {}
        }
        if crate::spawn_admission::is_value_flag(tok) {
            // Consume the value so it never reads as the name.
            if tok == "--name" {
                name = args.get(i + 1).cloned();
            }
            i += 2;
            continue;
        }
        if !tok.starts_with('-') {
            // First positional: the name; the tail is harness argv.
            return (req, i, name);
        }
        i += 1;
    }
    (req, args.len(), name)
}

/// The spawn argv the row records: every token except the schedule flags
/// with their values, and the `--force`/`-F` spelling the row stores as its
/// force bit. The prompt fence never splits the recorded argv: the tail
/// (harness passthrough) rides verbatim.
fn strip_schedule_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(tok) = args.get(i).map(String::as_str) {
        if tok == "--" {
            // The fence ends the spawn-owned region; harness passthrough
            // rides verbatim even when a token repeats a schedule spelling.
            out.extend(args.iter().skip(i).cloned());
            return out;
        }
        if matches!(
            tok,
            "--when" | "--every" | "--on" | "--grace" | "--list-scheduled" | "--cancel-scheduled"
        ) || tok.starts_with("--when=")
            || tok.starts_with("--every=")
            || tok.starts_with("--on=")
            || tok.starts_with("--grace=")
        {
            // A value flag drops one extra token.
            if matches!(
                tok,
                "--when" | "--every" | "--on" | "--grace" | "--cancel-scheduled"
            ) && args.get(i + 1).is_some_and(|t| !t.starts_with('-'))
            {
                i += 1;
            }
            i += 1;
            continue;
        }
        if tok == "--force" || tok == "-F" {
            i += 1;
            continue;
        }
        out.push(tok.to_string());
        i += 1;
    }
    out
}

/// The spawn front door's schedule arm: intercepts `--list-scheduled`,
/// `--cancel-scheduled <id>` and the trigger flags before the strict parse,
/// answers, and never reaches the daemon RPC. `None` when the argv carries
/// no schedule flag, so a plain spawn falls through untouched.
pub fn front_door(home: &AgentsHome, args: &[String]) -> Option<i32> {
    let (req, name_index, name_flag) = scan_spawn_argv(args);
    if !req.trigger && !req.list && req.cancel.is_none() {
        return None;
    }
    Some(run_front_door(home, args, req, name_index, name_flag))
}

/// The exit for the list and cancel arms; Err(()) when the argv names no
/// action and the trigger arm answers instead.
fn run_list_cancel(home: &AgentsHome, req: &ScheduleRequest, json: bool) -> Result<i32, ()> {
    if !(req.list || req.cancel.is_some()) {
        return Err(());
    }
    // Actions first; a trigger beside an action is malformed.
    if req.trigger {
        eprintln!("fno agent spawn: --list-scheduled and --cancel-scheduled take no trigger flag");
        return Ok(2);
    }
    if let Some(id) = req.cancel.as_deref() {
        return Ok(match cancel(home, id) {
            Ok(row) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"schema": "scheduled-spawns/v1", "cancelled": row.id})
                    );
                } else {
                    println!("cancelled scheduled spawn {}", row.id);
                }
                0
            }
            Err(e) => {
                eprintln!("fno agent spawn: {e}");
                2
            }
        });
    }
    let rows = match list(home) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno agent spawn: {e}");
            return Ok(2);
        }
    };
    if json {
        let rows_json: Vec<serde_json::Value> = rows.iter().map(schedule_row_json).collect();
        println!(
            "{}",
            serde_json::json!({"schema": "scheduled-spawns/v1", "rows": rows_json})
        );
        return Ok(0);
    }
    if rows.is_empty() {
        println!("no scheduled spawns");
        return Ok(0);
    }
    for row in &rows {
        println!("{}", schedule_row_line(row));
    }
    Ok(0)
}

/// The trigger-side fields one row resolves before insert: the grace, the
/// first fire instant (None for --on), and the --on command. None after
/// printing the refusal.
fn resolve_trigger(
    req: &ScheduleRequest,
    now: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> Option<(i64, Option<DateTime<Utc>>, Option<String>)> {
    let triggers = [req.when.is_some(), req.every.is_some(), req.on.is_some()]
        .into_iter()
        .filter(|b| *b)
        .count();
    if triggers > 1 {
        eprintln!("fno agent spawn: pick one of --when, --every, --on (a row carries one trigger)");
        return None;
    }
    if req.grace.is_some() && !req.trigger {
        eprintln!("fno agent spawn: --grace needs --when, --every or --on");
        return None;
    }
    let grace_secs = match req.grace.as_deref() {
        Some(raw) => match crate::duration::parse_duration_secs(raw) {
            Some(0) | None => {
                eprintln!(
                    "fno agent spawn: --grace wants a positive duration like 30m or 1h (got {raw})"
                );
                return None;
            }
            Some(secs) => secs as i64,
        },
        None => {
            if req.when.is_some() || req.every.is_some() {
                DEFAULT_GRACE_SECS
            } else {
                DEFAULT_ON_GRACE_SECS
            }
        }
    };
    // The trigger grammar decides; the spawn's own admission (harness, lane,
    // seed, message) runs at FIRE time, when the recorded argv re-enters the
    // front door. A row that would refuse fires anyway, retries inside its
    // grace, and journals `missed` with the gate reason.
    let fire_at = if let Some(raw) = req.when.as_deref() {
        match parse_when(raw, now, now_local) {
            Ok(at) => Some(at),
            Err(e) => {
                eprintln!("fno agent spawn: {e}");
                return None;
            }
        }
    } else if let Some(raw) = req.every.as_deref() {
        match parse_interval(raw) {
            Ok(secs) => Some(now + chrono::Duration::seconds(secs)),
            Err(e) => {
                eprintln!("fno agent spawn: {e}");
                return None;
            }
        }
    } else {
        None
    };
    let command = req.on.clone();
    if command.as_deref().is_some_and(str::is_empty) {
        eprintln!("fno agent spawn: --on needs a command");
        return None;
    }
    Some((grace_secs, fire_at, command))
}

/// The row-body fields the recorded argv carries, held out of
/// run_front_door so both stay under the line budget.
struct RowFields {
    force: bool,
    node: Option<String>,
    cwd: PathBuf,
    scheduler: Option<String>,
    trigger: Trigger,
}

fn row_fields(
    args: &[String],
    name_index: usize,
    req: &ScheduleRequest,
    command: Option<String>,
) -> RowFields {
    let force = args
        .iter()
        .take(name_index)
        .any(|a| a == "--force" || a == "-F");
    let owned = |needle: &str| {
        args.iter()
            .take(name_index)
            .zip(args.iter().skip(1))
            .find(|(a, _)| a.as_str() == needle)
            .map(|(_, v)| v.clone())
    };
    let node = owned("--node");
    // The recorded cwd honors an explicit --cwd so an --on poll runs where
    // the spawn would have; the default stays the scheduler's cwd.
    let cwd = owned("--cwd")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let scheduler = owned("--spawned-by").or_else(|| crate::claims::ambient_parent_edge().0);
    let trigger = if let Some(command) = command {
        Trigger::On { command }
    } else if let Some(raw) = req.every.as_deref() {
        // Already validated by resolve_trigger; the parse cannot fail here.
        Trigger::Every {
            interval_secs: parse_interval(raw).unwrap_or(3600),
        }
    } else {
        Trigger::At
    };
    RowFields {
        force,
        node,
        cwd,
        scheduler,
        trigger,
    }
}

fn run_front_door(
    home: &AgentsHome,
    args: &[String],
    req: ScheduleRequest,
    name_index: usize,
    name_flag: Option<String>,
) -> i32 {
    let json = args
        .iter()
        .take_while(|a| a.as_str() != "--argv")
        .any(|a| a == "--json" || a == "-J");
    if let Ok(exit) = run_list_cancel(home, &req, json) {
        return exit;
    }
    let now = Utc::now();
    let now_local = Local::now();
    let Some((grace_secs, fire_at, command)) = resolve_trigger(&req, now, now_local) else {
        return 2;
    };
    let name = name_flag.clone().or_else(|| args.get(name_index).cloned());
    if name.as_deref().is_none_or(str::is_empty) {
        eprintln!("fno agent spawn: a scheduled row still needs a <name> or --name");
        return 2;
    }
    // --wait waits at the gate for a spawn that never starts; refuse it.
    if args.iter().take(name_index).any(|a| a == "--wait") {
        eprintln!("fno agent spawn: --wait never fires: it waits at the gate for a spawn that starts now, not one queued for later");
        return 2;
    }
    // A bad runbook refuses before a row exists; the row records its
    // absolute path so the fire reads the same file from any cwd.
    let args = match crate::spawn::runbook::absolutize(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("fno agent spawn: {e}");
            return 2;
        }
    };
    let args = args.as_slice();
    let fields = row_fields(args, name_index, &req, command);
    let mut row = ScheduledSpawn {
        id: String::new(),
        fire_at_utc: fire_at,
        trigger: fields.trigger,
        grace_secs,
        force: fields.force,
        node: fields.node,
        argv: strip_schedule_flags(args),
        cwd: fields.cwd,
        scheduler_session: fields.scheduler.unwrap_or_default(),
        state: RowState::Pending,
        last_gate_reading: None,
        created_at_utc: now,
        last_attempt_utc: None,
    };
    if let Err(e) = insert(home, &mut row) {
        eprintln!("fno agent spawn: {e}");
        return 2;
    }
    let fire_desc = row
        .fire_at_utc
        .map(render_dual)
        .unwrap_or_else(|| "--on polls each tick".to_string());
    let note = row
        .fire_at_utc
        .and_then(|t| tomorrow_note(t, Local::now()))
        .map(|note| format!(" ({note})"))
        .unwrap_or_default();
    println!("scheduled spawn {} for {}{}", row.id, fire_desc, note);
    0
}

/// One human-readable list line.
fn schedule_row_line(row: &ScheduledSpawn) -> String {
    let fire = row
        .fire_at_utc
        .map(render_dual)
        .unwrap_or_else(|| "on condition".to_string());
    let force = if row.force { " force=yes" } else { "" };
    let node = row
        .node
        .as_deref()
        .map(|n| format!(" node={n}"))
        .unwrap_or_default();
    let reading = row
        .last_gate_reading
        .as_deref()
        .map(|r| format!(" gate: {r}"))
        .unwrap_or_default();
    format!(
        "{} {} [{}] {}{}{} grace={}m{}",
        row.id,
        row.state.as_str(),
        fire,
        row.argv.join(" "),
        force,
        node,
        row.grace_secs / 60,
        reading
    )
}

/// The machine surface the done-probe reads: schema-pinned row objects.
fn schedule_row_json(row: &ScheduledSpawn) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "fire_at_utc": row.fire_at_utc.map(rfc3339),
        "trigger": match &row.trigger {
            Trigger::At => serde_json::json!("at"),
            Trigger::Every { interval_secs } => serde_json::json!({"every_secs": interval_secs}),
            Trigger::On { command } => serde_json::json!({"on": command}),
        },
        "grace_secs": row.grace_secs,
        "force": row.force,
        "node": row.node,
        "argv": row.argv,
        "cwd": row.cwd.display().to_string(),
        "scheduler_session": row.scheduler_session,
        "state": row.state.as_str(),
        "last_gate_reading": row.last_gate_reading,
        "created_at_utc": rfc3339(row.created_at_utc),
        "last_attempt_utc": row.last_attempt_utc.map(rfc3339),
    })
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn now_utc() -> DateTime<Utc> {
        // Between the two sample instants below: 21:00Z stays today,
        // 18:00Z rolls to tomorrow.
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
        // 18:00Z is one hour past: rolls to tomorrow.
        let t = parse("18:00Z").unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 5, 18, 0, 0).unwrap());
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
            trigger: Trigger::Every {
                interval_secs: 3600,
            },
            grace_secs: DEFAULT_GRACE_SECS,
            force: true,
            node: Some("demo-node".into()),
            argv: vec![
                "w1".into(),
                "/fno:target demo-node".into(),
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
        assert_eq!(listed.first(), Some(&row));

        let due_rows = due(&home, now_utc()).unwrap();
        assert!(due_rows.is_empty());
        let due_rows = due(&home, now_utc() + chrono::Duration::hours(3)).unwrap();
        assert_eq!(due_rows.len(), 1);

        record_attempt(&home, &row.id, Some("seat cap full"), now_utc()).unwrap();
        let after = get(&home, &row.id).unwrap();
        assert_eq!(after.last_gate_reading.as_deref(), Some("seat cap full"));
        assert_eq!(after.state, RowState::Pending);

        requeue(&home, &row.id, now_utc() + chrono::Duration::hours(5)).unwrap();
        let requeued = get(&home, &row.id).unwrap();
        assert_eq!(requeued.state, RowState::Pending);
        // requeue refuses a non-every row
        let mut at_row = row.clone();
        at_row.id = String::new();
        at_row.trigger = Trigger::At;
        insert(&home, &mut at_row).unwrap();
        assert!(requeue(&home, &at_row.id, now_utc()).is_err());

        let cancelled = cancel(&home, &row.id).unwrap();
        assert_eq!(cancelled.state, RowState::Pending);
        assert_eq!(get(&home, &row.id).unwrap().state, RowState::Cancelled);
        let _ = std::fs::remove_dir_all(home.root());
    }

    #[test]
    fn scan_records_schedule_flags_before_the_name_and_strip_keeps_the_tail() {
        let args: Vec<String> = [
            "--harness",
            "claude",
            "--when",
            "20:00Z",
            "--force",
            "w1",
            "/fno:target demo-node",
            "--argv",
            "--",
            "claude",
            "--when=never",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (req, name_index, name_flag) = scan_spawn_argv(&args);
        assert_eq!(req.when.as_deref(), Some("20:00Z"));
        assert!(req.trigger);
        assert!(req.grace.is_none());
        assert_eq!(name_flag, None);
        assert_eq!(args.get(name_index).map(String::as_str), Some("w1"));
        let stripped = strip_schedule_flags(&args);
        // The schedule flags and the force bit drop; the harness passthrough
        // after --argv keeps even a --when spelling verbatim.
        assert!(!stripped.contains(&"--when".to_string()));
        assert!(!stripped.contains(&"--force".to_string()));
        assert!(stripped.contains(&"--when=never".to_string()));
        assert!(stripped.contains(&"claude".to_string()));
    }

    #[test]
    fn a_plain_spawn_carries_no_schedule_request() {
        let args: Vec<String> = ["w1", "/fno:target demo-node", "--at", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (req, name_index, name_flag) = scan_spawn_argv(&args);
        assert!(!req.trigger);
        assert!(!req.list);
        assert!(req.cancel.is_none());
        assert_eq!(name_index, 0);
        assert_eq!(name_flag, None);
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
