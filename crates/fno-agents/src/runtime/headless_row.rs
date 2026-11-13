//! The headless row lifecycle: the parent lane's two registry stamps around
//! a one-shot child. The parent observes both ends of the run, so it stamps
//! both: [`stamp_spawn`] records the child's pid (the sweep's positive death
//! proof), [`stamp_exit`] writes the observed exit. Both writes are keyed by
//! name, best-effort, and the child's SessionStart report joins the row by
//! the same key, so a repeat of a verifier name grafts onto the older row
//! by design, the report path's own rule.

use crate::paths::AgentsHome;
use crate::state::{self, HeadlessResult, RegistryEntry};
use crate::AgentStatus;

/// Stamp the run's start onto the row `name` names, inserting the row when
/// no row holds the name yet (the child's report has not landed). The child
/// pid plus its start time give the sweep a positive death proof; the
/// substrate stamp is what makes every later arm read the row as one-shot.
pub fn stamp_spawn(home: &AgentsHome, name: &str, harness: &str, cwd: &str, pid: u32) {
    let start = crate::process_probe::process_start_time(pid);
    let updated = state::update_registry(&home.registry_json(), |reg| {
        if let Some(entry) = row_named(&mut reg.entries, name) {
            entry.substrate = Some("headless".to_string());
            entry.pid = Some(pid);
            entry.pid_start_time = start;
            return;
        }
        let mut entry = RegistryEntry::default();
        entry.name = name.to_string();
        entry.harness = Some(harness.to_string());
        entry.substrate = Some("headless".to_string());
        entry.cwd = cwd.to_string();
        entry.status = AgentStatus::Spawning;
        entry.pid = Some(pid);
        entry.pid_start_time = start;
        entry.created_at = crate::daemon::now_rfc3339_like();
        reg.entries.push(entry);
    });
    if let Err(error) = updated {
        eprintln!("headless row stamp skipped (spawn): {error}");
    }
}

/// Stamp the observed exit onto the row `name` names. No row, no write: a
/// run that never showed owes nothing to the registry. The row must read as
/// one-shot (the substrate stamp) or the stamp refuses: a predicate keyed on
/// the row's own identity can never retire an interactive row that happens
/// to share the name. The parent's observed exit outranks a sweep stamp:
/// `FailedToStart` under a real exit was a wrong word, not history.
pub fn stamp_exit(home: &AgentsHome, name: &str, exit_code: Option<i32>) {
    let result = match exit_code {
        Some(0) | None => HeadlessResult::Finished,
        Some(_) => HeadlessResult::Failed,
    };
    let stamped = state::update_registry(&home.registry_json(), |reg| {
        let Some(entry) = row_named(&mut reg.entries, name) else {
            return;
        };
        if !entry.is_headless_once() {
            return;
        }
        entry.status = AgentStatus::Exited;
        entry.result = Some(result);
    });
    if let Err(error) = stamped {
        eprintln!("headless row stamp skipped (exit): {error}");
    }
}

/// The row an exact name (then an alias) names. Spawn names are unique in
/// the registry, so at most one row matches; the first wins the same way
/// `find_row`'s `Named` arm does.
fn row_named<'a>(entries: &'a mut [RegistryEntry], name: &'a str) -> Option<&'a mut RegistryEntry> {
    entries
        .iter_mut()
        .find(|e| e.name == name || e.aliases.iter().any(|a| a == name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AgentsHome;

    fn tmp_home(tag: &str) -> AgentsHome {
        let dir = std::env::temp_dir().join(format!(
            "fno-headless-row-{tag}-{}-{}",
            std::process::id(),
            crate::claims::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        AgentsHome::at(dir)
    }

    fn live_claude_row(name: &str) -> RegistryEntry {
        let mut entry = RegistryEntry::default();
        entry.name = name.to_string();
        entry.harness = Some("claude".to_string());
        entry.status = AgentStatus::Live;
        entry
    }

    #[test]
    fn spawn_stamp_inserts_then_exit_stamp_retires_by_name() {
        let home = tmp_home("roundtrip");
        stamp_spawn(&home, "t-once", "claude", "/w", 42);
        let reg = state::load_registry(&home.registry_json()).unwrap();
        let row = reg.entries.iter().find(|e| e.name == "t-once").unwrap();
        assert_eq!(row.substrate.as_deref(), Some("headless"));
        assert_eq!(row.pid, Some(42));
        assert_eq!(row.status, AgentStatus::Spawning);

        stamp_exit(&home, "t-once", Some(2));
        let reg = state::load_registry(&home.registry_json()).unwrap();
        let row = reg.entries.iter().find(|e| e.name == "t-once").unwrap();
        assert_eq!(row.status, AgentStatus::Exited);
        assert_eq!(row.result, Some(HeadlessResult::Failed));
        assert_eq!(row.exit_code, Some(2));
        std::fs::remove_dir_all(home.root()).ok();
    }

    #[test]
    fn exit_stamp_refuses_a_row_without_the_headless_stamp() {
        let home = tmp_home("interactive-guard");
        {
            let mut reg = state::Registry::default();
            reg.entries.push(live_claude_row("t-live"));
            let json = serde_json::to_string(&reg).unwrap();
            std::fs::write(home.registry_json(), json).unwrap();
        }
        stamp_exit(&home, "t-live", Some(0));
        let reg = state::load_registry(&home.registry_json()).unwrap();
        let row = reg.entries.iter().find(|e| e.name == "t-live").unwrap();
        assert_eq!(row.status, AgentStatus::Live);
        assert_eq!(row.result, None);
        // An unknown name writes nothing at all.
        stamp_exit(&home, "t-never-was", Some(0));
        assert!(state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .iter()
            .all(|e| e.name != "t-never-was"));
        std::fs::remove_dir_all(home.root()).ok();
    }
}
