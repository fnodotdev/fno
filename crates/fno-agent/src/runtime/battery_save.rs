//! The daemon's battery-save arm: at or below 5 percent on battery, save
//! state at once, so a dead-battery shutdown restores like a clean reboot.
//! One successful save per discharge episode; a failed save retries on the
//! next beat, and the latch clears when the source returns to AC. A
//! machine without a battery (a desktop on AC, or an unknown `pmset`
//! shape) never fires and costs one 60-second probe.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The charge at or below which the save fires, on battery or UPS.
pub const BATTERY_SAVE_THRESHOLD_PCT: u8 = 5;
/// The probe cadence. `pmset -g batt` is one local call, so 60 s bounds the
/// gap between a threshold crossing and the save to at most one minute.
pub const BATTERY_SAVE_INTERVAL_S: u64 = 60;
/// How long the graph WAL checkpoint waits on an active writer before it
/// gives up and leaves the fold to the next episode.
const CHECKPOINT_BUSY_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    Ac,
    Battery,
    Ups,
}

/// One `pmset -g batt` reading: where power comes from and the charge left.
/// `percent` is `None` when the source is AC or when the battery or UPS row
/// names no percent, and the arm refuses to fire on a reading it cannot
/// judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerReading {
    pub source: PowerSource,
    pub percent: Option<u8>,
}

/// Parse `pmset -g batt` stdout. `None` is output the parser does not
/// recognize (the binary is missing, or macOS changed the shape): the beat
/// skips rather than guess.
pub fn parse_pmset_batt(text: &str) -> Option<PowerReading> {
    let rest = text
        .lines()
        .find_map(|l| l.strip_prefix("Now drawing from '"))?;
    let quote = rest.find('\'')?;
    let source = match &rest[..quote] {
        "AC Power" => PowerSource::Ac,
        "Battery" => PowerSource::Battery,
        "UPS" => PowerSource::Ups,
        _ => return None,
    };
    let percent = match source {
        PowerSource::Ac => None,
        PowerSource::Battery | PowerSource::Ups => text
            .lines()
            .filter(|l| l.contains("-InternalBattery-") || l.contains("-UPS-"))
            .find_map(percent_of_line),
    };
    Some(PowerReading { source, percent })
}

/// The integer charge of one battery or UPS row: the digits immediately
/// before the row's first `%`. The row also carries digits elsewhere (the
/// battery index, the id), so a first-digit read would parse the wrong
/// number.
fn percent_of_line(line: &str) -> Option<u8> {
    let pct = line.find('%')?;
    let bytes = line.as_bytes();
    let mut start = pct;
    while start > 0
        && bytes
            .get(start - 1)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        start -= 1;
    }
    if start == pct {
        return None;
    }
    line.get(start..pct)?.parse::<u8>().ok()
}

/// The save fires when the machine runs on battery or UPS at or below the
/// threshold. AC never fires, and a source row without a percent never
/// fires: the arm saves on a reading, not on a guess.
pub fn should_save(reading: &PowerReading, threshold: u8) -> bool {
    reading.source != PowerSource::Ac && reading.percent.is_some_and(|pct| pct <= threshold)
}

/// One discharge episode: the source this episode drew from, and whether
/// the save already succeeded in it. A save that errored leaves `saved`
/// false, so the next beat retries until the episode ends.
#[derive(Default)]
struct Episode {
    source: Option<PowerSource>,
    saved: bool,
}

pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    episode: Arc<Mutex<Episode>>,
}

impl Default for Arm {
    fn default() -> Self {
        Self {
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            episode: Arc::new(Mutex::new(Episode::default())),
        }
    }
}

/// The firing half of one probe beat: an unrecognized reading skips, a
/// source change opens a new episode and re-arms the save, and the beat
/// fires at or below the threshold unless this episode already saved.
/// `episode_settle` records the outcome afterward.
fn episode_take(episode: &Mutex<Episode>, reading: Option<PowerReading>) -> bool {
    let Some(reading) = reading else {
        // A reading the parser does not recognize skips the beat. The
        // error was already logged at the probe site; staying silent here
        // keeps a headless Linux daemon from repeating it every minute.
        return false;
    };
    let mut episode = episode.lock().unwrap_or_else(|e| e.into_inner());
    if episode.source != Some(reading.source) {
        episode.source = Some(reading.source);
        episode.saved = false;
    }
    should_save(&reading, BATTERY_SAVE_THRESHOLD_PCT) && !episode.saved
}

