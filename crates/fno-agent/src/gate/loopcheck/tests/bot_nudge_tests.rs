use super::*;

#[test]
fn nudge_post_is_suppressed_by_the_escape_hatch() {
    // The test suite must never comment on a real PR. With the guard set,
    // post_nudge_comment returns false without spawning gh (a bogus bin here
    // would otherwise error, not silently succeed).
    std::env::set_var("FNO_LOOPCHECK_NO_COMMENT", "1");
    let posted = post_nudge_comment(
        "/nonexistent/gh",
        std::path::Path::new("/tmp"),
        618,
        "@codex review",
    );
    std::env::remove_var("FNO_LOOPCHECK_NO_COMMENT");
    assert!(!posted);
}
