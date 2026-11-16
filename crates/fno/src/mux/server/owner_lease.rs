//! The owner lease. Moved out of server.rs under the shrink-only rule.

use super::OwnerLease;

impl OwnerLease {
    /// The lease from the spawner's env, when complete.
    pub(super) fn from_env() -> Option<Self> {
        let pid = std::env::var("FNO_OWNER_PID").ok()?.parse::<u32>().ok()?;
        let birth = std::env::var("FNO_OWNER_BIRTH")
            .ok()?
            .parse::<u64>()
            .ok()
            .filter(|birth| *birth > 0)?;
        let session = std::env::var("FNO_OWNER_SESSION").ok().filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })?;
        (pid > 1).then_some(Self {
            pid,
            birth,
            session,
        })
    }

    /// The spawner still lives: the pid exists with the same birth.
    pub(super) fn alive(&self) -> bool {
        crate::proto::pid_start_time(self.pid) == Some(self.birth)
    }
}
