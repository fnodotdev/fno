//! The disk report: size per class with what is safe to delete. The
//! reading rides the census op as `--disk` (shrink law: no new client
//! action); `fno doctor disk` is a pure transport here.
//!
//! Attribution follows the class map: rebuildables in the cache class,
//! logs in the state class, install payloads in the data class, and the
//! state root's irreplaceable members in the config class.
//!
//! Verdicts are lane-backed: a verdict naming `fno doctor reclaim` is
//! enforced by a lane in reclaim.rs with the same bound, and a
//! self-capping store names its own cap in the verdict.
//!
//! The walk is bounded and streaming: one deadline covers the report, a
//! row that runs past it keeps its bytes so far (or stays uncounted) and
//! names the budget in its verdict, and the human render prints each row
//! as soon as its walk lands, cheap rows first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The user home: HOME, else /.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The state root: FNO_AGENT_HOME, else the ambient agents home.
fn state_root() -> PathBuf {
    crate::paths::AgentsHome::from_env()
        .root()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| home().join(".fno"))
}

/// One shared walk deadline. `start` begins it; `expired` reads it; `secs`
/// names it, so a stopped walk can say what stopped it.
pub(crate) struct WalkBudget {
    deadline: Instant,
    secs: u64,
}

impl WalkBudget {
    pub(crate) fn start(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
            secs: budget.as_secs().max(1),
        }
    }
    pub(crate) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
    pub(crate) fn secs(&self) -> u64 {
        self.secs
    }
}

/// The report's whole-walk budget. Long enough for a full healthy read of
/// the real state root, short enough that the door always answers; the
/// measured full walk there runs minutes only when the cache pile peaks.
const DISK_WALK_BUDGET_SECS: u64 = 60;

/// Bytes under `root`, symlinks skipped at descent, bounded by `budget`:
/// the walk checks the clock every 256 entries and stops descending once
/// it expired. The bool reads true when it stopped early, so no reader
/// takes a stopped walk for a whole one. Iterative on purpose: the state
/// root's trees run deep.
pub(crate) fn dir_bytes_bounded(root: &Path, budget: &WalkBudget) -> (u64, bool) {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return (0, false);
    };
    if meta.is_symlink() {
        return (0, false);
    }
    if !meta.is_dir() {
        return (meta.len(), false);
    }
    let mut total = 0u64;
    let mut partial = false;
    let mut visited = 0usize;
    let mut stack = vec![root.to_path_buf()];
    'outer: while let Some(dir) = stack.pop() {
        if budget.expired() {
            partial = true;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited % 256 == 0 && budget.expired() {
                partial = true;
                break 'outer;
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else {
                total += meta.len();
            }
        }
    }
    (total, partial)
}

/// One bounded read behind a row: bytes, partial flag, skipped flag. A row
/// that starts after the deadline is skipped, never half-walked again.
fn dir_row_bytes(root: &Path, budget: &WalkBudget) -> (u64, bool, bool) {
    if budget.expired() {
        return (0, false, true);
    }
    let (bytes, partial) = dir_bytes_bounded(root, budget);
    (bytes, partial, false)
}

/// The bounded read behind a multi-root row (the class homes and the
/// cargo-build store span several roots): the roots sum, and any partial
/// or skipped root marks the row.
fn sum_bounded(roots: &[PathBuf], budget: &WalkBudget) -> (u64, bool, bool) {
    let mut bytes = 0u64;
    let mut partial = false;
    let mut skipped = false;
    for root in roots {
        let (b, p, s) = dir_row_bytes(root, budget);
        bytes += b;
        partial |= p;
        skipped |= s;
    }
    (bytes, partial, skipped)
}

/// The verdict a row carries, naming the budget when the walk stopped.
fn budget_verdict(verdict: &str, partial: bool, skipped: bool, secs: u64) -> String {
    if skipped {
        format!("{verdict} [skipped: walk budget {secs}s hit]")
    } else if partial {
        format!("{verdict} [partial: walk budget {secs}s hit]")
    } else {
        verdict.to_string()
    }
}

