//! Reserve mode: the user needs the machine for a while (a live stream, a
//! video edit), so the fleet backs off at once and comes back on its own.
//!
//! A reserve is a named preset on the one machine-wide fleet incident
//! record, never a state file of its own: `stream` holds spawns and tests,
//! `edit` holds tests. The record carries the expiry, so the hold lifts
//! when the time box ends, and every door already reads it.
//!
//! `fno agents incident reserve stream|edit [--for 2h]` arms it by hand.
//! The daemon tick arms it too while an app on `agents.reserve_apps` runs
//! (OBS, Final Cut Pro, Premiere Pro and DaVinci Resolve by default), renews
//! it while the app stays open, and lifts it when the app quits. A reserve never replaces a stop someone armed, the
//! daemon's reserve never replaces one the user named, and a user who
//! clears the daemon's reserve keeps it cleared until that app restarts.

use std::path::Path;

use crate::census::ProcRow;
use crate::fleet_incident::{self, RecordMetadata, Verdict};
use crate::paths::AgentsHome;

/// `origin` of a reserve the user named.
pub const RESERVE_ORIGIN: &str = "reserve";
/// `origin` of a reserve the daemon armed for a running app.
pub const RESERVE_AUTO_ORIGIN: &str = "reserve_auto";

pub(crate) const DEFAULT_FOR_MS: u64 = 2 * 3_600_000;
/// The daemon's reserve outlives two ticks, so one missed tick never drops
/// it, and lifts within this window if the daemon itself stops.
const AUTO_TTL_MS: u64 = 30 * 60_000;
/// The daemon renews its reserve once less than this is left.
const AUTO_RENEW_BELOW_MS: i64 = 15 * 60_000;

/// The scopes a preset holds, in [`fleet_incident::SCOPES`] order.
pub fn preset_holds(preset: &str) -> Option<Vec<String>> {
    let holds: &[&str] = match preset {
        "stream" => &["spawns", "tests"],
        "edit" => &["tests"],
        _ => return None,
    };
    Some(holds.iter().map(|s| s.to_string()).collect())
}

/// Whether a writer with `origin` may replace the armed stop `current`:
/// a reserve replaces the machine arm's tests hold and an older reserve;
/// the daemon's reserve also yields to one the user named.
pub(crate) fn may_replace(origin: &str, current: Option<&str>) -> bool {
    match (origin, current) {
        (_, Some(fleet_incident::MACHINE_ORIGIN)) => true,
        (_, Some(RESERVE_AUTO_ORIGIN)) => true,
        (RESERVE_ORIGIN, Some(RESERVE_ORIGIN)) => true,
        _ => false,
    }
}

/// An app that reserves the machine while it runs: the name a person reads
/// (matched against its bundle and executable names) and the preset it arms.
struct App {
    name: String,
    preset: &'static str,
}

/// The apps watched when `agents.reserve_apps` is unset. An entry is an app
/// name; `Name=stream` arms the stream preset, any other entry arms edit.
pub const DEFAULT_APPS: &[&str] = &[
    "OBS=stream",
    "Final Cut Pro",
    "Premiere Pro",
    "DaVinci Resolve",
];

fn parse_apps<S: AsRef<str>>(entries: &[S]) -> Vec<App> {
    entries
        .iter()
        .filter_map(|entry| {
            let (name, preset) = match entry.as_ref().split_once('=') {
                Some((name, "stream")) => (name, "stream"),
                Some((name, _)) => (name, "edit"),
                None => (entry.as_ref(), "edit"),
            };
            let name = name.trim();
            (!name.is_empty()).then(|| App {
                name: name.to_string(),
                preset,
            })
        })
        .collect()
}

/// The watched apps for `cwd`: the configured list, else [`DEFAULT_APPS`].
fn configured_apps(cwd: &Path) -> Vec<App> {
    let list = crate::agents_config::config_lookup(cwd, &["agents", "reserve_apps"])
        .and_then(|v| v.as_array().cloned());
    match list {
        Some(items) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            parse_apps(&names)
        }
        None => parse_apps(DEFAULT_APPS),
    }
}

