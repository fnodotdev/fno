//! The harness session marker tables and the fno stamp reads: the shared
//! vocabulary of the caller-identity walk. It reaches no other module, so
//! build.rs copies it into fno.

/// Ambient harness session markers, highest precedence first. The shell twin
/// is `detect_provider()` in `hooks/helpers/init-target-state.sh`: the same
/// order, the same non-blank rule.
pub const HARNESS_SESSION_MARKERS: &[(&str, &str)] = &[
    ("CODEX_THREAD_ID", "codex"),
    ("CLAUDE_CODE_SESSION_ID", "claude"),
    ("CODEX_SESSION_ID", "codex"),
    ("GEMINI_SESSION_ID", "gemini"),
    ("OPENCODE_SESSION_ID", "opencode"),
    ("PI_SESSION_ID", "pi"),
];

pub const LEGACY_HARNESS_SESSION_MARKERS: &[(&str, &str)] = &[("CLAUDE_SESSION_ID", "claude")];

pub const FNO_HARNESS_NAME: &str = "FNO_HARNESS_NAME";
pub const FNO_HARNESS_SESSION_ID: &str = "FNO_HARNESS_SESSION_ID";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CanonicalDisposition {
    Absent,
    Invalid,
    NameOnly,
    Complete,
}

pub fn canonical_identity_from(
    get: impl Fn(&str) -> Option<String>,
) -> (Option<String>, Option<String>, CanonicalDisposition) {
    let raw_name = get(FNO_HARNESS_NAME);
    let raw_session = get(FNO_HARNESS_SESSION_ID);
    let name_present = raw_name.is_some();
    let session_present = raw_session.is_some();
    let name = raw_name.as_deref().unwrap_or_default().trim().to_string();
    let session = raw_session
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if !name_present && !session_present {
        return (None, None, CanonicalDisposition::Absent);
    }
    if !name_present || name.is_empty() || (session_present && session.is_empty()) {
        return (None, None, CanonicalDisposition::Invalid);
    }
    if !session_present {
        return (None, Some(name), CanonicalDisposition::NameOnly);
    }
    (Some(session), Some(name), CanonicalDisposition::Complete)
}

pub fn same_session_id(left: &str, right: &str) -> bool {
    if left.starts_with("ses_") || right.starts_with("ses_") {
        left == right
    } else {
        left.eq_ignore_ascii_case(right)
    }
}

/// The registry statuses that hold a live process (idle counts: an
/// idle-but-unreaped process still holds RAM), the one vocabulary every
/// fleet arm reads. The words are `AgentStatus`'s serde names; the
/// caller-identity walk reads registry rows as text, so the list lives here
/// and `AgentStatus::LIVE_STATUSES` points at it.
pub const LIVE_STATUSES: &[&str] = &["spawning", "ready", "idle", "busy", "live", "restarting"];
