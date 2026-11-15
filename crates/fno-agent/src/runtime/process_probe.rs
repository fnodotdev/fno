//! Per-process liveness probes: the start-time token and the zombie flag.
//! One read per platform; consumers compare the token only for equality
//! and read the zombie flag to prune lock holders that cannot hold a
//! flock anymore.

use crate::pid::Pid;

/// The (start-time token, zombie flag) pair on the module's own units:
/// start via the one start_time body, zombie via the one zombie body. A
/// zombie reads None on macOS (its BSD read is a zero write) and Some on
/// Linux, exactly as the split bodies did.
pub fn process_bsd(pid: u32) -> Option<(u64, bool)> {
    let pid = Pid::new(pid)?;
    Some((pid.start_time()?, pid.zombie()))
}

/// The start-time token [`process_bsd`] reads: per-host, per-boot, compared
/// only for equality (never a wall clock - see that function's contract).
pub fn process_start_time(pid: u32) -> Option<u64> {
    Pid::new(pid).and_then(|p| p.start_time())
}

/// A process's start as epoch seconds, the reading psutil's `create_time`
/// gives, so it compares against start times Python wrote. This is its own
/// field, not the [`process_start_time`] token: macOS's BSD start is already
/// epoch microseconds, Linux's ticks since boot need the boot time added.
pub fn process_started_epoch(pid: u32) -> Option<f64> {
    Pid::new(pid).and_then(|p| p.started_epoch())
}
