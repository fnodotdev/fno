//! What the OS said when asked for a pid's create time, and whether that
//! answer proves a claim holder is running. Named by the question it
//! answers; agents_view.rs is over the shrink-only budget, so code that
//! lands here never grows it. Focused copy of `claims.rs::PidProbe`.

/// `Refused` is its own arm because the holder EXISTS when inspection is
/// denied - permission cannot be denied on a pid that is gone - so a
/// refusal must never read as death.
use crate::pid::{Creation, Pid};

/// `Refused` is its own arm because the holder EXISTS when inspection is
/// denied - permission cannot be denied on a pid that is gone - so a
/// refusal must never read as death.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PidProbe {
    Created(i64),
    Absent,
    Refused,
}

pub(super) fn probe_pid(pid: i32) -> PidProbe {
    if pid <= 0 {
        return PidProbe::Absent;
    }
    let Some(pid) = Pid::try_from(i64::from(pid)).ok() else {
        return PidProbe::Absent;
    };
    match pid.created_ms() {
        Creation::Created(ms) => PidProbe::Created(ms),
        Creation::Refused => PidProbe::Refused,
        Creation::Absent => PidProbe::Absent,
    }
}

/// Refused still reads live: the holder provably exists, and the events
/// recency window bounds what a badge may claim.
pub(super) fn probe_is_live(probe: PidProbe, acquired_at: i64) -> bool {
    match probe {
        PidProbe::Created(create_ms) => create_ms <= acquired_at,
        PidProbe::Refused => true,
        PidProbe::Absent => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{probe_is_live, PidProbe};

    #[test]
    fn probe_is_live_refused_reads_live_absent_dead() {
        // inspection-refused is not pid-absent. A refusal proves the
        // pid EXISTS, so it must never read as death; only a real pid-reuse
        // (started after acquired_at) or a genuine absence reads dead. Driven
        // through the pure seam because no portable test pid refuses
        // inspection.
        assert!(probe_is_live(PidProbe::Refused, 0));
        assert!(probe_is_live(PidProbe::Created(100), 100));
        assert!(!probe_is_live(PidProbe::Created(101), 100));
        assert!(!probe_is_live(PidProbe::Absent, i64::MAX));
    }
}
