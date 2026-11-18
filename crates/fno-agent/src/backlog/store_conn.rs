//! The one door to the shared fleet stores.
//!
//! graph.db (the backlog and the claims table) and the event store open here
//! and nowhere else. The seam owns the open call, the live-store fence, the
//! busy wait and the WAL and FULL pragmas. Each caller keeps its own DDL.
//! Reads take a read-only handle and writes a read-write one, so a backend
//! that reads a local replica and writes to a remote primary changes these
//! two functions and no caller.

use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::path::Path;
use std::time::Duration;

const BUSY_WAIT: Duration = Duration::from_secs(5);

/// A read-write handle, creating the file and its parent when absent.
pub fn open_write(path: &Path) -> Result<Connection, String> {
    crate::live_store_fence::refuse_worktree_build_on_operator_store(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| named(parent, error))?;
    }
    refuse_truncated_database(path)?;
    let connection = Connection::open(path).map_err(|error| named(path, error))?;
    connection
        .busy_timeout(BUSY_WAIT)
        .map_err(|error| named(path, error))?;
    ensure_wal(&connection, path)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|error| named(path, error))?;
    Ok(connection)
}

/// A read-only handle. Never creates the file.
pub fn open_read(path: &Path) -> Result<Connection, String> {
    refuse_truncated_database(path)?;
    // The plain open first so its error stays the named failure; the
    // immutable retry only fires on the clean-exit mint (header stays WAL,
    // sidecars gone) that this door's own WAL stores become.
    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(original) => open_read_only_wal_safe(path).ok_or_else(|| named(path, original))?,
    };
    connection
        .busy_timeout(BUSY_WAIT)
        .map_err(|error| named(path, error))?;
    connection
        .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| named(path, error))?;
    Ok(connection)
}

/// Run `read` on this process's one reused read-only handle for `path`.
///
/// An open parses the whole schema, and the daemon's tick loops read the
/// store many times a second, so a fresh handle per read was most of its
/// CPU. The handle is reopened when the file at `path` is replaced. A read
/// that finds the handle busy on another thread, or a nested read of the
/// same path, takes a fresh handle instead of waiting.
pub fn with_read<T>(path: &Path, read: impl FnOnce(&Connection) -> T) -> Result<T, String> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock};
    type Slot = Arc<Mutex<Option<(u64, Connection)>>>;
    // Tests open many short-lived stores; the cap keeps their handles from
    // piling up. The daemon reads a handful of paths.
    const MAX_HELD: usize = 8;
    static HELD: OnceLock<Mutex<HashMap<PathBuf, Slot>>> = OnceLock::new();

    let identity = file_identity(path)?;
    let slot = {
        let mut held = HELD
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.len() >= MAX_HELD && !held.contains_key(path) {
            held.clear();
        }
        Arc::clone(held.entry(path.to_path_buf()).or_default())
    };
    let mut guard = match slot.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Ok(read(&open_read(path)?)),
    };
    if guard.as_ref().is_none_or(|(held, _)| *held != identity) {
        *guard = None;
        // Only a plain read-only handle is kept. The immutable retry in
        // `open_read` never sees a later write, so it is never held.
        match open_live_read(path) {
            Ok(connection) => *guard = Some((identity, connection)),
            Err(_) => return Ok(read(&open_read(path)?)),
        }
    }
    let (_, connection) = guard.as_ref().expect("the slot was filled above");
    Ok(read(connection))
}

fn open_live_read(path: &Path) -> Result<Connection, String> {
    refuse_truncated_database(path)?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| named(path, error))?;
    connection
        .busy_timeout(BUSY_WAIT)
        .map_err(|error| named(path, error))?;
    connection
        .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| named(path, error))?;
    Ok(connection)
}

/// What tells a replaced store file from the one a held handle reads.
fn file_identity(path: &Path) -> Result<u64, String> {
    let meta = path.metadata().map_err(|error| named(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(meta.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64))
    }
}

/// The one read-only open over a WAL-mode store, shared by this door's
/// readers: an ordinary read-only open, then the immutable retry a store
/// whose daemon exited clean (the -shm sidecar gone while the header stays
/// WAL) needs. The URI spelling percent-encodes the path so the
/// `?immutable=1` query survives reserved characters in the path itself.
/// The retry fires only when the plain open refused, so a live writer never
/// takes it. `None` when neither opens. Self-contained because build.rs
/// copies this module verbatim into the fno twin, which has no graph_store.
fn open_read_only_wal_safe(path: &Path) -> Option<Connection> {
    if let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        return Some(conn);
    }
    let mut encoded = String::new();
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-' | b'~' => {
                encoded.push(*byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    let uri = format!("file:{encoded}?immutable=1");
    let uri_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    Connection::open_with_flags(uri, uri_flags).ok()
}

