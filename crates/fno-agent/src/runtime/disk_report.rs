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

use std::path::{Path, PathBuf};

/// The user home: HOME, else /.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The state root: FNO_AGENT_HOME, else the ambient agents home.
fn state_root() -> PathBuf {
    if let Some(p) = std::env::var_os("FNO_AGENT_HOME").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    crate::paths::AgentsHome::from_env().root().to_path_buf()
}

/// Bytes under `root`, symlinks skipped at descent. The logs link and
/// class-home entries pointing back into counted homes would otherwise
/// double-count.
fn dir_bytes(root: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return 0;
    };
    if meta.is_dir() {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                total += dir_bytes(&entry.path());
            }
        }
        return total;
    }
    if meta.is_symlink() {
        return 0;
    }
    meta.len()
}

/// Cache-store roots: the class cache home first, the legacy state-root
/// spelling while the class pass has not moved the folder yet. Both can
/// exist mid-migration; a missing root contributes zero through
/// [`dir_bytes`].
fn cache_roots(name: &str) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    vec![
        crate::paths_cli::cache_home(&cwd).join(name),
        state_root().join("cache").join(name),
    ]
}

fn store_row(name: &str, class: &str, bytes: u64, verdict: &str) -> serde_json::Value {
    serde_json::json!({ "store": name, "class": class, "bytes": bytes, "verdict": verdict })
}

fn class_row(name: &str, home: &Path, bytes: u64, verdict: &str) -> serde_json::Value {
    serde_json::json!({ "class": name, "home": home.to_string_lossy(), "bytes": bytes, "verdict": verdict })
}

/// The named store rows, one per family with a lane or a self-cap behind
/// its verdict. The snapshots pile has no row: its bound is the share
/// rotation's keep-N, with no lane to name.
fn stores() -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    let cache_sum =
        |name: &str| -> u64 { cache_roots(name).iter().map(|root| dir_bytes(root)).sum() };
    rows.push(store_row(
        "cargo-build",
        "cache",
        cache_sum("cargo-build"),
        "safe: reclaim drains stale cargo bases; sccache caps its own dir",
    ));
    rows.push(store_row(
        "gh-facts",
        "cache",
        cache_sum("gh-facts"),
        "safe: reclaim removes rows older than 30 days",
    ));
    rows.push(store_row(
        "pr-status",
        "cache",
        cache_sum("pr-status"),
        "safe: reclaim trims at 14 days and 256 MiB",
    ));
    let otel_bytes = dir_bytes(&crate::paths::AgentsHome::from_env().otel_dir())
        + dir_bytes(&home().join(".fno").join("otel"));
    rows.push(store_row(
        "otel",
        "cache",
        otel_bytes,
        "safe: the ingest loop sheds rows older than 14 days and caps at 2 GiB",
    ));
    let history = crate::eval_reap::eval_history_path().unwrap_or_else(|| {
        crate::state_layout::place_in(&home(), "history", "evals-history.jsonl")
    });
    let mut evals_bytes = dir_bytes(&history);
    for gen in [".1", ".2", ".3"] {
        let mut gen_path = history.clone().into_os_string();
        gen_path.push(gen);
        evals_bytes += dir_bytes(&PathBuf::from(gen_path));
    }
    rows.push(store_row(
        "evals-history",
        "cache",
        evals_bytes,
        "safe: rotation sheds generations past 256 MiB, three generations, 90-day floor",
    ));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state_home = crate::paths_cli::state_home(&cwd);
    let root = state_root();
    let logs_bytes = dir_bytes(&state_home.join("logs")) + dir_bytes(&root.join("logs"));
    rows.push(store_row(
        "logs",
        "state",
        logs_bytes,
        "keep: append-only, class home resolves it",
    ));
    rows.push(store_row(
        "backups",
        "config",
        dir_bytes(&root.join("backups")),
        "safe once the audit verdict file exists: default retention 14 days",
    ));
    let mut retired = 0u64;
    let install_root = home().join(".fno/install");
    if let Ok(entries) = std::fs::read_dir(&install_root) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("bin.retired-")
            {
                retired += dir_bytes(&entry.path());
            }
        }
    }
    rows.push(store_row(
        "install-retired",
        "data",
        retired,
        "safe: reclaim removes retired payloads after 7 days",
    ));
    let mut strays = 0u64;
    for name in ["events.db.db", "events.db.db-shm", "events.db.db-wal"] {
        strays += dir_bytes(&root.join(name));
    }
    rows.push(store_row(
        "strays",
        "data",
        strays,
        "safe: the stray-db lane removes them after 7 days",
    ));
    rows
}

/// The class homes. Each class is one walk of its home: config is the
/// state root minus the top-level members that moved to their own
/// classes (cache, logs, install, otel, history), and the other three
/// are the class homes whole. The stores array is a second, independent
/// view: a store's bytes can overlap a class walk by design, and the
/// human render says so.
fn classes() -> Vec<serde_json::Value> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = state_root();
    let cache_home = crate::paths_cli::cache_home(&cwd);
    let data_home = crate::paths_cli::data_home(&cwd);
    let state_home = crate::paths_cli::state_home(&cwd);
    const MOVED: [&str; 5] = ["cache", "logs", "install", "otel", "history"];
    let mut config_bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if MOVED.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            config_bytes += dir_bytes(&entry.path());
        }
    }
    vec![
        class_row("config", &root, config_bytes, "keep"),
        class_row(
            "cache",
            &cache_home,
            dir_bytes(&cache_home) + dir_bytes(&root.join("cache")),
            "safe",
        ),
        class_row(
            "state",
            &state_home,
            dir_bytes(&state_home) + dir_bytes(&root.join("logs")),
            "keep",
        ),
        class_row("data", &data_home, dir_bytes(&data_home), "keep"),
    ]
}

/// The report: four class rows and the store rows, as one json value.
pub(crate) fn disk_report() -> serde_json::Value {
    serde_json::json!({
        "classes": classes(),
        "stores": stores(),
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

/// The human render: one line per class and per store. The two views
/// overlap by design, and the header says so.
fn render_human(report: &serde_json::Value) {
    println!("fno disk report (classes and stores overlap by design)");
    if let Some(rows) = report["classes"].as_array() {
        for row in rows {
            println!(
                "class {:>7} {:<28} {:>9}  {}",
                row["class"].as_str().unwrap_or("?"),
                row["home"].as_str().unwrap_or("?"),
                human_bytes(row["bytes"].as_u64().unwrap_or(0)),
                row["verdict"].as_str().unwrap_or(""),
            );
        }
    }
    if let Some(rows) = report["stores"].as_array() {
        for row in rows {
            println!(
                "store {:>15} {:<7} {:>9}  {}",
                row["store"].as_str().unwrap_or("?"),
                row["class"].as_str().unwrap_or("?"),
                human_bytes(row["bytes"].as_u64().unwrap_or(0)),
                row["verdict"].as_str().unwrap_or(""),
            );
        }
    }
}

/// `fno doctor disk` lands here through census `--disk`: print the
/// report, human or json.
pub fn disk_command(json: bool) -> i32 {
    let report = disk_report();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        render_human(&report);
    }
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
        let report = disk_report();
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
        std::fs::remove_dir_all(&base).ok();
    }
}
