//! The zombie read: one delegate to the pid module, whose macOS arm pins
//! the `p_stat` byte through the crate pair's ONE KinfoProcScratch
//! declaration and whose Linux arm reads the state letter after the last
//! `)` of `/proc/<pid>/stat`. An unreadable pid reads not-zombie: this
//! helper never invents a death.

use crate::pid::Pid;

/// True while `pid` is a zombie: dead but not yet reaped by its parent, so
/// a probe keeps succeeding even though it holds no fds and serves nothing.
pub fn pid_is_zombie(pid: i32) -> bool {
    Pid::try_from(i64::from(pid))
        .map(|p| p.zombie())
        .unwrap_or(false)
}
