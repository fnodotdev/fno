//! The bus log: where it lives, how its segments are read, and how one line
//! is appended under the sidecar lock. Nothing here names a fleet concept.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The bus log path, the one ladder every bus reader and writer shares:
/// `FNO_BUS_DIR`, then the `config.paths.bus_dir` override, else
/// `default_log`, the log the state-root layout names (the caller resolves
/// it, so this module stays free of the layout and the config cascade). The
/// caller reads the environment and the config and passes the values in.
pub fn resolve_log_path(
    default_log: &Path,
    bus_dir_env: Option<&str>,
    configured_dir: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = bus_dir_env.filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("messages.jsonl");
    }
    if let Some(dir) = configured_dir {
        return dir.join("messages.jsonl");
    }
    default_log.to_path_buf()
}

/// The bus DIRECTORY for `log_path`: its parent, or `<fno_dir>/bus` when the
/// path has none.
pub fn dir_of_log(log_path: &Path, fno_dir: &Path) -> PathBuf {
    log_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| fno_dir.join("bus"))
}

/// All retained bus lines oldest -> newest (log.py::_segment_paths_oldest_first
/// order), malformed lines skipped. Readers are lock-free by contract.
pub fn read_bus_segments(live: &Path) -> Vec<Value> {
    read_bus_segments_reported(live)
        .0
        .into_iter()
        .map(|r| r.row)
        .collect()
}

/// One parsed bus row and where it came from, so a validation warning can
/// name the segment and line the Python read leg named.
pub struct PositionedBusRow {
    pub row: Value,
    pub source: String,
    pub line: usize,
}

/// The same walk with one stderr warning per skipped line, the reader
/// contract `bus.log.iter_messages` froze: blank lines skip silently, a line
/// that fails to parse warns, and an unreadable segment warns whole.
pub fn read_bus_segments_reported(live: &Path) -> (Vec<PositionedBusRow>, Vec<String>) {
    let mut paths: Vec<(u64, PathBuf)> = Vec::new();
    if let Some(parent) = live.parent() {
        let prefix = format!(
            "{}.",
            live.file_name().unwrap_or_default().to_string_lossy()
        );
        if let Ok(rd) = std::fs::read_dir(parent) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(suffix) = name.strip_prefix(&prefix) {
                    if let Ok(n) = suffix.parse::<u64>() {
                        paths.push((n, entry.path()));
                    }
                }
            }
        }
    }
    paths.sort_by(|a, b| b.0.cmp(&a.0)); // oldest (highest N) first
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for p in paths
        .iter()
        .map(|(_, p)| p.as_path())
        .chain(std::iter::once(live))
    {
        let text = match std::fs::read_to_string(p) {
            Ok(text) => text,
            Err(e) => {
                warnings.push(format!("bus log: cannot read segment {}: {e}", p.display()));
                continue;
            }
        };
        for (lineno, line) in text.lines().enumerate() {
            match serde_json::from_str::<Value>(line) {
                Ok(v) if !super::envelope::is_readable(&v) => warnings.push(format!(
                    "bus log: skipping row {}:{} (v={} is newer than supported v={})",
                    p.file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default(),
                    lineno + 1,
                    super::envelope::row_version(&v),
                    super::envelope::ENVELOPE_VERSION
                )),
                Ok(v) => out.push(PositionedBusRow {
                    row: v,
                    source: p
                        .file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default()
                        .to_string(),
                    line: lineno + 1,
                }),
                Err(_) if line.trim().is_empty() => {}
                Err(_) => warnings.push(format!(
                    "bus log: skipping malformed line {}:{} (JSONDecodeError)",
                    p.file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default(),
                    lineno + 1
                )),
            }
        }
    }
    (out, warnings)
}

/// Exclusive flock on the Python sidecar lockfile (`messages.jsonl + .lock`),
/// so the one-line writer serializes against every Python appender.
struct BusLock {
    /// Held for the lock's lifetime; the flock dies with this handle.
    _file: File,
}