fn refuse_truncated_database(path: &Path) -> Result<(), String> {
    // SQLite's header is 100 bytes. Metadata avoids a raw descriptor whose
    // close could release this process's SQLite locks.
    match path.metadata() {
        Ok(meta) if meta.len() > 0 && meta.len() < 100 => {
            Err(format!("{}: file is not a database", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(named(path, error)),
    }
}

fn ensure_wal(connection: &Connection, path: &Path) -> Result<(), String> {
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| named(path, error))?;
    if mode.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    // First opens of a new file race to switch it to WAL. SQLite answers the
    // loser busy at once, with no busy handler, since waiting could deadlock.
    // The loser goes on: the winner switches the file, and a connection that
    // meets a WAL file reads its header and uses WAL. A retry here waited out
    // a reader in this same process that could not finish until the open did.
    match connection.execute_batch("PRAGMA journal_mode=WAL;") {
        Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy => {
            Ok(())
        }
        outcome => outcome.map_err(|error| named(path, error)),
    }
}

fn named(path: &Path, error: impl std::fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_conn_writes_wal_full_reads_read_only_and_refuses_non_databases() {
        assert!(rusqlite::version_number() >= 3_051_003);
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.db");
        let error = open_read(&absent).unwrap_err();
        assert!(error.starts_with(&absent.display().to_string()), "{error}");
        assert!(!absent.exists(), "a read never creates the store");

        let path = dir.path().join("nested/graph.db");
        let writer = open_write(&path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = writer
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!((mode.as_str(), synchronous), ("wal", 2));
        writer
            .execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('row');")
            .unwrap();
        let reader = open_read(&path).unwrap();
        let value: String = reader
            .query_row("SELECT v FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "row");
        assert!(reader.execute_batch("INSERT INTO t VALUES ('no')").is_err());

        let junk = dir.path().join("junk.db");
        for bytes in [
            b"{".as_slice(),
            b"not a database, but long enough".as_slice(),
        ] {
            std::fs::write(&junk, bytes).unwrap();
            assert_eq!(
                open_write(&junk).unwrap_err(),
                format!("{}: file is not a database", junk.display())
            );
            assert!(open_read(&junk)
                .unwrap_err()
                .contains("file is not a database"));
            assert_eq!(std::fs::read(&junk).unwrap(), bytes);
        }
    }

    /// The reused handle serves later writes, follows a replaced file, and
    /// a nested read of the same path still answers.
    #[test]
    fn the_shared_read_handle_sees_new_rows_and_a_replaced_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.db");
        let count = |conn: &Connection| -> i64 {
            conn.query_row("SELECT count(*) FROM t", [], |row| row.get(0))
                .unwrap()
        };
        let writer = open_write(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('a');")
            .unwrap();
        assert_eq!(with_read(&path, count).unwrap(), 1);
        writer.execute("INSERT INTO t VALUES ('b')", []).unwrap();
        assert_eq!(
            with_read(&path, count).unwrap(),
            2,
            "a held handle reads new rows"
        );
        let nested = with_read(&path, |_| with_read(&path, count).unwrap()).unwrap();
        assert_eq!(nested, 2, "a nested read takes its own handle");
        drop(writer);

        let other = dir.path().join("other.db");
        open_write(&other)
            .unwrap()
            .execute_batch("CREATE TABLE t (v TEXT);")
            .unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let from = dir.path().join(format!("other.db{suffix}"));
            let to = dir.path().join(format!("shared.db{suffix}"));
            std::fs::remove_file(&to).ok();
            if from.exists() {
                std::fs::rename(&from, &to).unwrap();
            }
        }
        assert_eq!(
            with_read(&path, count).unwrap(),
            0,
            "a replaced file reopens"
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            with_read(&path, count).is_err(),
            "a gone file never reads from the old handle"
        );
    }

    /// The clean-exit mint: a WAL store whose sidecars are gone (the daemon
    /// exited clean and the -shm left with the -wal) must still read. The
    /// immutable retry fires only where the plain open refuses - which is
    /// platform-shaped: modern SQLite reads a sidecar-less mint plainly,
    /// and the refusing shapes (a -wal left with no -shm) take the retry.
    /// The pin is the reader outcome: the row comes back.
    #[test]
    fn a_sidecar_less_wal_store_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minted.db");
        let writer = open_write(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('row');")
            .unwrap();
        drop(writer);
        // A clean close already checkpoints and removes the sidecars; the
        // header still says WAL. Anything left over is removed by hand so
        // the fixture is the minted shape regardless of close behavior.
        for suffix in ["-wal", "-shm"] {
            let sidecar = path.with_file_name(format!("minted.db{suffix}"));
            std::fs::remove_file(&sidecar).ok();
        }
        assert!(!path.with_file_name("minted.db-wal").exists());
        let reader = open_read(&path).unwrap();
        let value: String = reader
            .query_row("SELECT v FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "row");
    }
}
