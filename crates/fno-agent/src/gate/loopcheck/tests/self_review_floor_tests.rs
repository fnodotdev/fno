use super::super::self_review_floor::classify_payload;
use super::*;

#[test]
fn self_review_gate_fails_closed_when_main_resolves_but_diffs_fail() {
    // origin/main RESOLVES but its diff fails (unrelated histories exit
    // 128 "no merge base"): the fallback must not quietly re-base onto a
    // sibling origin/master, which can be a stale pre-migration ancestor
    // and would size the payload from a whole era. Fail closed instead.
    let dir = tempfile::tempdir().unwrap();
    let script = write_exec(
            dir.path(),
            "git",
            "#!/bin/sh\ncase \"$*\" in\n  rev-parse*origin/main*) printf 'sha\\n' ;;\n  *origin/main*) exit 1 ;;\n  rev-parse*origin/master*) printf 'sha\\n' ;;\n  *origin/master*) printf 'src/lib.rs\\n' ;;\n  *) exit 1 ;;\nesac\n",
        );
    let (is_code, assumed) = classify_payload(script.to_str().unwrap(), Path::new("."));
    assert!(is_code);
    assert!(assumed);
}

#[test]
fn unresolved_harness_floors_the_self_review_gate() {
    // `None` means UNATTRIBUTABLE, not commandless. A claude session
    // started from a codex shell resolves no single family, and reading
    // that ambiguity as "no floor" silently disengaged the only review a
    // stock install demands. Ambiguity is not permission; the explicit
    // `--author-harness none` pin stays the hermetic opt-out.
    assert!(self_review_floor_applies(None, false));
    assert!(!self_review_floor_applies(None, true));
    assert!(self_review_floor_applies(Some("claude"), false));
    assert!(self_review_floor_applies(Some("codex"), true));
    // The owned fno review lane runs wherever the plugin runs, so no
    // KNOWN harness is commandless any more: gemini/agy/opencode floor too.
    assert!(self_review_floor_applies(Some("opencode"), false));
    assert!(self_review_floor_applies(Some("gemini"), false));
    assert!(self_review_floor_applies(Some("agy"), false));
    // An unrecognized spelling is an unattributed run: it floors rather
    // than passing an unknown name through the command table.
    assert!(self_review_floor_applies(Some("hermes"), false));
}