/// Why `append_raw` wrote nothing.
#[derive(Debug)]
pub enum AppendError {
    Serialize(serde_json::Error),
    LockDir(std::io::Error),
    LockOpen {
        path: PathBuf,
        source: std::io::Error,
    },
    LockTimeout {
        timeout: Duration,
        path: PathBuf,
    },
    Lock(std::io::Error),
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    Write(std::io::Error),
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::Serialize(e) => write!(f, "serialize: {e}"),
            AppendError::LockDir(e) => write!(f, "bus lock dir: {e}"),
            AppendError::LockOpen { path, source } => {
                write!(f, "bus lock open {}: {source}", path.display())
            }
            AppendError::LockTimeout { timeout, path } => {
                write!(
                    f,
                    "bus lock timeout after {timeout:?} at {}",
                    path.display()
                )
            }
            AppendError::Lock(e) => write!(f, "bus lock: {e}"),
            AppendError::Open { path, source } => {
                write!(f, "bus open {}: {source}", path.display())
            }
            AppendError::Write(e) => write!(f, "bus append: {e}"),
        }
    }
}

impl std::error::Error for AppendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppendError::Serialize(e) => Some(e),
            AppendError::LockDir(e) | AppendError::Lock(e) | AppendError::Write(e) => Some(e),
            AppendError::LockOpen { source, .. } | AppendError::Open { source, .. } => Some(source),
            AppendError::LockTimeout { .. } => None,
        }
    }
}

impl From<AppendError> for String {
    fn from(e: AppendError) -> String {
        e.to_string()
    }
}

impl BusLock {
    fn acquire(live: &Path) -> Result<BusLock, AppendError> {
        let lock_path = PathBuf::from(format!("{}.lock", live.display()));
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(AppendError::LockDir)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| AppendError::LockOpen {
                path: lock_path.clone(),
                source,
            })?;
        // The Python tests' tuning rides env now that the door owns the
        // wait: a contended lock must time out inside the caller's own
        // subprocess budget (FNO_BUS_LOCK_TIMEOUT_SECS).
        let timeout = lock_timeout();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(BusLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(AppendError::LockTimeout {
                            timeout,
                            path: lock_path,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(AppendError::Lock(e)),
            }
        }
    }
}

fn lock_timeout() -> Duration {
    std::env::var("FNO_BUS_LOCK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(LOCK_TIMEOUT)
}

/// The bus log's size cap and segment count, the Python appender's env
/// contract (`FNO_BUS_MAX_BYTES` / `FNO_BUS_RETAIN`).
fn bus_max_bytes() -> u64 {
    std::env::var("FNO_BUS_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(5 * 1024 * 1024)
}

fn bus_retain() -> usize {
    std::env::var("FNO_BUS_RETAIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(5)
}

/// Shift the rotated segments up one and start a fresh live log, the
/// Python appender's `_rotate_locked` port. Every step is best-effort: a
/// rotation failure must not lose the message, so the write follows anyway.
fn rotate_bus_locked(live: &Path) {
    let name = live
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let segment = |n: usize| live.with_file_name(format!("{name}.{n}"));
    let retain = bus_retain();
    let _ = std::fs::remove_file(segment(retain));
    for n in (1..retain).rev() {
        let _ = std::fs::rename(segment(n), segment(n + 1));
    }
    let _ = std::fs::rename(live, segment(1));
}

/// Append one row under the sidecar flock, with the Python appender's
/// rotation and owner-only mode. The caller owns every policy check (sender
/// guard, test fence, chat-store record) and runs it before this call.
pub fn append_raw(live: &Path, obj: &Value) -> Result<(), AppendError> {
    let mut line = serde_json::to_string(obj).map_err(AppendError::Serialize)?;
    line.push('\n');
    let _lock = BusLock::acquire(live)?;
    // A live log at or over the cap rotates before the write; a stat
    // failure must not lose the message.
    if let Ok(meta) = std::fs::metadata(live) {
        if meta.len() >= bus_max_bytes() {
            rotate_bus_locked(live);
        }
    }
    // The log holds message bodies: owner-only on creation and on an
    // existing segment, best-effort, like the Python appender's chmod.
    if live.exists() {
        let _ = std::fs::set_permissions(live, std::fs::Permissions::from_mode(0o600));
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(live)
        .map_err(|source| AppendError::Open {
            path: live.to_path_buf(),
            source,
        })?;
    f.write_all(line.as_bytes()).map_err(AppendError::Write)?;
    Ok(())
}