fn store_row(name: &str, class: &str, bytes: u64, verdict: &str) -> serde_json::Value {
    serde_json::json!({ "store": name, "class": class, "bytes": bytes, "verdict": verdict })
}

fn class_row(name: &str, home: &Path, bytes: u64, verdict: &str) -> serde_json::Value {
    serde_json::json!({ "class": name, "home": home.to_string_lossy(), "bytes": bytes, "verdict": verdict })
}

/// The class rows, computed cheap-first (state, data, config, cache) so
/// the streaming render answers before the cache walk. Each row lands
/// through `emit` as soon as its walk finishes.
fn class_rows(budget: &WalkBudget, mut emit: impl FnMut(serde_json::Value)) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = state_root();
    let cache_home = crate::paths_cli::cache_home(&cwd);
    let data_home = crate::paths_cli::data_home(&cwd);
    let state_home = crate::paths_cli::state_home(&cwd);
    let (bytes, partial, skipped) = sum_bounded(&[state_home.clone(), root.join("logs")], budget);
    emit(class_row(
        "state",
        &state_home,
        bytes,
        &budget_verdict("keep", partial, skipped, budget.secs()),
    ));
    let (bytes, partial, skipped) = dir_row_bytes(&data_home, budget);
    emit(class_row(
        "data",
        &data_home,
        bytes,
        &budget_verdict("keep", partial, skipped, budget.secs()),
    ));
    const MOVED: [&str; 5] = ["cache", "logs", "install", "otel", "history"];
    let mut config_bytes = 0u64;
    let mut config_partial = false;
    let mut config_skipped = budget.expired();
    if !config_skipped {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if MOVED.contains(&entry.file_name().to_string_lossy().as_ref()) {
                    continue;
                }
                let (b, p, s) = dir_row_bytes(&entry.path(), budget);
                config_bytes += b;
                config_partial |= p;
                config_skipped |= s;
                if config_skipped {
                    break;
                }
            }
        }
    }
    emit(class_row(
        "config",
        &root,
        config_bytes,
        &budget_verdict("keep", config_partial, config_skipped, budget.secs()),
    ));
    let (bytes, partial, skipped) = sum_bounded(&[cache_home.clone(), root.join("cache")], budget);
    emit(class_row(
        "cache",
        &cache_home,
        bytes,
        &budget_verdict("safe", partial, skipped, budget.secs()),
    ));
}

