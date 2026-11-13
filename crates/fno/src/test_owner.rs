//! The test-owner identity the fno-agents-worker watchdog reads from
//! FNO_TEST_OWNER_PID/FNO_TEST_OWNER_BIRTH: a test-spawned server
//! passes it to every keeper it launches, so the keeper reaps when the test
//! run ends instead of orphaning as a ppid-1 pane keeper for hours.
//!
//! This is the boundary-forced twin of fno-agents' `test_run::self_owner_env`
//! + `daemon::process_start_time`: the product boundary (the mux never links
//! the runtime, not even as a dev dependency - proven by
//! `the_mux_never_links_the_runtime`) forbids calling the original, so this
//! twin must stay behavior-identical to it. The UNITS are load-bearing: the
//! worker compares this value against its own `process_start_time` read, and
//! a mismatch reads as a dead owner that reaps the pane instantly.
//!
//! Shared by two compilation units through one source: the lib's cfg(test)
//! build (`src/server_tests.rs`) and the integration harness
//! (`tests/common/mod.rs` includes it by `#[path]`).

/// Microseconds-since-epoch on macOS, clock-ticks-since-boot on Linux -
/// the pid module's start_time read, shared verbatim with fno-agents
/// through the build.rs copy. The boundary-forced twin body this used to
/// carry deleted: the mux never links the runtime, but the generated
/// `crate::pid` copy is the crate's own module, so one body serves both
/// crates again.
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    crate::pid::Pid::new(pid).and_then(|p| p.start_time())
}

/// The current test process's identity in the env shape every spawned
/// server, client, and keeper inherits.
pub(crate) fn self_owner_env() -> [(&'static str, String); 2] {
    let pid = std::process::id();
    let birth =
        process_start_time(pid).expect("the current test process must have a readable birth time");
    [
        ("FNO_TEST_OWNER_PID", pid.to_string()),
        ("FNO_TEST_OWNER_BIRTH", birth.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::process_start_time;

    #[test]
    fn reads_a_live_pids_birth_and_none_for_a_bare_pid() {
        // Positive control: THIS process is introspectable on macOS/Linux.
        let own = process_start_time(std::process::id());
        assert!(own.is_some(), "own birth time must read");
        // pid 4 (launchd/init sibling slot, never us) is absent or refused on
        // both platforms; either way it must not fabricate a birth.
        let low = process_start_time(4);
        assert_ne!(low, own, "a foreign low pid must not read as our birth");
    }
}
