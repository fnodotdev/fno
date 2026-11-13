# SUMMARY - Pid newtype and one process-FFI module

Node x-57de. Plan: `internal/fno/plans/20261112-pid-newtype-process-ffi-x-57de.md` (quick-plan, route 1 chosen).

## What landed

`crates/fno-agents/src/runtime/pid.rs` is the one process-FFI module: `Pid(NonZeroI32)` admitting 1..=i32::MAX, `SignalResult` (Sent / AlreadyGone / Failed(io::Error)), `Creation`, and typed wrappers for every reader (start_time, started_epoch, created_ms, zombie, parent_pid, cwd, open_file_paths, bsd, bsd_checked, task, thread_handles, thread_info, short_bsd). One `KinfoProcScratch`/`KinfoProcHead` declaration, one start-time reader per OS. `build.rs` copies the body verbatim (minus the tests module) to `crates/fno/src/pid.rs`, so both crates resolve `crate::pid`.

About 110 call sites across both crates now route every kill, probe, killpg and proc_pidinfo/proc_pidpath read through the module. The done-probe grep reads 0 raw sites outside the module. Local const `PROC_PIDLISTTHREADS = 6` covers the constant libc 0.2.191 does not export.

## Deviations from the plan

- Parse-at-entry instead of Pid-taking signatures: public fn signatures are unchanged; each site parses its own raw pid (`Pid::new(..)`, `Pid::try_from(..)`) and keeps its own refuse polarity. This kept the diff mechanical and the wire protocol untouched.
- census on Linux: the parent_pid leg read `ps` by spawn; it now reads `/proc/<pid>/stat` through the module, same field, no subprocess.
- orphans cwd read: strict UTF-8 lossy variance unified to the module's reader.
- gc.rs: an out-of-range pid now reads as not-ours (false). The old cast could wrap negative and reach a kill broadcast form.
- test_run / machine_load / evals group kills: a 0-pid now refuses instead of signalling the caller's own group. The old `kill(0, sig)` broadcast was never an intended effect.
- Review round 1 added `Pid::kill_child_group(&Child)`: 17 call sites repeated `from_child(...).signal_group(SIGKILL)` by hand; the helper is the one correct form to copy. Two sites keep distinct semantics (a plain single-process SIGKILL in the keeper-test Drop, and bounded_spawn's kill-then-reap) and stay explicit.
- `libc::PROC_PIDLISTTHREADS` is not exported by libc 0.2.191; the module carries a local `const PROC_PIDLISTTHREADS: libc::c_int = 6` (stable Darwin ABI value, census's old definition).

## Validation

Local builds compile both crates green (run under the memory-pressure job cap). `cargo test` is left to CI per the fleet-incident law of the day; the pid module's unit tests (AC1-AC5) run there. Gates: `check-file-budget.sh` green (daemon.rs, mux_cli.rs, server.rs all net 0), done-probe grep 0, generated copies verified byte-identical to canonical modulo the generated header and the stripped tests module.

origin/main moved during the run (c7d3477615); per the merge law the PR opens as-is and any conflict resolves by merging main at the GitHub conflict, never a rebase.