/// The settling half: a successful save latches the episode; a failed one
/// stays armed, so the next 60-second beat retries until the episode ends.
fn episode_settle(episode: &Mutex<Episode>, saved: bool) {
    if saved {
        let mut episode = episode.lock().unwrap_or_else(|e| e.into_inner());
        episode.saved = true;
    }
}

pub fn maybe_tick(arm: &Arm) {
    let interval = Duration::from_secs(BATTERY_SAVE_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let episode = Arc::clone(&arm.episode);
    // SweepGate releases the one-in-flight flag on drop, so a panic in the
    // probe or the save cannot wedge the arm shut (the daemon loop's rule).
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let reading = probe();
        if episode_take(&episode, reading) {
            let saved = save_state();
            episode_settle(&episode, saved);
        }
    });
}

/// Read the power source. `None` is a probe the arm cannot use: a missing
/// or unreadable `pmset`, or output it cannot parse. The failure is logged
/// here once per beat; the beat itself stays quiet.
fn probe() -> Option<PowerReading> {
    let mut cmd = std::process::Command::new("pmset");
    cmd.arg("-g").arg("batt");
    // The bounded spawn seam: null stdin, own process group, transient
    // spawn failures retried. `output()` is a disallowed method here.
    let child = match crate::bounded_spawn::spawn_bounded_command(cmd) {
        Ok(child) => child,
        Err(error) => {
            // No `pmset` on this machine (a Linux dev box, a container):
            // the arm has nothing to watch, and one line per beat is the
            // honest record of that.
            tracing::debug!(?error, "battery-save: pmset unavailable");
            return None;
        }
    };
    let out = match child.wait_with_output() {
        Ok(out) => out,
        Err(error) => {
            tracing::debug!(%error, "battery-save: pmset read failed");
            return None;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match parse_pmset_batt(&text) {
        Some(reading) => Some(reading),
        // R9: the drop carries its reason - an unparseable shape must not
        // fire the save, and the operator can find this line in the log.
        None => {
            tracing::debug!("battery-save: pmset output not recognized; beat skipped");
            None
        }
    }
}

/// The save itself: every leg runs even when the one before it failed, and
/// the `true` answer means no leg errored. The outbox keeps its rows when
/// the primary is unreachable, so a retry on the next beat sends them.
fn save_state() -> bool {
    let graph = crate::backlog::settings::graph_path();
    let mut saved = true;
    if let Err(error) = crate::backlog::backlog_share::flush(&graph) {
        tracing::warn!(%error, "battery-save: outbox flush failed");
        saved = false;
    }
    if let Err(error) = crate::backlog::backlog_share::sync(&graph) {
        tracing::warn!(%error, "battery-save: outbox sync failed");
        saved = false;
    }
    saved &= checkpoint_graph(&graph);
    saved
}

/// Fold the graph's WAL into the main file, so a dead-battery shutdown is
/// followed by a first open that replays nothing. Every commit is already
/// `synchronous=FULL` durable, so this is a recovery-smoothing leg, not a
/// durability one; a busy store (a writer mid-batch) skips until the next
/// beat.
fn checkpoint_graph(graph: &Path) -> bool {
    let db = crate::backlog::database_path(graph);
    if !db.exists() {
        return true;
    }
    let opened = rusqlite::Connection::open(&db).and_then(|conn| {
        conn.busy_timeout(CHECKPOINT_BUSY_WAIT)?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    });
    if let Err(error) = opened {
        tracing::warn!(%error, "battery-save: graph WAL checkpoint failed");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const AC_ONLY: &str = "Now drawing from 'AC Power'\n";
    const BATTERY_4: &str = "Now drawing from 'Battery'\n -InternalBattery-0 (id=5373031)\t4%; discharging; 0:42 remaining present: true\n";
    const BATTERY_92: &str = "Now drawing from 'Battery'\n -InternalBattery-0 (id=5373031)\t92%; discharging; 4:11 remaining present: true\n";
    const BATTERY_NO_PCT: &str = "Now drawing from 'Battery'\n -InternalBattery-0 (id=5373031)\tdischarging; present: true\n";

    #[test]
    fn parse_reads_source_and_charge() {
        let cases = [
            (AC_ONLY, PowerSource::Ac, None),
            (BATTERY_4, PowerSource::Battery, Some(4)),
            (BATTERY_92, PowerSource::Battery, Some(92)),
        ];
        for (text, source, percent) in cases {
            let reading = parse_pmset_batt(text).expect("recognized shape parses");
            assert_eq!(reading.source, source, "{text}");
            assert_eq!(reading.percent, percent, "{text}");
        }
        assert!(
            parse_pmset_batt(BATTERY_NO_PCT)
                .is_some_and(|r| r.source == PowerSource::Battery && r.percent.is_none()),
            "a battery row with no percent still reads its source"
        );
        assert!(
            parse_pmset_batt("").is_none(),
            "empty output is not a reading"
        );
    }

    #[test]
    fn percent_takes_the_digits_before_the_percent_sign() {
        // The row's first digits are the battery index and id; the charge
        // lives right before `%`.
        assert_eq!(percent_of_line("\t4%; discharging"), Some(4));
        assert_eq!(percent_of_line("(id=5373031)\t92%; discharging"), Some(92));
        assert_eq!(percent_of_line("no charge here"), None);
    }

    #[test]
    fn save_fires_only_on_battery_at_or_below_threshold() {
        let threshold = BATTERY_SAVE_THRESHOLD_PCT;
        let ac_high = PowerReading {
            source: PowerSource::Ac,
            percent: None,
        };
        let battery_6 = PowerReading {
            source: PowerSource::Battery,
            percent: Some(6),
        };
        let battery_5 = PowerReading {
            source: PowerSource::Battery,
            percent: Some(5),
        };
        let battery_0 = PowerReading {
            source: PowerSource::Battery,
            percent: Some(0),
        };
        let battery_unjudged = PowerReading {
            source: PowerSource::Battery,
            percent: None,
        };
        let ups_3 = PowerReading {
            source: PowerSource::Ups,
            percent: Some(3),
        };
        assert!(!should_save(&ac_high, threshold), "AC never fires");
        assert!(
            !should_save(&battery_6, threshold),
            "above the threshold never fires"
        );
        assert!(
            should_save(&battery_5, threshold),
            "exactly at the threshold fires"
        );
        assert!(should_save(&battery_0, threshold), "zero charge fires");
        assert!(
            !should_save(&battery_unjudged, threshold),
            "no percent, no fire"
        );
        assert!(should_save(&ups_3, threshold), "a UPS counts as battery");
    }

    #[test]
    fn beat_saves_once_per_episode_and_rearms_on_ac() {
        let episode = Mutex::new(Episode::default());
        let low = PowerReading {
            source: PowerSource::Battery,
            percent: Some(4),
        };
        let high = PowerReading {
            source: PowerSource::Battery,
            percent: Some(92),
        };
        let ac = PowerReading {
            source: PowerSource::Ac,
            percent: None,
        };
        assert!(
            !episode_take(&episode, Some(high)),
            "above threshold, no save"
        );
        assert!(episode_take(&episode, Some(low)), "first crossing fires");
        episode_settle(&episode, true);
        assert!(
            !episode_take(&episode, Some(low)),
            "the same episode does not refire"
        );
        assert!(!episode_take(&episode, Some(ac)), "AC itself never fires");
        assert!(
            episode_take(&episode, Some(low)),
            "a new discharge episode re-arms"
        );
    }

    #[test]
    fn a_failed_save_stays_armed_for_the_next_beat() {
        let episode = Mutex::new(Episode::default());
        let low = PowerReading {
            source: PowerSource::Battery,
            percent: Some(3),
        };
        assert!(episode_take(&episode, Some(low)), "crossing fires");
        episode_settle(&episode, false);
        assert!(
            episode_take(&episode, Some(low)),
            "a failed save retries on the next beat"
        );
        episode_settle(&episode, true);
        assert!(
            !episode_take(&episode, Some(low)),
            "a successful save latches the episode"
        );
    }
}
