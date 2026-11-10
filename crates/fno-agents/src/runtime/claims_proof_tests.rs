//! The attended-human proof tests: ancestry walk and tty seam for
//! `attended_human_proof`. Every test holds `test_env_lock()`. The census
//! and tty guards are `pub(crate)`: the grant-door tests in spawn_promote
//! reuse them instead of carrying a second copy.

use crate::census::ProcRow;
use crate::claims::{attended_human_proof, set_test_stdin_tty, test_env_lock, EnvVarGuard};
use crate::spawn_context::set_test_ancestry_table;
use std::collections::BTreeMap;

pub(crate) fn proof_row(pid: u32, ppid: u32, command: &str) -> (u32, ProcRow) {
    (
        pid,
        ProcRow {
            pid,
            ppid,
            state: 'S',
            elapsed_s: 0,
            cpu_pct: 0.0,
            rss_kb: 0,
            command: command.to_string(),
        },
    )
}

/// Pin a synthetic census whose chain starts at this test process's REAL
/// ppid: the ambient walker starts at `getppid()` and takes no start pid. A
/// `launchd` row at pid 1 ends the walk with ppid 0, so the hidden-census
/// fallback never runs over live processes.
pub(crate) struct CensusGuard;

impl CensusGuard {
    pub(crate) fn chain(rows: &[(u32, u32, &str)]) -> Self {
        let table: BTreeMap<u32, ProcRow> = rows
            .iter()
            .copied()
            .map(|(pid, ppid, cmd)| proof_row(pid, ppid, cmd))
            .collect();
        set_test_ancestry_table(Some(table));
        Self
    }
}

impl Drop for CensusGuard {
    fn drop(&mut self) {
        set_test_ancestry_table(None);
    }
}

/// Clear the tty override on drop, unwind-safe: a panicking test must not
/// leave its answer where a sibling proof test reads it.
pub(crate) struct TtyGuard;

impl Drop for TtyGuard {
    fn drop(&mut self) {
        set_test_stdin_tty(None);
    }
}

#[test]
fn a_terminal_caller_with_no_harness_ancestor_proves_human() {
    let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _pid = EnvVarGuard::unset("FNO_SESSION_PID");
    let _harness = EnvVarGuard::unset("FNO_SESSION_HARNESS");
    let ppid: u32 = unsafe { libc::getppid() } as u32;
    let _table = CensusGuard::chain(&[
        (
            std::process::id(),
            ppid,
            "fno-agents org promote w --scope e-1",
        ),
        (ppid, 1, "-zsh"),
        (1, 0, "launchd"),
    ]);
    let _tty = {
        set_test_stdin_tty(Some(true));
        TtyGuard
    };
    assert_eq!(attended_human_proof(), Ok(()));
    // The plain chain alone is not the proof: the same walk without a
    // terminal refuses on the tty half.
    set_test_stdin_tty(Some(false));
    assert_eq!(attended_human_proof(), Err("stdin is not a terminal"));
}

#[test]
fn a_harness_ancestor_refuses_the_proof_even_with_a_tty() {
    let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let _pid = EnvVarGuard::unset("FNO_SESSION_PID");
    let _harness = EnvVarGuard::unset("FNO_SESSION_HARNESS");
    let ppid: u32 = unsafe { libc::getppid() } as u32;
    let _table = CensusGuard::chain(&[
        (
            std::process::id(),
            ppid,
            "fno-agents org promote w --scope e-1",
        ),
        (ppid, 1, "claude --resume /t/session.jsonl --name lead"),
        (1, 0, "launchd"),
    ]);
    let _tty = {
        set_test_stdin_tty(Some(true));
        TtyGuard
    };
    assert_eq!(
        attended_human_proof(),
        Err("a harness process is in this call's ancestry")
    );
}