/// The named store rows, one per family with a lane or a self-cap behind
/// its verdict, cheap-first so the streaming render answers early; the
/// cargo-build walk goes last because it is the long one. The snapshots
/// pile has no row: its bound is the share rotation's keep-N, with no
/// lane to name.
fn store_rows(budget: &WalkBudget, mut emit: impl FnMut(serde_json::Value)) {
    let cache_sum = |name: &str, budget: &WalkBudget| -> (u64, bool, bool) {
        let roots: Vec<PathBuf> = crate::reclaim::cache_class_roots(name)
            .iter()
            .map(|root| root.to_path_buf())
            .collect();
        sum_bounded(&roots, budget)
    };
    let (bytes, partial, skipped) = cache_sum("gh-facts", budget);
    emit(store_row(
        "gh-facts",
        "cache",
        bytes,
        &budget_verdict(
            "safe: reclaim removes rows older than 30 days",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
    let (bytes, partial, skipped) = cache_sum("pr-status", budget);
    emit(store_row(
        "pr-status",
        "cache",
        bytes,
        &budget_verdict(
            "safe: reclaim trims at 14 days and 256 MiB",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
    let (otel_a, pa, sa) = dir_row_bytes(&crate::paths::AgentsHome::from_env().otel_dir(), budget);
    let (otel_b, pb, sb) = dir_row_bytes(&home().join(".fno").join("otel"), budget);
    emit(store_row(
        "otel",
        "cache",
        otel_a + otel_b,
        &budget_verdict(
            "safe: the ingest loop sheds rows older than 14 days and caps at 2 GiB",
            pa || pb,
            sa || sb,
            budget.secs(),
        ),
    ));
    let history = crate::eval_reap::eval_history_path().unwrap_or_else(|| {
        crate::state_layout::place_in(&home(), "history", "evals-history.jsonl")
    });
    let mut gen_paths = vec![history.clone()];
    for gen in [".1", ".2", ".3"] {
        let mut gen_path = history.clone().into_os_string();
        gen_path.push(gen);
        gen_paths.push(PathBuf::from(gen_path));
    }
    let (bytes, partial, skipped) = sum_bounded(&gen_paths, budget);
    emit(store_row(
        "evals-history",
        "cache",
        bytes,
        &budget_verdict(
            "safe: rotation sheds generations past 256 MiB, three generations, 90-day floor",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state_home = crate::paths_cli::state_home(&cwd);
    let root = state_root();
    let (bytes, partial, skipped) =
        sum_bounded(&[state_home.join("logs"), root.join("logs")], budget);
    emit(store_row(
        "logs",
        "state",
        bytes,
        &budget_verdict(
            "keep: append-only, class home resolves it",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
    let (bytes, partial, skipped) = dir_row_bytes(&root.join("backups"), budget);
    emit(store_row(
        "backups",
        "config",
        bytes,
        &budget_verdict(
            "safe once the audit verdict file exists: default retention 14 days",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
    let mut retired = 0u64;
    let mut retired_partial = false;
    let mut retired_skipped = budget.expired();
    let install_root = home().join(".fno/install");
    if !retired_skipped {
        if let Ok(entries) = std::fs::read_dir(&install_root) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("bin.retired-")
                {
                    let (b, p, s) = dir_row_bytes(&entry.path(), budget);
                    retired += b;
                    retired_partial |= p;
                    retired_skipped |= s;
                    if retired_skipped {
                        break;
                    }
                }
            }
        }
    }
    emit(store_row(
        "install-retired",
        "data",
        retired,
        &budget_verdict(
            "safe: reclaim removes retired payloads after 7 days",
            retired_partial,
            retired_skipped,
            budget.secs(),
        ),
    ));
    let mut strays = 0u64;
    let mut strays_partial = false;
    let mut strays_skipped = budget.expired();
    for name in ["events.db.db", "events.db.db-shm", "events.db.db-wal"] {
        let (b, p, s) = dir_row_bytes(&root.join(name), budget);
        strays += b;
        strays_partial |= p;
        strays_skipped |= s;
    }
    emit(store_row(
        "strays",
        "data",
        strays,
        &budget_verdict(
            "safe: the stray-db lane removes them after 7 days",
            strays_partial,
            strays_skipped,
            budget.secs(),
        ),
    ));
    let (bytes, partial, skipped) = cache_sum("cargo-build", budget);
    emit(store_row(
        "cargo-build",
        "cache",
        bytes,
        &budget_verdict(
            "safe: reclaim drains stale cargo bases; sccache caps its own dir",
            partial,
            skipped,
            budget.secs(),
        ),
    ));
}

/// The report under an explicit budget: the shape tests and the json
/// transport both land here.
pub(crate) fn disk_report_with_budget(budget: Duration) -> serde_json::Value {
    let budget = WalkBudget::start(budget);
    let mut classes = Vec::new();
    class_rows(&budget, |row| classes.push(row));
    let mut stores = Vec::new();
    store_rows(&budget, |row| stores.push(row));
    serde_json::json!({
        "classes": classes,
        "stores": stores,
    })
}

/// Bytes in binary units a reader scans.
fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0usize;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

/// `fno doctor disk` lands here through census `--disk`: stream the rows
/// human or print the document whole in json. Either way the walk is
/// bounded by one budget, and a row the budget stopped says so in its
/// verdict instead of passing a stopped walk off as a whole one.
pub fn disk_command(json: bool) -> i32 {
    if json {
        let report = disk_report_with_budget(Duration::from_secs(DISK_WALK_BUDGET_SECS));
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
        return 0;
    }
    let budget = WalkBudget::start(Duration::from_secs(DISK_WALK_BUDGET_SECS));
    println!("fno disk report (classes and stores overlap by design)");
    class_rows(&budget, |row| {
        println!(
            "class {:>7} {:<28} {:>9}  {}",
            row["class"].as_str().unwrap_or("?"),
            row["home"].as_str().unwrap_or("?"),
            human_bytes(row["bytes"].as_u64().unwrap_or(0)),
            row["verdict"].as_str().unwrap_or(""),
        );
    });
    store_rows(&budget, |row| {
        println!(
            "store {:>15} {:<7} {:>9}  {}",
            row["store"].as_str().unwrap_or("?"),
            row["class"].as_str().unwrap_or("?"),
            human_bytes(row["bytes"].as_u64().unwrap_or(0)),
            row["verdict"].as_str().unwrap_or(""),
        );
    });
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_pins_four_classes_and_finds_a_store_row() {
        let _guard = crate::claims::test_env_lock();
        let base = std::env::temp_dir().join(format!("fno-disk-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let pins = [
            ("FNO_AGENT_HOME", "home"),
            ("FNO_CACHE_HOME", "cache"),
            ("FNO_DATA_HOME", "data"),
            ("FNO_STATE_HOME", "state"),
        ];
        for (key, leaf) in pins {
            std::fs::create_dir_all(base.join(leaf)).unwrap();
            std::env::set_var(key, base.join(leaf));
        }
        let facts = base.join("cache/gh-facts");
        std::fs::create_dir_all(&facts).unwrap();
        std::fs::write(facts.join("old-row.json"), "12345678\n").unwrap();
        let report = disk_report_with_budget(Duration::from_secs(DISK_WALK_BUDGET_SECS));
        for (key, _) in pins {
            std::env::remove_var(key);
        }
        assert_eq!(report["classes"].as_array().map(Vec::len), Some(4));
        let rows = report["stores"].as_array().unwrap();
        let gh = rows
            .iter()
            .find(|r| r["store"] == "gh-facts")
            .expect("gh-facts row");
        assert!(gh["bytes"].as_u64().unwrap() >= 8, "{gh}");
        assert!(!gh["verdict"].as_str().unwrap().contains("partial"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn an_expired_budget_marks_rows_and_returns_fast() {
        let _guard = crate::claims::test_env_lock();
        let base = std::env::temp_dir().join(format!("fno-disk-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let pins = [
            ("FNO_AGENT_HOME", "home"),
            ("FNO_CACHE_HOME", "cache"),
            ("FNO_DATA_HOME", "data"),
            ("FNO_STATE_HOME", "state"),
        ];
        for (key, leaf) in pins {
            std::fs::create_dir_all(base.join(leaf)).unwrap();
            std::env::set_var(key, base.join(leaf));
        }
        // A zero budget expires at construction: the config row's upfront
        // expiry seed marks it no matter how fast the machine runs the
        // empty walks.
        let report = disk_report_with_budget(Duration::ZERO);
        for (key, _) in pins {
            std::env::remove_var(key);
        }
        let rows = report["classes"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        let marked = rows
            .iter()
            .filter(|r| {
                let v = r["verdict"].as_str().unwrap_or("");
                v.contains("partial") || v.contains("skipped")
            })
            .count();
        assert!(marked >= 1, "no row names the budget: {rows:?}");
        let store_rows = report["stores"].as_array().unwrap();
        let strays = store_rows.iter().find(|r| r["store"] == "strays").unwrap();
        assert!(
            strays["verdict"].as_str().unwrap().contains("skipped"),
            "{strays}"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
