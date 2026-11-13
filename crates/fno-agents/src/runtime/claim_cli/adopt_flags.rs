//! The `claim adopt-flags` leaf: the init helper's adopt decision, printed as
//! its `--handover-from` flags. One own-session check shared with the start
//! pre-check ([`crate::target_start::claim_names_current_session`]), so a
//! claim one surface adopts the other cannot refuse. Prints nothing when no
//! rung passes, so the caller's ordinary acquire decides. Always exit 0.

pub fn run(args: &[String]) -> i32 {
    let mut key: Option<&str> = None;
    for arg in args.iter() {
        match arg.as_str() {
            "--" => {}
            other => {
                if other.starts_with('-') {
                    eprintln!("fno-agents: claim adopt-flags: unknown flag {other}");
                    return 2;
                }
                if key.is_some() {
                    eprintln!("fno-agents: claim adopt-flags: one key only");
                    return 2;
                }
                key = Some(other);
            }
        }
    }
    let Some(key) = key else {
        eprintln!("fno-agents: claim adopt-flags: one claim key required, e.g. node:sample-node");
        return 2;
    };
    let (state, rec) = crate::claims::status(key, None);
    // A free/corrupted row has nothing to adopt; the caller's ordinary
    // acquire decides. The start pre-check maps corrupted to free the same
    // way, so both surfaces read one classification.
    if matches!(
        state,
        crate::claims::ClaimState::Free | crate::claims::ClaimState::Corrupted
    ) {
        return 0;
    }
    let Some(rec) = rec else {
        return 0;
    };
    if crate::target_start::claim_names_current_session(&rec.holder, &rec) {
        println!("--handover-from {}", rec.holder);
    }
    0
}
