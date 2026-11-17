//! The methods a paired device may call: fleet reads only (rulings D2503,
//! D2524). Every write (spawn, ask, stop, rm) stays local. The door answers
//! anything else itself and never opens the supervisor socket for it.

pub const ALLOWED: &[&str] = &["agent.list", "agent.watch", "agent.status", "agent.truth"];

pub fn allowed(method: &str) -> bool {
    ALLOWED.contains(&method)
}
