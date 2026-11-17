use super::*;

// Hermetic: every child is a stub script in a tempdir, so no test touches
// a real `gh` or `fno`, and every timing case asserts wall-clock bounds
// wide enough to survive parallel test scheduling.

#[test]
fn bounded_run_kills_a_forked_process_group_on_timeout() {
    // A descendant keeps the stderr pipe open past the leader's death;
    // only a process-GROUP kill reaps it, and the run must resolve as a
    // distinct timeout inside the wall-clock budget, never as a hang.
    let tmp = tempfile::tempdir().unwrap();
    let fno = write_exec(
        tmp.path(),
        "fno",
        "#!/bin/sh\n(sleep 30 &) 1>&2\nsleep 30\n",
    );
    let cwd = std::env::temp_dir();
    let started = std::time::Instant::now();
    match run_bounded(
        fno.as_os_str(),
        &[],
        &cwd,
        std::time::Duration::from_millis(200),
    ) {
        BoundedRun::TimedOut(elapsed, _) => {
            assert!(elapsed >= std::time::Duration::from_millis(200));
            // Cleanup slack for spawn + group kill, generous for parallel
            // test scheduling: the point is "bounded", not "precise".
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
        }
        other => panic!("expected TimedOut, got {}", bounded_kind(&other)),
    }
}

/// Render a `BoundedRun` variant name for assertion failures without a
/// Debug derive on the payload-carrying enum.
fn bounded_kind(run: &BoundedRun) -> &'static str {
    match run {
        BoundedRun::Completed(_) => "Completed",
        BoundedRun::TimedOut(..) => "TimedOut",
        BoundedRun::SpawnFailed(_) => "SpawnFailed",
        BoundedRun::WaitFailed => "WaitFailed",
        BoundedRun::Refused => "Refused",
    }
}

/// A bound at the spent line refuses without spawning: enough floored reads
/// would spend the drain's reserved slice, so the transport refuses instead.
#[test]
fn a_bound_at_the_spent_line_refuses_without_spawning() {
    let deadline = std::time::Instant::now();
    crate::loopcheck::stopgate_stamp_fire(0, deadline, 16_000);
    let tmp = tempfile::tempdir().unwrap();
    let run = run_bounded(
        std::ffi::OsStr::new("/bin/sh"),
        &["-c", "echo spawned"],
        tmp.path(),
        crate::loopcheck::read_bounds::STOPGATE_PRE_DRAIN_SPENT_BOUND,
    );
    assert!(
        matches!(run, BoundedRun::Refused),
        "got {}",
        bounded_kind(&run)
    );
}

#[test]
fn no_direct_external_read_bypasses_bounded_runner() {
    // Production region only: test modules may legitimately spawn helper
    // processes. Every child module is production too, so a bypass hiding
    // in a named-by-question child cannot dodge a root-only scan.
    let production = production_source();
    // Positive control first, so an empty scan can never read as green:
    // the centralized runner must exist and carry real call sites.
    assert!(production.contains("fn run_bounded("));
    assert!(
        production.matches("bounded_read(").count() >= 10,
        "the bounded transport must carry its registered read sites"
    );
    assert!(
        production.matches("git_bounded(").count() >= 10,
        "the bounded transport must carry the stop-gate git read sites"
    );
    let bypasses = direct_wait_bypasses(&production);
    assert!(
        bypasses.is_empty(),
        "direct synchronous gh/fno/git waits outside the bounded runner: {bypasses:?}"
    );
}