/// `needle` in `hay`, ignoring case, with no letter or digit beside it: `obs`
/// is in `OBS` and `OBS Studio` and not in `Jobs`.
fn has_words(hay: &str, needle: &str) -> bool {
    let hay = hay.to_lowercase();
    let needle = needle.to_lowercase();
    hay.match_indices(&needle).any(|(at, _)| {
        let before = hay[..at].chars().next_back();
        let after = hay[at + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// The app whose executable `command` runs. `command` must start with an
/// absolute bundle path before any flag, so a session whose prompt names a
/// bundle path never reads as the app.
fn app_of<'a>(command: &str, apps: &'a [App]) -> Option<&'a App> {
    const MARK: &str = ".app/Contents/MacOS/";
    if !command.starts_with('/') {
        return None;
    }
    let at = command.find(MARK)?;
    let head = &command[..at];
    if head.contains(" -") {
        return None;
    }
    let bundle = head.rsplit('/').next().unwrap_or_default();
    let exec = command[at + MARK.len()..]
        .split(" -")
        .next()
        .unwrap_or_default();
    apps.iter()
        .find(|app| has_words(bundle, &app.name) || has_words(exec, &app.name))
}

/// The running app that sets the reserve and when its oldest process
/// started, epoch seconds. A stream app outranks an editor.
fn detect<'a>(procs: &[ProcRow], now_epoch: i64, apps: &'a [App]) -> Option<(&'a App, i64)> {
    let mut best: Option<(&'a App, i64)> = None;
    for row in procs {
        let Some(app) = app_of(&row.command, apps) else {
            continue;
        };
        let started = now_epoch - row.elapsed_s as i64;
        best = match best {
            Some((held, at)) if held.preset == "stream" && app.preset != "stream" => {
                Some((held, at))
            }
            Some((held, at)) if held.preset == app.preset => Some((held, at.min(started))),
            _ => Some((app, started)),
        };
    }
    best
}

fn epoch_of(rfc3339: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|t| t.timestamp())
}

/// One daemon tick: arm, renew or lift the daemon's reserve from the apps
/// running now. `Some(line)` when the record changed. `agents.reserve_auto
/// = false` turns the arm off.
pub fn auto_tick(
    home: &AgentsHome,
    cwd: &Path,
    procs: &[ProcRow],
    now_epoch: i64,
) -> Option<String> {
    let enabled = crate::agents_config::config_lookup(cwd, &["agents", "reserve_auto"])
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    let path = fleet_incident::fleet_stop_path(home);
    let apps = configured_apps(cwd);
    let seen = detect(procs, now_epoch, &apps);
    let current = fleet_incident::read_at(&path);
    let (state, reason, holds) = match (&current, seen) {
        (Verdict::Unavailable(_), _) => return None,
        (Verdict::Stopped(r), None) if r.origin.as_deref() == Some(RESERVE_AUTO_ORIGIN) => (
            "clear",
            "the app that reserved the machine quit".to_string(),
            Vec::new(),
        ),
        (Verdict::Stopped(r), Some((app, _)))
            if r.origin.as_deref() == Some(RESERVE_AUTO_ORIGIN) =>
        {
            let holds = preset_holds(app.preset)?;
            let left_ms = r
                .expires_at
                .as_deref()
                .and_then(epoch_of)
                .map_or(0, |at| (at - now_epoch) * 1000);
            if r.holds == holds && left_ms > AUTO_RENEW_BELOW_MS {
                return None;
            }
            ("stopped", auto_reason(app), holds)
        }
        (Verdict::Stopped(_), _) | (Verdict::Clear(_), None) => return None,
        (Verdict::Clear(r), Some((app, started))) => {
            // A person lifted the hold while this app ran: leave it lifted.
            let lifted_by_person = r.state == "clear"
                && r.origin.as_deref() != Some(RESERVE_AUTO_ORIGIN)
                && epoch_of(&r.changed_at).is_some_and(|at| at >= started);
            if lifted_by_person {
                return None;
            }
            ("stopped", auto_reason(app), preset_holds(app.preset)?)
        }
    };
    let expires_at = match state {
        "stopped" => Some(fleet_incident::expires_after(AUTO_TTL_MS).ok()?),
        _ => None,
    };
    let record = fleet_incident::write_transition_with_metadata(
        &path,
        state,
        Some(&reason),
        Some(RESERVE_AUTO_ORIGIN),
        holds,
        RecordMetadata {
            expires_at,
            origin: Some(RESERVE_AUTO_ORIGIN.to_string()),
            ..RecordMetadata::default()
        },
    )
    .ok()?;
    Some(format!(
        "reserve {} (generation {}): {}",
        record.state, record.generation, record.reason
    ))
}

fn auto_reason(app: &App) -> String {
    format!(
        "{} is running, so the fleet keeps the machine free for {} (agents.reserve_auto); `fno agents incident clear` lifts it until {} restarts",
        app.name, app.preset, app.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(command: &str, elapsed_s: u64) -> ProcRow {
        ProcRow {
            pid: 1,
            ppid: 1,
            state: 'S',
            elapsed_s,
            cpu_pct: 0.0,
            rss_kb: 0,
            command: command.to_string(),
        }
    }

    /// The daemon's reserve over its whole life: OBS arms stream, a quiet
    /// tick changes nothing, a person's clear stays cleared while OBS runs,
    /// and an OBS restart arms again. A user reserve and an operator stop
    /// are never replaced, and a prompt that names the bundle is no app.
    #[test]
    fn the_daemon_reserve_follows_the_app_and_yields_to_people() {
        let dir = std::env::temp_dir().join(format!("fno-reserve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = AgentsHome::at(dir.clone());
        let path = fleet_incident::fleet_stop_path(&home);
        let cwd = dir.clone();
        let now = chrono::Utc::now().timestamp();
        let obs = [row(
            "/Applications/OBS.app/Contents/MacOS/OBS --startstreaming",
            600,
        )];
        let prompt = [row(
            "/usr/local/bin/claude -p look at /Applications/OBS.app/Contents/MacOS/OBS",
            600,
        )];

        assert_eq!(
            auto_tick(&home, &cwd, &prompt, now),
            None,
            "a prompt is no app"
        );
        let armed = auto_tick(&home, &cwd, &obs, now).expect("OBS arms the reserve");
        assert!(armed.contains("OBS is running"), "{armed}");
        let Verdict::Stopped(record) = fleet_incident::read_at(&path) else {
            panic!("the reserve holds");
        };
        assert_eq!(record.holds, vec!["spawns", "tests"]);
        assert_eq!(
            auto_tick(&home, &cwd, &obs, now),
            None,
            "a fresh reserve is not renewed"
        );

        // A person's clear while OBS runs stays cleared; a restart re-arms.
        fleet_incident::write_transition_with_metadata(
            &path,
            "clear",
            Some("streaming done"),
            Some("user"),
            vec![],
            RecordMetadata::default(),
        )
        .unwrap();
        assert_eq!(auto_tick(&home, &cwd, &obs, now), None);
        let restarted = [row("/Applications/OBS.app/Contents/MacOS/OBS", 0)];
        assert!(auto_tick(&home, &cwd, &restarted, now + 60).is_some());

        // The app quits: the daemon lifts its own reserve.
        let lifted = auto_tick(&home, &cwd, &[], now + 120).expect("the quit lifts it");
        assert!(lifted.starts_with("reserve clear"), "{lifted}");

        // The daemon's reserve never replaces the user's; a reserve never
        // replaces an operator stop.
        assert!(may_replace(RESERVE_ORIGIN, Some(RESERVE_AUTO_ORIGIN)));
        assert!(!may_replace(RESERVE_AUTO_ORIGIN, Some(RESERVE_ORIGIN)));
        fleet_incident::write_transition_with_metadata(
            &path,
            "stopped",
            Some("incident"),
            Some("operator"),
            vec![],
            RecordMetadata::default(),
        )
        .unwrap();
        let over = fleet_incident::write_transition_with_metadata(
            &path,
            "stopped",
            Some("streaming"),
            Some("user"),
            preset_holds("stream").unwrap(),
            RecordMetadata {
                origin: Some(RESERVE_ORIGIN.to_string()),
                ..RecordMetadata::default()
            },
        );
        assert!(
            over.is_err(),
            "the writer refuses a reserve over an operator stop"
        );
        assert_eq!(auto_tick(&home, &cwd, &obs, now + 180), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The list is the config's, and a name matches whole words: `OBS` is
    /// not `Jobs`, `Premiere Pro` is `Adobe Premiere Pro 2025`, and an entry
    /// with `=stream` arms the stream preset.
    #[test]
    fn the_watched_apps_come_from_a_list_and_match_whole_words() {
        let apps = parse_apps(&["OBS=stream", "Premiere Pro", " ", "Ableton Live=edit"]);
        assert_eq!(apps.len(), 3, "a blank entry is dropped");
        let hit = |cmd: &str| app_of(cmd, &apps).map(|a| (a.name.as_str(), a.preset));
        assert_eq!(
            hit("/Applications/OBS.app/Contents/MacOS/OBS"),
            Some(("OBS", "stream"))
        );
        assert_eq!(
            hit("/Applications/Adobe Premiere Pro 2025/Adobe Premiere Pro 2025.app/Contents/MacOS/Adobe Premiere Pro 2025"),
            Some(("Premiere Pro", "edit"))
        );
        assert_eq!(
            hit("/Applications/Ableton Live 12.app/Contents/MacOS/Live"),
            Some(("Ableton Live", "edit"))
        );
        assert_eq!(hit("/Applications/Jobs.app/Contents/MacOS/Jobs"), None);
        assert!(
            parse_apps::<&str>(&[]).is_empty(),
            "an empty list watches nothing"
        );
    }
}
