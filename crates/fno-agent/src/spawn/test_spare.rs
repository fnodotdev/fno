//! The idle-spare test fixture, shared by the door tests that run the spare
//! verdict through their own lane: a live process wearing claude's spare-pool
//! argv with its claim socket still on disk. The argv + socket pair is the
//! only idle signal there is, so every fixture process here must stay alive
//! for the duration of its test.

use crate::runtime::process_probe::process_start_time;
use crate::runtime::state::RegistryEntry;
use std::path::Path;
use std::process::Child;

/// A process wearing claude's idle-spare argv (pool shape, claim socket
/// still on disk). The caller kills it when the test ends.
pub(crate) fn fake_spare_process(sock: &Path) -> Child {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    cmd.arg0(format!("claude bg-spare --bg-spare {}", sock.display()));
    cmd.spawn().expect("fake spare spawns")
}

/// The registry row for the fake spare: the pid recorded with its start
/// time, so a recycled pid never reads as the recorded holder.
pub(crate) fn spare_row(name: &str, pid: u32) -> RegistryEntry {
    let mut row = RegistryEntry::default();
    row.name = name.into();
    row.harness = Some("claude".into());
    row.cwd = "/tmp".into();
    row.created_at = "2026-11-12T00:00:00Z".into();
    row.pid = Some(pid);
    row.pid_start_time = process_start_time(pid);
    row
}
