//! One name-collision verdict for every spawn door.
//!
//! Claude's background daemon keeps a pool of pre-warmed spares, and a taken
//! session keeps the spare argv for good; the claim socket on disk is the
//! only idle signal (`footprint_cause`). A registry row whose pid has gone
//! back to the pool holds no work, yet its name refused every spawn for as
//! long as the pool kept the process alive (one idle spare held a lead's
//! name for over eleven hours). The verdict reclaims exactly that row and
//! refuses every other holder, naming the holder pid and the way out.

use std::collections::BTreeSet;

use crate::paths::AgentsHome;
use crate::runtime::footprint_cause::idle_spare_process;
use crate::runtime::process_probe::process_start_time;
use crate::runtime::state::{self, Registry, RegistryEntry};
use crate::spawn::claude_ask::{emit_event, py_repr};

/// The spawn-door decision for one name.
#[derive(Debug)]
pub enum Resolution {
    /// No row carries the name, or the sole holder was an idle spare and the
    /// row was reclaimed: the spawn proceeds.
    Clear,
    /// The sole holder was an idle spare and the row was reclaimed.
    Reclaimed { pid: u32 },
    /// A holder that is not an idle spare owns the name; the line carries the
    /// holder pid and the way out.
    Refuse(String),
    /// The registry could not be read or written; the line is the failure.
    Registry(String),
}

/// The row's pid when that pid currently serves claude's idle spare pool:
/// pool-shaped argv with the claim socket still on disk, so nothing is being
/// served. A recorded start time must still match the live process, or the
/// pid was recycled and the argv describes a stranger.
pub fn idle_spare_holder_pid(entry: &RegistryEntry) -> Option<u32> {
    let pid = entry.pid?;
    if let Some(recorded) = entry.pid_start_time {
        if process_start_time(pid) != Some(recorded) {
            return None;
        }
    }
    idle_spare_process(pid).then_some(pid)
}

/// The refusal for a holder that is not an idle spare. `already exists` stays
/// in the line: the advance and think dispatchers match on it to read the
/// refusal as a benign already-running dedup. The pidless form is byte-kept:
/// rows without a pid (ask rows, seeded rows) refused exactly this before.
pub fn refusal_line(name: &str, row: &RegistryEntry) -> String {
    match row.pid {
        Some(pid) => format!(
            "agent {} already exists (holder pid {pid}); use 'fno agents rm {}' first or pick another name",
            py_repr(name),
            name
        ),
        None => format!(
            "agent {} already exists; use 'fno agents rm {}' first or pick another name",
            py_repr(name),
            name
        ),
    }
}

