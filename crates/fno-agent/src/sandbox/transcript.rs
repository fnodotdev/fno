//! The host copy of one sandbox's transcript: a 0600 file under the agents
//! home's `sandbox-transcripts/`, outside every harness store. The registry
//! names it only in `sandbox_transcript`, which display readers opt in to;
//! `transcript_path`, whose readers trust user turns, is never set for a
//! sandbox. Bytes append only at the expected offset, so a replayed or
//! out-of-order frame cannot rewrite what is there, and the file stops at a
//! cap with a marker line that says so.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The most one sandbox transcript holds on the host.
pub const CAP: u64 = 64 * 1024 * 1024;
/// The line written once at the cap.
pub const MARKER: &str = "{\"type\":\"fno-sandbox-truncated\",\"reason\":\"the sandbox transcript reached its size cap\"}\n";

pub struct Store {
    file: File,
    path: PathBuf,
    len: u64,
    cap: u64,
    capped: bool,
}

impl Store {
    /// Create the file for sandbox `id`. An existing file is an error: one id
    /// is one sandbox.
    pub fn create(dir: &Path, id: &str, cap: u64) -> std::io::Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        if !super::valid_id(id) {
            return Err(std::io::Error::other(format!(
                "invalid sandbox id {}",
                super::escape(id, 80)
            )));
        }
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let path = dir.join(format!("{id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            file,
            path,
            len: 0,
            cap,
            capped: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn capped(&self) -> bool {
        self.capped
    }

    /// Append `bytes` that the guest says start at `offset`. Anything but
    /// the next offset is dropped. Past the cap the rest is dropped after one
    /// marker line.
    pub fn append(&mut self, offset: u64, bytes: &[u8]) -> std::io::Result<()> {
        if self.capped || offset != self.len || bytes.is_empty() {
            return Ok(());
        }
        let room = self.cap.saturating_sub(self.len);
        if (bytes.len() as u64) <= room {
            self.file.write_all(bytes)?;
            self.len += bytes.len() as u64;
            return Ok(());
        }
        self.file
            .write_all(bytes.get(..room as usize).unwrap_or(bytes))?;
        self.len += room;
        // The marker starts a line of its own even when the cut fell mid-line.
        self.file.write_all(b"\n")?;
        self.file.write_all(MARKER.as_bytes())?;
        self.capped = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes match byte for byte across chunk edges and a long line; a
    /// replayed or skipped offset changes nothing; the cap writes one marker.
    #[test]
    fn store_appends_in_order_and_marks_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::create(dir.path(), "w-1", 100 * 1024).unwrap();
        let long = format!("{{\"line\":\"{}\"}}\n", "x".repeat(70 * 1024));
        let (a, b) = long.as_bytes().split_at(65536);
        s.append(0, a).unwrap();
        s.append(0, b"forged").unwrap();
        s.append(9, b"skip").unwrap();
        s.append(65536, b).unwrap();
        assert_eq!(std::fs::read(s.path()).unwrap(), long.as_bytes());
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let at = long.len() as u64;
        s.append(at, &vec![b'y'; 64 * 1024]).unwrap();
        assert!(s.capped());
        let body = std::fs::read_to_string(s.path()).unwrap();
        assert!(body.ends_with(MARKER) && body.matches("fno-sandbox-truncated").count() == 1);
        assert!(
            Store::create(dir.path(), "w-1", 1).is_err(),
            "one id, one file"
        );
        assert!(Store::create(dir.path(), "../x", 1).is_err());
    }
}