/// The spawn-door decision. Call under the lane's per-agent lock with the
/// lane's already-loaded registry view. The reclaim re-verifies the holder
/// under the registry write lock, so a spare claimed between the read and the
/// write still refuses on the fresh state.
pub fn resolve(home: &AgentsHome, registry: &Registry, name: &str) -> Resolution {
    let Some(row) = registry.find(name) else {
        return Resolution::Clear;
    };
    let Some(pid) = idle_spare_holder_pid(row) else {
        return Resolution::Refuse(refusal_line(name, row));
    };
    let stale = row.clone();
    let registry_path = home.registry_json();
    let retiring = BTreeSet::from([name.to_string()]);
    let dropped = state::update_registry_retiring(&registry_path, &retiring, |reg| {
        let before = reg.entries.len();
        reg.entries
            .retain(|e| !(e.name == name && idle_spare_holder_pid(e).is_some()));
        before - reg.entries.len()
    });
    let dropped = match dropped {
        Ok(dropped) => dropped,
        Err(e) => return Resolution::Registry(format!("registry reclaim write failed: {e}")),
    };
    if dropped == 0 {
        // The spare was claimed (or the row replaced) between the read and
        // the write. Classify on the fresh state instead of proceeding on a
        // verdict that no longer describes the world.
        let fresh = match state::load_registry(&registry_path) {
            Ok(r) => r,
            Err(e) => return Resolution::Registry(format!("registry read failed: {e}")),
        };
        return match fresh.find(name) {
            Some(row) => Resolution::Refuse(refusal_line(name, row)),
            None => Resolution::Clear,
        };
    }
    // The tombstone lands after the removal persists: a stamp written before
    // the write would mark a live session removed if the write was refused.
    // A resolver that holds the pre-write row sees row-gone briefly, and the
    // tombstone heals any retry after it.
    if let Some(session_id) = stale
        .harness_session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        let _ = crate::rm_tombstone::record(
            home,
            stale.harness_name(),
            session_id,
            stale.short_id.as_str(),
            &stale.name,
            &stale.cwd,
            stale.host_mode.as_deref().unwrap_or_default(),
        );
        let _ = crate::wake_name::stamp_from_row(home, &stale);
    }
    emit_event(
        &home.events_jsonl(),
        "agent_name_reclaimed",
        &[("name", name.into()), ("pid", pid.into())],
    );
    Resolution::Reclaimed { pid }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::test_spare::{fake_spare_process as spawn_fake_spare, spare_row};
    use std::process::Child;

    #[cfg(unix)]
    fn spawn_plain_sleeper() -> Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    }

    fn seed(home: &AgentsHome, row: RegistryEntry) {
        state::update_registry(&home.registry_json(), |reg| reg.entries.push(row)).unwrap();
    }

    fn read_back(home: &AgentsHome, name: &str) -> bool {
        state::load_registry(&home.registry_json())
            .unwrap()
            .find(name)
            .is_some()
    }

    #[cfg(unix)]
    #[test]
    fn spawning_over_an_idle_spare_name_reclaims_the_row() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let sock = temp.path().join("pool").join("s.claim.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        std::fs::write(&sock, b"").unwrap();
        let mut spare = spawn_fake_spare(&sock);
        seed(&home, spare_row("t-idle-lead", spare.id()));
        let registry = state::load_registry(&home.registry_json()).unwrap();
        let resolution = resolve(&home, &registry, "t-idle-lead");
        match resolution {
            Resolution::Reclaimed { pid } => assert_eq!(pid, spare.id()),
            other => panic!("expected a reclaim, got {other:?}"),
        }
        assert!(!read_back(&home, "t-idle-lead"), "the row must be gone");
        // The next spawn over the same name sees no holder at all.
        let registry = state::load_registry(&home.registry_json()).unwrap();
        assert!(matches!(
            resolve(&home, &registry, "t-idle-lead"),
            Resolution::Clear
        ));
        spare.kill().unwrap();
        spare.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_live_working_holder_refuses_with_pid_and_the_way_out() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let mut worker = spawn_plain_sleeper();
        seed(&home, spare_row("t-live-lead", worker.id()));
        let registry = state::load_registry(&home.registry_json()).unwrap();
        let resolution = resolve(&home, &registry, "t-live-lead");
        let Resolution::Refuse(line) = resolution else {
            panic!("a non-spare holder must refuse, got {resolution:?}")
        };
        assert!(line.contains("already exists"), "{line}");
        assert!(
            line.contains(&format!("holder pid {}", worker.id())),
            "{line}"
        );
        assert!(line.contains("fno agents rm t-live-lead"), "{line}");
        assert!(read_back(&home, "t-live-lead"), "the refusal keeps the row");
        worker.kill().unwrap();
        worker.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_claimed_spare_still_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let sock = temp.path().join("pool").join("c.claim.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        std::fs::write(&sock, b"").unwrap();
        let mut spare = spawn_fake_spare(&sock);
        // Claude removes the claim socket when a session takes the spare.
        std::fs::remove_file(&sock).unwrap();
        seed(&home, spare_row("t-claimed", spare.id()));
        let registry = state::load_registry(&home.registry_json()).unwrap();
        let resolution = resolve(&home, &registry, "t-claimed");
        assert!(
            matches!(resolution, Resolution::Refuse(_)),
            "a claimed spare is a working session, got {resolution:?}"
        );
        assert!(read_back(&home, "t-claimed"));
        spare.kill().unwrap();
        spare.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_recycled_pid_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let sock = temp.path().join("pool").join("r.claim.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        std::fs::write(&sock, b"").unwrap();
        let mut spare = spawn_fake_spare(&sock);
        let mut row = spare_row("t-recycled", spare.id());
        // A start-time mismatch means the pid now belongs to a stranger.
        row.pid_start_time = row.pid_start_time.map(|t| t.wrapping_add(1));
        seed(&home, row);
        let registry = state::load_registry(&home.registry_json()).unwrap();
        let resolution = resolve(&home, &registry, "t-recycled");
        assert!(
            matches!(resolution, Resolution::Refuse(_)),
            "a recycled pid is not the recorded holder, got {resolution:?}"
        );
        spare.kill().unwrap();
        spare.wait().unwrap();
    }

    #[test]
    fn a_pidless_row_refuses_with_the_untouched_line() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let mut row = RegistryEntry::default();
        row.name = "t-pidless".into();
        // The store requires a resolvable handle: pidless rows (the claude
        // --cloud shape) carry a session id.
        row.harness = Some("claude".into());
        row.harness_session_id = Some("1e133a42-5b04-4c05-9df1-2c7ae1a4f6d0".into());
        row.cwd = "/tmp".into();
        row.created_at = "2026-11-12T00:00:00Z".into();
        seed(&home, row);
        let registry = state::load_registry(&home.registry_json()).unwrap();
        let Resolution::Refuse(line) = resolve(&home, &registry, "t-pidless") else {
            panic!("a pidless row must refuse")
        };
        assert_eq!(
            line,
            "agent 't-pidless' already exists; use 'fno agents rm t-pidless' first or pick another name"
        );
    }
}
