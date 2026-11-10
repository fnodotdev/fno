//! The new-agent composer's unit suite : editor semantics, focus
//! order, submit refusals, update correlation, and the render table. The
//! subprocess-boundary journeys live in the integration suite
//! (`tests/agent_launcher_journey.rs`).

use super::agent_launcher::card::{card_hit, CardHit};
use super::agent_launcher::{
    apply_launch_update, close, open, open_in, CatalogOutcome, Focus, HarnessChoice, LauncherEsc,
    Phase, ProjectFacts,
};
use super::*;
use crate::model_catalog::ModelState;
use crate::proto::agent_launch::{AgentLaunchUpdate, LaunchState};

struct WireVersionFixture(std::path::PathBuf);

impl Drop for WireVersionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wire_fixture_at(version: u32) -> (String, WireVersionFixture) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let session = format!("x50ed-wire-fixture-{}-{nonce}", std::process::id());
    let socket = crate::proto::socket_path(&session).unwrap();
    let sidecar = crate::proto::version_sidecar_path(&socket);
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, format!("{version}\n")).unwrap();
    (session, WireVersionFixture(sidecar))
}

fn plain_view() -> View {
    let mut v = two_pane_view();
    v.term = (24, 80);
    v
}

fn view_with_launcher() -> View {
    let mut v = plain_view();
    // The draft starts in its workspace's folder; the catalog fixtures key
    // their project facts on the process cwd, so the active workspace sits
    // there.
    let own = std::env::current_dir().unwrap().display().to_string();
    let active = v.layout.active_squad;
    for s in v.layout.squads.iter_mut().filter(|s| s.id == active) {
        s.canonical_cwd = own.clone();
    }
    open(&mut v);
    v
}

fn catalog(names: &[(&str, bool, bool)]) -> Option<CatalogOutcome> {
    Some(CatalogOutcome::Ok(
        names
            .iter()
            .map(|(n, native, installed)| HarnessChoice {
                name: n.to_string(),
                native: *native,
                installed: *installed,
                models: Vec::new(),
                more: Vec::new(),
                catalog_error: None,
                models_error: None,
                // Free-text surface by default: the effort chip stays
                // offered in tests that do not name a list.
                efforts: Some(Vec::new()),
                permission_modes: Some(Vec::new()),
                launch_flags: None,
            })
            .collect(),
        None,
        Vec::new(),
    ))
}

fn sync_catalog(v: &mut View) {
    // Mirror the run loop's rx arm: land the catalog and sync the draft's
    // harness names through the same seam the client calls.
    if let Some(l) = v.launcher.as_mut() {
        super::agent_launcher::sync_harness_names(l, &v.launcher_catalog);
    }
}

fn type_message(v: &mut View, text: &str) {
    // Focus the message field from a fresh dock: Harness -> Project ->
    // Message.
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    let mut esc = LauncherEsc::default();
    let keys = esc.fold(text.as_bytes());
    v.launcher_esc = esc;
    let _ = keys;
    // Route the same bytes through the real key folder so editor state and
    // folding agree.
    let bytes = text.as_bytes().to_vec();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(v, &bytes, &mut sock).await;
    });
}
#[allow(clippy::indexing_slicing)]
#[test]
fn dock_lifecycle_rows() {
    // The unit twin of the pty repro: the scanner flushes [0x1b] into
    // launcher_keys after its quiet window, and the dock's own carry must
    // release a trailing lone ESC as a bare Esc press, not re-buffer it.
    let mut v = view_with_launcher();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(v.launcher.is_none(), "one lone Esc byte closes the dock");
    assert!(
        !v.launcher_closed.is_empty(),
        "the draft is retained for reopen"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "ship it");
    let revision_before = v.launcher.as_ref().unwrap().draft.revision;
    close(&mut v);
    assert!(v.launcher.is_none(), "dock hidden");
    assert!(!v.launcher_closed.is_empty(), "draft retained");
    // Reopen: same draft, same revision, nothing lost.
    open(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "ship it");
    assert_eq!(l.draft.revision, revision_before);
    assert_eq!(l.draft.harnesses, vec!["claude", "codex"]);

    let mut v = view_with_launcher();
    v.sideline_full = true;
    type_message(&mut v, "keep me");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b ", &mut sock).await;
    });
    assert!(!v.sideline_full, "Esc leaves full-screen sideline");
    assert!(v.launcher.is_none(), "composer closed");
    assert_eq!(
        v.launcher_closed[&1].draft.message, "keep me",
        "draft retained"
    );
}

#[test]
fn arrow_reaches_the_dock_whole_or_scanner_rejoined() {
    // AC1-EDGE: an arrow in one chunk, or as the scanner-rejoined pair a
    // lone-Esc read plus a follow-up, moves the cursor / focus and never
    // reads as Esc.
    let mut v = view_with_launcher();
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    type_message(&mut v, "ab");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[A", &mut sock).await;
    });
    // Whole-chunk arrow: the cursor moves inside the editor, no close.
    assert!(v.launcher.is_some(), "an arrow never closes the dock");
    // The scanner rejoins an arrow into ONE chunk, so the fold sees
    // `\x1b` + `[A` in a single read and folds Up, never Esc.
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b[A"),
        vec![super::agent_launcher::LKey::Up],
        "a rejoined arrow folds Up, not Esc"
    );
}
#[test]
fn paste_rows() {
    let mut v = view_with_launcher();
    type_message(&mut v, "");
    // A paste whose payload looks like an arrow sequence must land as text.
    let payload = b"\x1b[200~\x1b[Ahello\x1b[201~";
    type_message(&mut v, std::str::from_utf8(payload).unwrap());
    let l = v.launcher.as_ref().unwrap();
    assert!(
        l.draft.message.contains("\x1b[Ahello"),
        "pasted bytes are data: {}",
        l.draft.message
    );

    let mut esc = LauncherEsc::default();
    // The emoji U+1F600 is four bytes; split after the first.
    let full = "\u{1f600}".as_bytes();
    let first = esc.fold(&full[..1]);
    assert!(first.is_empty(), "an incomplete sequence waits");
    let rest = esc.fold(&full[1..]);
    assert_eq!(rest, vec![super::agent_launcher::LKey::Char('\u{1f600}')]);
}

#[test]
fn the_line_edit_grammar_folds_and_edits() {
    // The Cmd line-edit grammar, three sections: each byte spelling folds to
    // exactly one key, never Esc-then-something; the keys edit the draft
    // (word motion, Home/End, word kill); and a pasted path lands once, at
    // the cursor, even when the paste arrives split across reads.
    let cases: &[(&[u8], super::agent_launcher::LKey)] = &[
        (b"\x1b\x7f", super::agent_launcher::LKey::KillWord),
        (b"\x15", super::agent_launcher::LKey::KillLeft),
        (b"\x1bb", super::agent_launcher::LKey::WordLeft),
        (b"\x1bf", super::agent_launcher::LKey::WordRight),
        (b"\x1b[1;3D", super::agent_launcher::LKey::WordLeft),
        (b"\x1b[1;3C", super::agent_launcher::LKey::WordRight),
        (b"\x1b[1;5D", super::agent_launcher::LKey::Left),
        (b"\x1b[D", super::agent_launcher::LKey::Left),
        (b"\x1b[H", super::agent_launcher::LKey::Home),
        (b"\x1b[F", super::agent_launcher::LKey::End),
        (b"\x1bOH", super::agent_launcher::LKey::Home),
        (b"\x1bOF", super::agent_launcher::LKey::End),
        (b"\x1b[127;3u", super::agent_launcher::LKey::KillWord),
    ];
    for (bytes, want) in cases {
        let mut esc = LauncherEsc::default();
        assert_eq!(esc.fold(bytes), vec![want.clone()], "{bytes:?}");
    }

    let mut v = view_with_launcher();
    type_message(&mut v, "launch the /home/alice/x.png thing");
    let cur = |v: &View| v.launcher.as_ref().unwrap().draft.cursor_chars;
    let text = |v: &View| v.launcher.as_ref().unwrap().draft.message.clone();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Option+Left (ESC b) from the end lands on `thing`'s first char.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1bb", &mut sock).await;
    });
    assert_eq!(cur(&v), 29, "one word left");
    // Cmd+Left/Right spell as Home/End.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[H", &mut sock).await;
    });
    assert_eq!(cur(&v), 0, "Home jumps to the row start");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[F", &mut sock).await;
    });
    assert_eq!(cur(&v), 34, "End jumps to the row end");
    // Two Option+Right from the row start land after `the`; the word kill
    // takes `the ` in one press.
    rt.block_on(async {
        let _ =
            super::agent_launcher::launcher_keys(&mut v, b"\x1b[H\x1bf\x1bf\x1b\x7f", &mut sock)
                .await;
    });
    assert_eq!(cur(&v), 6, "cursor after the kill");
    assert_eq!(
        text(&v),
        "launch /home/alice/x.png thing",
        "the word and its trailing space are gone"
    );
    // Word motion is row-aware through a newline.
    v.launcher.as_mut().unwrap().draft.message.clear();
    v.launcher.as_mut().unwrap().draft.cursor_chars = 0;
    type_message(&mut v, "ab\ncd");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[H", &mut sock).await;
    });
    assert_eq!(cur(&v), 3, "Home lands on the second row's head");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1bb", &mut sock).await;
    });
    assert_eq!(cur(&v), 0, "word left is flat: over the newline onto `ab`");

    // The paste section: a fresh draft, cursor parked mid-word.
    v.launcher.as_mut().unwrap().draft.message.clear();
    v.launcher.as_mut().unwrap().draft.cursor_chars = 0;
    type_message(&mut v, "claude-code-claude-code.png");
    type_message(&mut v, "\x1b[D\x1b[D");
    let mid = "claude-code-claude-code.png".chars().count() - 2;
    assert_eq!(v.launcher.as_ref().unwrap().draft.cursor_chars, mid);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(
            &mut v,
            b"\x1b[200~/home/alice/Pictures/x",
            &mut sock,
        )
        .await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b".png\x1b[201~", &mut sock).await;
    });
    let want = "claude-code-claude-code.p".to_string() + "/home/alice/Pictures/x.png" + "ng";
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, want, "the paste lands once, at the cursor");
    assert_eq!(
        l.draft.cursor_chars,
        mid + "/home/alice/Pictures/x.png".chars().count(),
        "the cursor rides the paste's tail"
    );
}
#[test]
fn submit_refusal_rows() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("claude", true, true),
        ("gemini", true, false),
        ("hermes", false, true),
    ]);
    sync_catalog(&mut v);
    // Point the draft at the not-installed one; the submission door refuses
    // it AT SUBMIT with its own reason (the body lists selectable rows only).
    if let Some(l) = v.launcher.as_mut() {
        let idx = l
            .draft
            .harnesses
            .iter()
            .position(|h| h == "hermes")
            .unwrap();
        l.draft.harness_idx = idx;
        l.focus = Focus::Message;
    }
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("no native fno spawn") || reason.contains("not installed"),
                "reason: {reason}"
            );
        }
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }
    assert_eq!(l.draft.message, "", "draft untouched");
    assert!(sock.is_empty(), "nothing went on the wire");

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Unknown {
            reason: "launch timed out".into(),
        },
    };
    apply_launch_update(&mut v, update);
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Unknown { .. }
    ));
    // The attempt is remembered BEYOND the sheet (close + reopen).
    close(&mut v);
    open(&mut v);
    assert!(v.launch_attempt.is_some(), "attempt outlives the sheet");
    // Esc is the explicit cancel while pending: the block resolves, the
    // draft survives, the sheet stays open, and launch is possible again.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(matches!(v.launcher.as_ref().unwrap().phase, Phase::Editing));
    assert!(v.launcher.is_some(), "the canceling Esc does not close");

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Unknown {
                reason: "launch timed out".into(),
            },
        },
    );
    // Arm-released Unknown: the exact state the old gate leaked through.
    assert_eq!(v.launcher.as_ref().unwrap().armed, None);
    // ^j (the launch key) is inert until the operator cancels explicitly.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\n", &mut sock).await;
    });
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Unknown { .. }
    ));
    assert!(sock.is_empty(), "nothing went on the wire: {sock:?}");
}

#[test]
fn draft_staleness_rows() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    // Arm request 1, then edit the draft (a newer revision).
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Refused {
            reason: "capacity".into(),
        },
    };
    apply_launch_update(&mut v, update);
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Refused { .. }
    ));
    // A STALE id (2 never armed by this dock) applies nowhere: the dock's
    // newer draft state survives.
    let stale = AgentLaunchUpdate {
        request_id: 2,
        state: LaunchState::Refused {
            reason: "bogus".into(),
        },
    };
    apply_launch_update(&mut v, stale);
    if let Some(l) = v.launcher.as_ref() {
        assert!(matches!(l.phase, Phase::Refused { ref reason, .. } if reason == "capacity"));
    }

    // AC9-EDGE: the binding rides only while the message's second word
    // (sentence punctuation trimmed) still names the node. A retarget, a
    // prose rewrite or an erasure never binds a stale node; appended
    // prose after the same id keeps it.
    let mut v = plain_view();
    open(&mut v);
    super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
        .expect("a fresh draft yields");
    let cases: &[(&str, Option<&str>)] = &[
        ("/fno:target x-2", None),
        ("fix the flake", None),
        ("", None),
        ("/fno:target x-1 focus on the flake", Some("x-1")),
        ("/fno:target x-1.", Some("x-1")),
    ];
    for (message, want) in cases {
        if let Some(l) = v.launcher.as_mut() {
            l.draft.message = message.to_string();
        }
        let req = v.launcher.as_ref().unwrap().draft.request(1);
        assert_eq!(
            req.node.as_deref(),
            *want,
            "message {message:?} binds {want:?}"
        );
    }
}

#[test]
fn launched_pane_update_returns_the_focus_pane_and_the_card_gives_way() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Launched {
            name: "quiet-otter".into(),
            pane: Some(7),
            seed_delivered: Some(false),
        },
    };
    let pane = apply_launch_update(&mut v, update);
    assert_eq!(pane, Some(7), "the requesting client focuses the new pane");
    // The card gives way to the new row and keeps no draft.
    assert!(v.launcher.is_none(), "the card closes on Launched");
    assert!(v.launcher_closed.is_empty(), "a launched draft is not kept");
}

#[test]
fn footer_names_the_lifecycle_and_the_refusal_reason() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Refused {
                reason: "spawn gate: no free slot".into(),
            },
        },
    );
    let footer = v.launcher.as_ref().unwrap().footer();
    assert!(
        footer.contains("refused") && footer.contains("no free slot"),
        "footer: {footer}"
    );
}
/// A Starting attempt whose update is lost must stay escapable: Esc during
/// Submitting cancels the attempt and returns the sheet to Editing (retry
/// then arms a fresh id, one attempt each).
#[test]
fn submitting_sheet_offers_cancel_and_recovers_to_editing() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(matches!(v.launcher.as_ref().unwrap().phase, Phase::Editing));
    assert_eq!(v.launcher.as_ref().unwrap().armed, None);
}

/// A degraded catalog re-probes on the next open instead of sticking for
/// the session.

/// The screen row display row `i` paints on.
fn card_screen_row(v: &View, i: usize) -> u16 {
    (v.sideline_top() + i - v.sideline_offset()) as u16
}

fn header_index(v: &View, squad: u64) -> usize {
    v.display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Sel(s) if s.tab.is_none() && s.squad == squad))
        .expect("the workspace header is a display row")
}

fn card_texts(v: &View) -> Vec<String> {
    v.launcher
        .as_ref()
        .unwrap()
        .card_lines(v, v.card_width())
        .into_iter()
        .map(|cl| cl.text)
        .collect()
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_card_taps_unfold_a_list_and_fold_the_card() {
    // A tap on a token focuses it and unfolds its list inside the card; the
    // keybar's esc word folds the card and keeps the draft.
    let mut v = view_with_launcher();
    // Tall enough that the unfolded list keeps the esc word on screen.
    v.term = (60, 80);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "keep me");
    // At rest the chip rows are folded; Tab unfolds them.
    v.launcher.as_mut().unwrap().focus = Focus::Permission;
    let tap = |v: &mut View, want: CardHit| {
        let lines = v.launcher.as_ref().unwrap().card_lines(v, v.card_width());
        let (k, col) = lines
            .iter()
            .enumerate()
            .find_map(|(k, cl)| {
                cl.spans
                    .iter()
                    .find(|(_, _, h)| *h == want)
                    .map(|(a, _, _)| (k, *a))
            })
            .unwrap_or_else(|| panic!("{want:?} paints on the card"));
        let at = v
            .display_rows()
            .iter()
            .position(|r| matches!(r, DisplayRow::Card(n) if *n == k))
            .expect("the card line is a display row");
        let Some(ChromeHit::Card(hit)) = v.chrome_hit(card_screen_row(v, at), col as u16) else {
            panic!("a tap on {want:?} resolves to a card hit");
        };
        assert_eq!(hit, want);
        let mut sock: Vec<u8> = Vec::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(card_hit(v, hit, &mut sock)).unwrap();
    };
    let before = card_texts(&v).len();
    tap(&mut v, CardHit::Chip(Focus::Project));
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.focus, Focus::Project, "the tap landed on the token");
    assert_eq!(
        l.picker.as_ref().expect("the tap unfolded a list").field,
        Focus::Project
    );
    assert!(
        card_texts(&v).len() > before,
        "the list unfolds inside the card"
    );
    tap(&mut v, CardHit::Close);
    assert!(v.launcher.is_none(), "the esc word folded the card");
    assert_eq!(
        v.launcher_closed[&1].draft.message, "keep me",
        "the draft is retained"
    );
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_card_header_plus_opens_that_workspaces_draft() {
    // AC1-HP: the + on B's header, or prefix+i with B focused, opens a card
    // at the top of B's group with B's folder on Local; A's group is
    // unchanged.
    let mut v = plain_view();
    let at = header_index(&v, 2);
    let plus = v
        .header_plus_cols()
        .expect("the header spares the + cells")
        .start;
    let hit = v.chrome_hit(card_screen_row(&v, at), plus as u16);
    assert!(matches!(hit, Some(ChromeHit::OpenDraft(2))), "{hit:?}");
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(super::sideline::open_draft(&mut v, 2, &mut sock))
        .unwrap();
    let l = v.launcher.as_ref().expect("the + opened a card");
    assert_eq!(l.squad, 2);
    assert_eq!(l.draft.request(1).cwd, "/code/notes");
    assert_eq!(l.chip_label(Focus::Where, &v.launcher_catalog), "Local");
    let rows = v.display_rows();
    assert!(matches!(rows[header_index(&v, 2) + 1], DisplayRow::Card(0)));
    assert!(!matches!(
        rows[header_index(&v, 1) + 1],
        DisplayRow::Card(_)
    ));
    assert!(v.launcher_closed.is_empty(), "A has no draft");

    // The chord: prefix+i opens the focused workspace's card.
    close(&mut v);
    v.layout.active_squad = 2;
    rt.block_on(super::sideline::toggle_composer(&mut v, &mut sock))
        .unwrap();
    assert_eq!(
        v.launcher.as_ref().expect("prefix+i opened a card").squad,
        2
    );
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_card_esc_folds_to_a_ghost_row_and_reopening_restores_the_text() {
    // AC2-HP: B's card holds text; opening A's folds B to one ghost line,
    // and reopening B restores the text.
    let mut v = plain_view();
    open_in(&mut v, 2);
    type_message(&mut v, "fix b");
    open_in(&mut v, 1);
    let rows = v.display_rows();
    assert!(
        matches!(rows[header_index(&v, 2) + 1], DisplayRow::Ghost(2)),
        "B shows its ghost line"
    );
    assert!(matches!(rows[header_index(&v, 1) + 1], DisplayRow::Card(0)));
    open_in(&mut v, 2);
    assert_eq!(v.launcher.as_ref().unwrap().draft.message, "fix b");
}

#[test]
fn composer_card_launch_lands_in_the_workspace_and_the_card_gives_way() {
    // AC3-HP: the request carries B's folder, which is the cwd rung that
    // places the new row in B's group; Launched closes the card.
    let mut v = plain_view();
    open_in(&mut v, 2);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_mut().unwrap();
    assert_eq!(l.draft.request(1).cwd, "/code/notes");
    l.armed = Some(1);
    l.phase = Phase::Submitting { request_id: 1 };
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Launched {
                name: "w".into(),
                pane: None,
                seed_delivered: Some(true),
            },
        },
    );
    assert!(v.launcher.is_none());
    assert!(!v
        .display_rows()
        .iter()
        .any(|r| matches!(r, DisplayRow::Card(_))));
}

#[test]
fn composer_card_refusal_stays_in_the_card_with_the_draft() {
    // AC4-EDGE: a refused launch leaves the card open with the reason under
    // the prompt and the draft kept.
    let mut v = plain_view();
    open_in(&mut v, 2);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "do it");
    let l = v.launcher.as_mut().unwrap();
    l.armed = Some(1);
    l.phase = Phase::Submitting { request_id: 1 };
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Refused {
                reason: "spawn gate: no free slot".into(),
            },
        },
    );
    let l = v.launcher.as_ref().expect("a refusal keeps the card open");
    assert_eq!(l.draft.message, "do it");
    // The reason wraps at the card's width; read it back as one sentence.
    let reason = card_texts(&v)
        .join(" ")
        .replace('\u{2502}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        reason.contains("refused: spawn gate: no free slot"),
        "{reason}"
    );
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_card_no_folder_refuses_naming_the_missing_folder() {
    // AC6-EDGE: a workspace with no folder refuses at launch, naming it, and
    // keeps the draft.
    let mut v = plain_view();
    v.layout.squads[1].canonical_cwd = String::new();
    open_in(&mut v, 2);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "go");
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(
        matches!(&l.phase, Phase::Refused { reason, .. } if reason.contains("no folder")),
        "{:?}",
        l.phase
    );
    assert_eq!(l.draft.message, "go");
    assert!(sock.is_empty(), "nothing went on the wire");
}
#[test]
fn chips_carry_values_not_axis_names() {
    // AC1-HP, values half: every chip reads its axis's current VALUE, never
    // the axis label the old tab bar painted.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.chip_label(Focus::Harness, &v.launcher_catalog), "claude");
    assert_eq!(l.chip_label(Focus::Model, &v.launcher_catalog), "default");
    assert_eq!(l.chip_label(Focus::Where, &v.launcher_catalog), "Local");
    assert_eq!(l.chip_label(Focus::Permission, &v.launcher_catalog), "auto");
    assert_eq!(l.chip_label(Focus::Effort, &v.launcher_catalog), "default");
    // A non-default placement rides the Where chip's value.
    let mut l = l.clone();
    l.draft.placement = super::agent_launcher::Placement::ThreadSplitBeside;
    assert!(l
        .chip_label(Focus::Where, &v.launcher_catalog)
        .contains("split beside"));
}
/// The regression behind the model-floor contract: a claude account with no
/// pinned model
/// emptied the Model tab, because the catalog read every harness but
/// opencode solely from account records. The capability table now floors
/// each harness's list with its own measured model ids.
#[test]
fn model_floor_rows() {
    let parsed: toml::Value = toml::from_str(super::agent_launcher::CAPABILITY_TOML).unwrap();
    let floor = |harness: &str| -> Vec<String> {
        parsed["harness"][harness]["models"]
            .as_array()
            .unwrap_or_else(|| panic!("{harness} carries a models floor"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let claude = floor("claude");
    for want in ["opus", "sonnet", "haiku", "fable", "claude-opus-5-5"] {
        assert!(
            claude.iter().any(|m| m == want),
            "claude floor names {want}: {claude:?}"
        );
    }
    let codex = floor("codex");
    for want in ["gpt-6-luna", "gpt-6-astra", "gpt-5.5"] {
        assert!(
            codex.iter().any(|m| m == want),
            "codex floor names {want}: {codex:?}"
        );
    }
    assert!(
        parsed["harness"]["opencode"].get("models").is_none(),
        "opencode owns its list through its model command; no floor"
    );

    let cache = r#"{"models":[
        {"slug":"gpt-6-luna","visibility":"list"},
        {"slug":"gpt-reserve","visibility":"hide"},
        {"slug":"gpt-6-luna"},
        {"slug":"gpt-6-astra"}
    ]}"#;
    let (models, hidden) = super::agent_launcher::parse_codex_models(cache);
    let ids: Vec<&str> = models.iter().map(|m| m.model.as_str()).collect();
    assert_eq!(
        ids,
        vec!["gpt-6-luna", "gpt-6-astra"],
        "hide drops, dupes collapse"
    );
    assert_eq!(
        hidden,
        vec!["gpt-reserve"],
        "hidden slugs come back by name"
    );
    assert!(
        models
            .iter()
            .all(|m| m.provider.is_none() && matches!(m.state, ModelState::Ready)),
        "cache slugs are harness-native choices"
    );
    let (empty_models, empty_hidden) = super::agent_launcher::parse_codex_models("not json");
    assert!(
        empty_models.is_empty() && empty_hidden.is_empty(),
        "an unreadable cache is the floor-stands case, never an error"
    );
    assert!(
        super::agent_launcher::parse_codex_models("{}").0.is_empty(),
        "a cache without a models list is the same floor-stands case"
    );

    let floor = vec![super::agent_launcher::ModelChoice {
        name: "opus".into(),
        model: "opus".into(),
        route: String::new(),
        provider: None,
        state: ModelState::Ready,
        key_env: None,
        key_file: None,
    }];
    let pins = super::agent_launcher::parse_configured_account_models(
        r#"{"value":[{"id":"a","harness":"claude","route_provider_id":"zai","model_name":"glm-5.3-flash[1m]","route":"zai/glm-5.3-flash[1m]"}]}"#,
    )
    .unwrap();
    let mut merged = floor;
    super::agent_launcher::merge_model_choices(&mut merged, &pins["claude"]);
    super::agent_launcher::merge_model_choices(&mut merged, &pins["claude"]);
    assert_eq!(merged.len(), 2, "floor first, the routed pin merged once");
    assert_eq!(merged[0].name, "opus", "the floor leads");
    assert_eq!(
        merged[1].provider.as_deref(),
        Some("zai"),
        "the pin keeps its route"
    );
}

/// The regression that forced the tab rewrite: the unavailable row's LABEL
/// used to be ellipsized to fit the long error hint, so "model list
/// unavailable" never reached the screen in CI. Labels stay whole; the
/// hint truncates instead; the selection spans the full inner width.

#[test]
fn degraded_inventory_rows() {
    // When the inventory read fails, the model list shows a disabled entry
    // carrying the reason and the harness default stays launchable;
    // No model row is fabricated when the configured inventory is absent.
    let mut v = view_with_launcher();
    v.launcher_catalog = Some(CatalogOutcome::Ok(
        vec![HarnessChoice {
            name: "claude".into(),
            native: true,
            installed: true,
            models: Vec::new(),
            more: Vec::new(),
            catalog_error: None,
            models_error: None,
            efforts: Some(Vec::new()),
            permission_modes: Some(Vec::new()),
            launch_flags: None,
        }],
        Some("routing inventory unavailable".into()),
        Vec::new(),
    ));
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let (rows, _) =
        super::agent_launcher::picker_rows(&l, Focus::Model, &v.launcher_catalog, &v.backlog);
    let disabled: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry {
                label,
                enabled: false,
                ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let enabled: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry {
                label,
                enabled: true,
                ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        disabled.iter().any(|d| d.contains("unavailable")),
        "the failure is named: {disabled:?}"
    );
    assert!(
        enabled.iter().any(|e| e.starts_with("default (")),
        "the default row still launches: {enabled:?}"
    );
    assert!(
        !enabled.iter().any(|d| d.contains("type a model")),
        "no invented model row: {enabled:?}"
    );
    assert!(
        !v.launcher.as_ref().unwrap().draft.harnesses.is_empty(),
        "harness choices survive the degraded model list"
    );

    let mut v = plain_view();
    v.launcher_catalog = Some(CatalogOutcome::Degraded("probe failed".into()));
    open(&mut v);
    assert!(v.catalog_want, "a degraded read re-arms the probe");
}

/// A claude catalog with a captured flags list and the facts row the
/// worktree resolve wants; the sidecar answers at the worktree generation.
fn pill_harness_view() -> (View, std::sync::MutexGuard<'static, ()>, WireVersionFixture) {
    // One shared FNO_STATE_DIR lock with the model_catalog tests, held for
    // the whole body. Merged tests that already hold the guard call
    // pill_view directly so the non-reentrant lock is taken exactly once.
    let guard = crate::model_catalog::state_env_lock();
    let (v, fx) = pill_view(&guard);
    (v, guard, fx)
}

fn pill_view(_guard: &std::sync::MutexGuard<'static, ()>) -> (View, WireVersionFixture) {
    let mut v = view_with_launcher();
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let own = std::env::current_dir().unwrap().display().to_string();
    v.launcher_catalog = Some(CatalogOutcome::Ok(
        vec![HarnessChoice {
            name: "claude".into(),
            native: true,
            installed: true,
            models: Vec::new(),
            more: Vec::new(),
            catalog_error: None,
            models_error: None,
            efforts: Some(Vec::new()),
            permission_modes: Some(Vec::new()),
            launch_flags: Some(vec!["--agent <agent>".into(), "--verbose".into()]),
        }],
        None,
        vec![super::agent_launcher::ProjectFacts {
            cwd: own,
            current: Some("main".into()),
            branches: vec!["main".into(), "feature/x".into()],
            policy: Ok("external".into()),
        }],
    ));
    sync_catalog(&mut v);
    (v, _wire_fixture)
}

fn launch_argv(v: &mut View, keys: &[u8]) -> Vec<String> {
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(v, keys, &mut sock).await;
    });
    if sock.is_empty() {
        panic!("no launch went to the wire for {keys:?}");
    }
    let mut wire = std::io::Cursor::new(sock);
    match crate::proto::read_msg_sync(&mut wire).unwrap() {
        crate::proto::ClientMsg::AgentLaunch(request) => request.extra_flags,
        other => panic!("composer wrote a different client message: {other:?}"),
    }
}

#[test]
fn pill_rows() {
    // AC11-HP: `--foo bar` typed and committed launches --foo and bar as
    // argv, and the message excludes them.
    let (mut v, _lock, _fx) = pill_harness_view();
    // The picker opens on the second dash and filters "foo"; Space in the
    // picker commits the word verbatim; value capture takes "bar".
    let argv = launch_argv(&mut v, b"--foo bar\r");
    assert_eq!(argv, vec!["--foo", "bar"]);
    let draft = &v.launcher.as_ref().unwrap().draft;
    assert!(
        !draft.message.contains("--foo"),
        "the message excludes the flag"
    );

    for prefix in ["--", "—", "+"] {
        let (mut v, _fx) = pill_view(&_lock);
        let argv = launch_argv(&mut v, format!("explain {prefix}foo bar\r").as_bytes());
        assert_eq!(
            argv,
            vec!["--foo", "bar"],
            "{prefix} forwards a canonical flag"
        );
        assert_eq!(v.launcher.as_ref().unwrap().draft.message, "explain ");
    }
    for prefix in ["—", "+", "--"] {
        let (mut v, _fx) = pill_view(&_lock);
        let l = v.launcher.as_mut().unwrap();
        l.draft.message = "do later".into();
        l.draft.cursor_chars = 3;
        let argv = launch_argv(&mut v, format!("{prefix}ag\rvalue\r").as_bytes());
        assert_eq!(argv, vec!["--agent", "value"]);
        assert_eq!(v.launcher.as_ref().unwrap().draft.message, "do later");
    }
    for text in ["well—being", "C++"] {
        let (mut v, _fx) = pill_view(&_lock);
        assert!(launch_argv(&mut v, format!("{text}\r").as_bytes()).is_empty());
        assert_eq!(v.launcher.as_ref().unwrap().draft.message, text);
    }

    // AC10-EDGE: `--model x` committed reads the model chip x and stores no
    // pill; the argv carries no --model element.
    let (mut v, _fx) = pill_view(&_lock);
    let argv = launch_argv(&mut v, b"--model x\r");
    assert!(
        argv.is_empty(),
        "no --model element rides the argv: {argv:?}"
    );
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.model, "x", "the model chip reads the pinned value");
    assert!(
        l.draft.pills.is_empty(),
        "no pill stores for a chip-owned flag"
    );

    let (mut v, _fx) = pill_view(&_lock);
    let argv = launch_argv(&mut v, b"--permission-mode default\r");
    assert!(
        argv.is_empty(),
        "permission pins its chip rather than an extra flag: {argv:?}"
    );
    assert_eq!(v.launcher.as_ref().unwrap().draft.permission, "default");

    let (mut v, _fx) = pill_view(&_lock);
    v.launcher.as_mut().unwrap().draft.pill_value_capture = false;
    v.launcher
        .as_mut()
        .unwrap()
        .draft
        .pills
        .push(("--verbose".to_string(), None));
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Backspace with an empty message removes the pill, not message text.
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x7f", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(l.draft.pills.is_empty(), "the pill is gone");

    let (mut v, _fx) = pill_view(&_lock);
    // `--` opens the picker; "ag" narrows to --agent; Enter picks it; the
    // trailing Space ends the empty value capture; Enter launches. The flag
    // takes a value and nothing follows: it stores valueless.
    let argv = launch_argv(&mut v, b"--ag\r \r");
    assert_eq!(argv, vec!["--agent"]);
    let draft = &v.launcher.as_ref().unwrap().draft;
    assert!(
        !draft.message.contains("--"),
        "the typed dashes left the message"
    );
    assert_eq!(draft.pills.len(), 1, "the pill stores");
}

#[test]
fn the_flags_picker_lists_the_harness_launch_flags() {
    // AC10-HP: `--` at a word start opens the picker over the harness's
    // launch_flags; its rows carry AddPill with the parsed value arity.
    let (_seed, _lock, _seed_fx) = pill_harness_view();
    for prefix in ["--", "—", "+"] {
        let (mut v, _fx) = pill_view(&_lock);
        let mut sock: Vec<u8> = Vec::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ =
                super::agent_launcher::launcher_keys(&mut v, prefix.as_bytes(), &mut sock).await;
        });
        let l = v.launcher.as_ref().unwrap();
        let picker = l.picker.as_ref().expect("the flags picker opened");
        assert_eq!(picker.field, super::agent_launcher::Focus::Plus);
        let (rows, actions) = super::agent_launcher::picker_rows(
            l,
            super::agent_launcher::Focus::Plus,
            &v.launcher_catalog,
            &v.backlog,
        );
        let labels: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels.iter().any(|l| l == "--agent <agent>"),
            "the captured flags list: {labels:?}"
        );
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Some(super::agent_launcher::PickerAction::AddPill {
                    flag,
                    picks_value
                }) if flag == "--agent" && *picks_value
            )),
            "the flag row commits AddPill"
        );
    }
}

#[test]
fn a_chip_owned_typed_flag_still_refuses_at_submit() {
    // AC11-ERR: `--cwd /x` committed verbatim refuses at submit, naming the
    // composer chip that owns it.
    let (mut v, _lock, _fx) = pill_harness_view();
    v.launcher
        .as_mut()
        .unwrap()
        .draft
        .pills
        .push(("--cwd".to_string(), Some("/x".to_string())));
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        super::agent_launcher::Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("composer chip"),
                "the refusal names the chip: {reason}"
            );
        }
        other => panic!("expected the chip refusal, got {other:?}"),
    }
}

#[test]
fn where_picker_lists_local_and_the_placement_rows() {
    // AC4-HP: the Where picker lists Local (checked) under `Run on`, then
    // known fleet hosts, then the four placement rows under `Open as`.
    let mut v = plain_view();
    super::machine_tree::set_on(&mut v, true);
    super::machine_tree::apply(
        &mut v,
        Ok(serde_json::json!({"machines": [
            {"host": "remote-box", "local": false, "fresh": true, "age_s": 1, "sessions": []}
        ]})),
    );
    open(&mut v);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let (rows, actions) =
        super::agent_launcher::picker_rows(&l, Focus::Where, &v.launcher_catalog, &v.backlog);
    let labels: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Header(h) => Some(format!("[{h}]")),
            crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "[Run on]",
            "Local",
            "remote-box",
            "[Open as]",
            "thread",
            "thread split beside",
            "thread new tab",
            "pane: active tab",
        ],
        "the Where picker's rows: {labels:?}"
    );
    // The Local row carries the check and no action (there is nothing to
    // switch to).
    let local_action = actions[1].clone();
    assert_eq!(
        local_action,
        Some(super::agent_launcher::PickerAction::RunOn(None))
    );
    // Committing split beside records the placement; request() turns it
    // into --substrate thread --portal N --split right.
    let mut l = v.launcher.take().unwrap();
    super::agent_launcher::apply_picker_action(
        &mut l,
        &v.launcher_catalog,
        super::agent_launcher::PickerAction::Place(
            super::agent_launcher::Placement::ThreadSplitBeside,
        ),
        2,
        Focus::Where,
    );
    v.launcher = Some(l.clone());
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.placement_portal, 2);
    assert!(
        l.chip_label(Focus::Where, &v.launcher_catalog)
            .contains("split beside"),
        "the Where chip shows the picked view: {:?}",
        l.chip_label(Focus::Where, &v.launcher_catalog)
    );
    let mut l = v.launcher.take().unwrap();
    super::agent_launcher::apply_picker_action(
        &mut l,
        &v.launcher_catalog,
        super::agent_launcher::PickerAction::RunOn(Some("remote-box".into())),
        2,
        Focus::Where,
    );
    assert_eq!(l.draft.request(1).host.as_deref(), Some("remote-box"));
    assert!(l
        .chip_label(Focus::Where, &v.launcher_catalog)
        .starts_with("remote-box"));
    let (session, _version) = wire_fixture_at(121);
    v.session = session;
    l.draft.worktree = Some(false);
    v.launcher = Some(l);
    let mut sock = Vec::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock)
            .await
            .unwrap();
    });
    assert!(
        sock.is_empty(),
        "an older server must never receive a remote launch"
    );
    assert!(
        matches!(&v.launcher.as_ref().unwrap().phase, Phase::Refused { reason, .. } if reason.contains("does not support remote launches"))
    );
}

#[test]
fn shell_mode_keys_glyph_and_placeholders() {
    // The composer's bang mode: `!` on an empty input is the mode switch
    // (AC1), every later key is command text with the launch gestures
    // literal (AC3), Backspace on an empty line leaves again (AC2), and
    // the glyph + placeholder paint the mode. Folds the retired
    // paint-marker test's contract (plain glyph and placeholder on a
    // fresh dock and after typing).
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(v.launcher.as_ref().unwrap().focus, Focus::Message);
    assert_painted(&v, '\u{276f}', "prompt \u{b7} +flag");

    // AC1: the `!` is consumed as the mode switch.
    type_message(&mut v, "!");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(l.shell);
        assert!(l.draft.message.is_empty(), "the ! is consumed");
    }
    assert_painted(&v, '!', "shell command in");

    // AC3: in shell mode the launch gestures land as text and open no
    // picker.
    type_message(&mut v, "git log --oneline @x");
    {
        let l = v.launcher.as_ref().unwrap();
        assert_eq!(l.draft.message, "git log --oneline @x");
        assert!(l.picker.is_none(), "no picker in shell mode");
    }

    // Typing replaces the placeholder; the glyph stays.
    assert_painted(&v, '!', "git log");

    // AC2: Backspace through the text, then one more leaves shell mode.
    for _ in 0..30 {
        type_message(&mut v, "\x7f");
        if v.launcher
            .as_ref()
            .is_some_and(|l| l.draft.message.is_empty())
        {
            break;
        }
    }
    assert!(v.launcher.as_ref().unwrap().shell, "still in shell mode");
    type_message(&mut v, "\x7f");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell, "the empty-line Backspace left shell mode");
        assert!(l.draft.message.is_empty());
    }
    assert_painted(&v, '\u{276f}', "prompt \u{b7} +flag");

    // Folded from the retired newline test: ^j inserts a newline in the
    // message and Enter never fires from inside it.
    type_message(&mut v, "line one");
    type_message(&mut v, "\nline two");
    {
        let l = v.launcher.as_ref().unwrap();
        assert_eq!(l.draft.message, "line one\nline two");
        assert_eq!(l.armed, None, "Enter inside the message never submits");
    }
    for _ in 0..30 {
        type_message(&mut v, "\x7f");
        if v.launcher
            .as_ref()
            .is_some_and(|l| l.draft.message.is_empty())
        {
            break;
        }
    }

    // AC3: a `!` mid-text is literal, and a pasted `!ls` never enters
    // shell mode.
    type_message(&mut v, "fix the !bang parser");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell);
        assert_eq!(l.draft.message, "fix the !bang parser");
    }
    type_message(&mut v, "\x1b[200~!ls\x1b[201~");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell, "a paste never enters shell mode");
        assert!(l.draft.message.contains("!ls"), "paste is literal text");
        assert!(l.picker.is_none());
    }
}

/// Assert the card's first prompt line carries `glyph` and `placeholder`
/// (the placeholder text may be a prefix of a longer one).
fn assert_painted(v: &View, glyph: char, placeholder: &str) {
    // Line 0 is the frame's top rule; the prompt is the line under it.
    let row = card_texts(v).remove(1);
    assert!(row.contains(glyph), "glyph {glyph} painted: {row:?}");
    assert!(
        row.contains(placeholder),
        "placeholder {placeholder:?} painted: {row:?}"
    );
}

#[test]
fn picker_filter_rows() {
    // AC2-HP: Enter on a chip opens its picker; typing narrows the rows in
    // place; Backspace widens again; the chip's own value is untouched.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Walk to the Harness chip and open its picker.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    assert!(v.launcher.as_ref().unwrap().picker.is_some());
    // Type `cod`: only the codex row survives the filter.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"cod", &mut sock).await;
    });
    let read = |v: &View| -> Vec<String> {
        let l = v.launcher.as_ref().unwrap();
        let picker = l.picker.as_ref().unwrap();
        picker
            .popup
            .rows
            .iter()
            .filter_map(|r| match r {
                crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        read(&v),
        vec!["codex"],
        "the query narrows the rows: {:?}",
        read(&v)
    );
    assert_eq!(
        v.launcher.as_ref().unwrap().picker.as_ref().unwrap().filter,
        "cod",
        "the query is live"
    );
    // Backspace once: the query `co` still narrows to codex.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x7f], &mut sock).await;
    });
    assert_eq!(read(&v), vec!["codex"], "`co` still filters");
    // Clearing the query fully restores every row.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x7f, 0x7f], &mut sock).await;
    });
    assert_eq!(read(&v).len(), 2, "widened");

    // Filtering keeps the target/action mapping on the actual harness row.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Walk to the Harness chip, open the picker, and narrow to codex.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"co", &mut sock).await;
    });
    {
        let l = v.launcher.as_ref().unwrap();
        let picker = l.picker.as_ref().unwrap();
        assert_eq!(picker.filter, "co", "the query is live");
        let rows: Vec<_> = picker
            .popup
            .rows
            .iter()
            .filter(|r| matches!(r, crate::popup::PopupRow::Entry { .. }))
            .collect();
        assert_eq!(rows.len(), 1, "narrowed to one row: {rows:?}");
    }
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x0d], &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.harness(), "codex", "the highlighted row committed");
    assert!(l.picker.is_none(), "the pick closed the picker");
}

#[test]
fn at_opens_the_node_picker_and_picking_inserts_the_id() {
    // Change 7: `@` in the message opens a node picker over the layout's
    // backlog cards; the glyph never lands in the draft; the pick inserts
    // `<id> ` at the cursor.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    // One backlog card for the picker to list (the fixture ships none).
    v.backlog = vec![crate::proto::BacklogCard {
        id: "x-6233".into(),
        slug: "a-node".into(),
        priority: "p1".into(),
        state: crate::proto::CardState::Ready,
        pane_id: None,
        attach_id: None,
        where_hint: None,
        project: None,
        lane: None,
        plan_path: None,
        number: None,
        head: false,
        link: None,
    }];
    type_message(&mut v, "plan ");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"@", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert_eq!(
        picker.field,
        Focus::Message,
        "the node picker rides the message field"
    );
    let entry_count = picker
        .popup
        .rows
        .iter()
        .filter(|r| matches!(r, crate::popup::PopupRow::Entry { .. }))
        .count();
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.message,
        "plan ",
        "the @ never lands in the draft"
    );
    // Pick the first card: its id lands at the cursor.
    let first_id: String = picker
        .actions
        .iter()
        .find_map(|a| match a {
            Some(super::agent_launcher::PickerAction::InsertNode(id)) => Some(id.clone()),
            _ => None,
        })
        .expect("the fixture carries at least one InsertNode row");
    assert!(entry_count >= 1, "cards list: {entry_count}");
    let mut l = v.launcher.take().unwrap();
    let action = super::agent_launcher::PickerAction::InsertNode(first_id.clone());
    super::agent_launcher::apply_picker_action(
        &mut l,
        &v.launcher_catalog,
        action,
        0,
        Focus::Message,
    );
    v.launcher = Some(l);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.message,
        format!("plan {first_id} "),
        "the node id lands at the cursor"
    );
}

#[test]
fn open_with_rows() {
    // AC6-HP: the board prefill lands the message, the cursor at its end,
    // the node's project (appended when it was not a candidate) and the
    // node binding; the wire request carries the node. The phase resets to
    // Editing so the operator's next Launch submits fresh.
    let mut v = plain_view();
    open(&mut v);
    super::agent_launcher::open_with(
        &mut v,
        "/fno:target x-1".into(),
        Some("/r/footnote"),
        "x-1".into(),
    )
    .expect("a fresh draft yields");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "/fno:target x-1");
    assert_eq!(l.draft.cursor_chars, "/fno:target x-1".chars().count());
    let idx = l.draft.project_idx;
    assert_eq!(l.draft.projects[idx], "/r/footnote");
    assert_eq!(l.draft.node.as_deref(), Some("x-1"));
    assert_eq!(l.draft.request(9).node.as_deref(), Some("x-1"));
    assert_eq!(l.phase, Phase::Editing);
    assert_eq!(l.focus, Focus::Message);

    // AC7-EDGE: a kept draft with typed text is never overwritten; the
    // error names the way out and the draft is unchanged.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "fix the flake".into();
        l.draft.revision += 1;
    }
    let err = super::agent_launcher::open_with(
        &mut v,
        "/fno:target x-1".into(),
        Some("/r/footnote"),
        "x-1".into(),
    )
    .expect_err("a held draft refuses");
    assert!(
        err.contains("holds a draft"),
        "the refusal names the kept draft: {err}"
    );
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "fix the flake");
    assert_eq!(l.draft.node, None);

    // The in-flight guard: even an EMPTY draft does not yield while an
    // owned attempt is Starting. A seedless launch's outcome must fold
    // onto the dock it belongs to, never onto a fresh board prefill.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Starting,
        },
    );
    let err =
        super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
            .expect_err("an in-flight attempt holds the draft");
    assert!(err.contains("holds a draft"), "err: {err}");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.phase, Phase::Submitting { request_id: 1 });
    assert_eq!(l.draft.node, None);

    // AC8-EDGE: a terminal `Launched` attempt makes way for another node's
    // prefill; the phase is Editing again.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "/fno:target x-9".into();
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Launched {
                name: "w".into(),
                pane: Some(3),
                seed_delivered: Some(true),
            },
        },
    );
    super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
        .expect("a launched draft yields");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "/fno:target x-1");
    assert_eq!(l.draft.node.as_deref(), Some("x-1"));
    assert_eq!(l.phase, Phase::Editing);
}
#[test]
fn probe_projects_prefers_the_open_draft_and_falls_back_to_candidates() {
    // The probe reads facts for the projects the dock actually shows; with
    // the dock closed it still probes the fresh-draft list so the first
    // open lands facts together with the catalog.
    let mut v = plain_view();
    let own = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    assert!(
        super::agent_launcher::probe_projects(&v)
            .iter()
            .any(|p| *p == own),
        "a closed dock probes the candidate list: {:?}",
        super::agent_launcher::probe_projects(&v)
    );
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.projects = vec!["/tmp/proj-a".into(), "/tmp/proj-b".into()];
    }
    assert_eq!(
        super::agent_launcher::probe_projects(&v),
        vec!["/tmp/proj-a".to_string(), "/tmp/proj-b".to_string()]
    );
}
#[test]
fn the_composer_picker_never_lists_the_retired_gemini() {
    // The user retired gemini: load_catalog filters the capability table's
    // row out of the picker list (the table keeps it for resume argv and
    // state grants).
    let _env = crate::model_catalog::state_env_lock();
    let dir = fresh_state_dir();
    std::env::set_var("FNO_STATE_DIR", &dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let outcome = rt.block_on(super::agent_launcher::load_catalog(Vec::new()));
    let rows = match outcome {
        CatalogOutcome::Ok(rows, _, _) => rows,
        CatalogOutcome::Degraded(reason) => panic!("harness rows must not degrade: {reason}"),
    };
    assert!(
        rows.iter().all(|r| r.name != "gemini"),
        "gemini never lists in the composer picker"
    );
    assert!(
        rows.iter().any(|r| r.name == "claude"),
        "the rest of the catalog still loads"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh state root with no cache/ subdir, so a fetch cannot even write.
fn fresh_state_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fno-launcher-catalog-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn composer_card_paint_rows() {
    // AC1-HP: the card carries the values, never the old tab names; the
    // folder line shows only while a project token holds the keyboard.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let rest = card_texts(&v);
    assert!(
        !rest.iter().any(|t| t.contains("[less]")),
        "the chip rows fold at rest: {rest:?}"
    );
    assert!(
        rest.iter().any(|t| t.contains("$ ")),
        "the command line paints at rest: {rest:?}"
    );
    v.launcher.as_mut().unwrap().focus = Focus::Permission;
    let texts = card_texts(&v);
    for chip in ["auto", "claude", "default"] {
        assert!(
            texts.iter().any(|t| t.contains(chip)),
            "chip {chip:?} paints on the card: {texts:?}"
        );
    }
    for tab in ["Harness", "Mode", "Flags"] {
        assert!(
            !texts.iter().any(|t| t.contains(tab)),
            "no tab strip: {tab:?} never paints"
        );
    }
    assert!(
        !texts.iter().any(|t| t.contains("working directory")),
        "the folder line hides while Message holds focus"
    );
    v.launcher.as_mut().unwrap().focus = Focus::Message;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ =
            super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z\x1b[Z\x1b[Z", &mut sock).await;
    });
    assert_eq!(v.launcher.as_ref().unwrap().focus, Focus::Project);
    // The folder line hard-wraps at the card's width; its first row carries
    // the label and the head of the path.
    let own = std::env::current_dir().unwrap().display().to_string();
    let head = own.get(..6).unwrap_or(&own).to_string();
    // The frame and the wrap split the line, so read it without them.
    let flat: String = card_texts(&v)
        .join("")
        .replace('\u{2502}', "")
        .split_whitespace()
        .collect();
    let texts = card_texts(&v);
    assert!(
        flat.contains("workingdirectory") && flat.contains(head.as_str()),
        "the folder line derives from the real cwd: {texts:?}"
    );

    // AC1-EDGE: a narrow card wraps tokens onto more lines; no line outruns
    // the width and no value is cut.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    // Wide enough for a labelled chip such as `harness claude` inside the frame.
    let w = 20;
    v.launcher.as_mut().unwrap().focus = Focus::Permission;
    let l = v.launcher.as_ref().unwrap();
    let texts: Vec<String> = l.card_lines(&v, w).into_iter().map(|cl| cl.text).collect();
    for t in &texts {
        assert!(t.chars().count() <= w, "line {t:?} fits the {w}-col card");
    }
    // Where stays hidden while the workspace is local, so it has no chip.
    for f in [Focus::Harness, Focus::Model, Focus::Permission] {
        let label = l.chip_label(f, &v.launcher_catalog);
        assert!(
            texts.iter().any(|t| t.contains(&label)),
            "token {label:?} paints whole: {texts:?}"
        );
    }
}

#[test]
fn a_picker_open_across_the_catalog_landing_refreshes_on_input() {
    // The picker opens while the catalog read pends; when the read lands,
    // the next key refreshes the STORED rows before it is handled, so a
    // commit resolves through the rows the operator sees.
    let mut v = view_with_launcher();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t\r", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert!(
        picker.all_rows.iter().any(
            |r| matches!(r, crate::popup::PopupRow::Entry { label, .. } if label == "reading harnesses...")
        ),
        "opened on the pending rows"
    );
    // The catalog lands; one key refreshes the picker in place.
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"x", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert!(
        picker
            .all_rows
            .iter()
            .any(|r| matches!(r, crate::popup::PopupRow::Entry { label, .. } if label == "claude")),
        "the stored rows refreshed: {:?}",
        picker.all_rows
    );
    assert_eq!(picker.filter, "x", "the query survives the refresh");
}

/// One git project's facts row for the draft cwd, with a knobbed policy
/// word: the shape every worktree-box test starts from.
fn git_facts(policy: &str) -> Vec<ProjectFacts> {
    let own = std::env::current_dir().unwrap().display().to_string();
    vec![ProjectFacts {
        cwd: own,
        current: Some("main".into()),
        branches: vec!["main".into(), "feature/x".into()],
        policy: Ok(policy.into()),
    }]
}

#[test]
fn the_box_defaults_to_the_policy_and_never_greys_out() {
    // AC6-HP: an `external` project defaults to checked with Branch `main`;
    // a `never` project paints unchecked and greyed with its reason on the
    // facts line, and the box never toggles there.
    let mut v = view_with_launcher();
    let never = catalog(&[("claude", true, true)]).unwrap();
    let mut never = never;
    if let CatalogOutcome::Ok(rows, err, facts) = &mut never {
        *facts = git_facts("never");
        let _ = (rows, err);
    }
    v.launcher_catalog = Some(never);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "no",
        "never keeps the box unchecked"
    );
    assert_eq!(
        l.chip_label(Focus::Branch, &v.launcher_catalog),
        "main",
        "the Branch chip reads the current branch under never"
    );
    // The facts line names the policy: the greyed state carries its reason.
    v.launcher.as_mut().unwrap().focus = Focus::Worktree;
    let texts = card_texts(&v);
    let flat: String = texts
        .join("")
        .replace('\u{2502}', "")
        .split_whitespace()
        .collect();
    assert!(
        flat.contains("policynever:runsinplace"),
        "the greyed box names its reason: {texts:?}"
    );
    // Enter toggles nothing under never.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "no",
        "never never toggles"
    );

    // The same draft on an `external` project defaults to checked, Branch
    // `main`, and Enter toggles the box off.
    let mut v = view_with_launcher();
    let mut ext = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut ext {
        *facts = git_facts("external");
    }
    v.launcher_catalog = Some(ext);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "yes",
        "external defaults to checked"
    );
    assert_eq!(
        l.chip_label(Focus::Branch, &v.launcher_catalog),
        "main",
        "a checked box launches `main` by default"
    );
    // Shift-tab once: Message -> Worktree. Enter toggles the box off.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z", &mut sock).await;
    });
    assert_eq!(
        v.launcher.as_ref().unwrap().focus,
        Focus::Worktree,
        "shift-tab reaches the box"
    );
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "no",
        "Enter on the box toggles it off"
    );
}

#[test]
fn git_policy_refusal_rows() {
    // AC6-EDGE: with the facts unread and no explicit pick, the box reads
    // `worktree ?` and the submit refuses with its reason; toggling the box
    // resolves the state and the launch proceeds.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "?",
        "the unknown state names itself"
    );
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("worktree policy unread"),
            "the refusal names the unread policy: {reason}"
        ),
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }
    // An explicit toggle resolves it: the box turns on and the draft
    // launches past the policy gate.
    v.launcher.as_mut().unwrap().phase = Phase::Editing;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z", &mut sock).await;
    });
    assert_eq!(
        v.launcher.as_ref().unwrap().focus,
        Focus::Worktree,
        "shift-tab reaches the box"
    );
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b" ", &mut sock).await;
    });
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "yes",
        "the explicit pick clears the ? state"
    );

    // AC6-EDGE covers the failed read as well as the missing one: a facts
    // row whose policy read failed paints `worktree ?` and refuses the
    // launch with its reason instead of guessing a checked default.
    let mut v = view_with_launcher();
    let mut failed = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut failed {
        *facts = vec![ProjectFacts {
            cwd: std::env::current_dir().unwrap().display().to_string(),
            current: Some("main".into()),
            branches: vec!["main".into()],
            policy: Err("policy command failed".into()),
        }];
    }
    v.launcher_catalog = Some(failed);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "?",
        "a failed read names itself"
    );
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("worktree policy unread"),
            "the refusal names the unread policy: {reason}"
        ),
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }

    use super::agent_launcher::{version_at_least, LAUNCH_EXTRA_AXES_PROTO, LAUNCH_WORKTREE_PROTO};
    assert!(!version_at_least(None, LAUNCH_EXTRA_AXES_PROTO));
    assert!(!version_at_least(Some(90), LAUNCH_EXTRA_AXES_PROTO));
    assert!(version_at_least(Some(91), LAUNCH_EXTRA_AXES_PROTO));
    assert!(version_at_least(Some(92), LAUNCH_EXTRA_AXES_PROTO));
    // The worktree gate sits one generation later than the launch extras.
    assert!(!version_at_least(Some(93), LAUNCH_WORKTREE_PROTO));
    assert!(!version_at_least(Some(94), LAUNCH_WORKTREE_PROTO));
    assert!(version_at_least(Some(95), LAUNCH_WORKTREE_PROTO));

    // AC9-HP: under a wire-91 sidecar a checked box refuses before the wire
    // with the reconnect reason; the same draft on wire 94 submits.
    let mut v = view_with_launcher();
    let mut ext = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut ext {
        *facts = git_facts("external");
    }
    v.launcher_catalog = Some(ext);
    sync_catalog(&mut v);
    let (session, _wire_fixture) = wire_fixture_at(91);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("does not support worktree launches"),
            "the refusal names the reconnect: {reason}"
        ),
        other => panic!("expected the wire refusal, got {other:?}"),
    }
}
#[test]
fn the_harness_preselect_is_claude_then_codex_never_alphabetical() {
    // First sync on a fresh dock: claude when the catalog has it, codex when
    // claude is missing, the first row only when neither exists. In-session,
    // the retained draft IS the last-harness-used memory.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("agy", true, true),
        ("claude", true, true),
        ("codex", true, true),
    ]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "claude",
        "claude preselects over the alphabetical first row"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("agy", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "codex",
        "a missing claude falls to codex"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("agy", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "a catalog with neither still shows a value"
    );

    // The mux-dir store outranks the ladder: the harness the last launch
    // used preselects even when claude sits first in the catalog.
    std::fs::create_dir_all(crate::proto::mux_dir()).unwrap();
    std::fs::write(
        crate::proto::mux_dir().join("composer-last-harness"),
        "agy\n",
    )
    .unwrap();
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("claude", true, true),
        ("codex", true, true),
        ("agy", true, true),
    ]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "the stored last-used harness outranks the claude ladder"
    );

    // The retained draft carries the last harness used across close/open.
    if let Some(l) = v.launcher.as_mut() {
        l.draft.harness_idx = l.draft.harnesses.iter().position(|h| h == "agy").unwrap();
    }
    close(&mut v);
    open(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "the retained draft is the last-harness memory"
    );
}
fn floor(name: &str) -> super::agent_launcher::ModelChoice {
    super::agent_launcher::ModelChoice {
        name: name.to_string(),
        model: name.to_string(),
        route: String::new(),
        provider: None,
        state: ModelState::Ready,
        key_env: None,
        key_file: None,
    }
}
fn choice_ready(name: &str, provider: &str) -> super::agent_launcher::ModelChoice {
    let mut m = floor(name);
    m.provider = Some(provider.to_string());
    m
}

fn nokey(name: &str, provider: &str, env: &str) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.state = ModelState::NoKey {
        key_env: env.to_string(),
        steps: vec![],
    };
    m.key_env = Some(env.to_string());
    m
}
fn unreachable(name: &str, provider: &str, reason: &str) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.state = ModelState::Unreachable {
        reason: reason.to_string(),
    };
    m
}

#[test]
fn nokey_row_enter_shows_connect_steps_and_esc_returns_to_more() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.more = vec![nokey_steps(
            "MiniMax-M2",
            "minimax",
            "MINIMAX_API_KEY",
            "export MINIMAX_API_KEY=<your key>",
        )];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    let mut l = v.launcher.take().unwrap();
    let anchor = crate::popup::Anchor::At { row: 4, col: 10 };
    super::agent_launcher::open_more(&mut l, &v.launcher_catalog, anchor);
    let action = more_row_action(&l, "MiniMax-M2").expect("the nokey row carries an action");
    assert!(
        matches!(
            action,
            super::agent_launcher::PickerAction::ShowSteps { .. }
        ),
        "a nokey rows enter names the connect steps: {action:?}"
    );
    if let super::agent_launcher::PickerAction::ShowSteps { title, lines } = action {
        assert_eq!(title, "connect minimax");
        assert_eq!(lines, vec!["export MINIMAX_API_KEY=<your key>".to_string()]);
        super::agent_launcher::show_steps(&mut l, title, lines, anchor);
    }
    let picker = l.picker.as_ref().expect("steps sheet open");
    assert_eq!(
        picker.mode,
        super::agent_launcher::PickerMode::Steps {
            title: "connect minimax".to_string()
        }
    );
    let rows_now: Vec<String> = picker
        .all_rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Header(h) => Some(format!("# {h}")),
            crate::popup::PopupRow::Entry { label, enabled, .. } => {
                Some(format!("{} {}", if *enabled { "+" } else { "-" }, label))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        rows_now,
        vec![
            "# connect minimax".to_string(),
            "- export MINIMAX_API_KEY=<your key>".to_string()
        ],
        "the steps sheet is one header plus disabled lines: {rows_now:?}"
    );
    assert_eq!(l.draft.model, "", "nothing was picked");
    let current = l.picker.take().unwrap();
    super::agent_launcher::picker_step_down(&mut l, &v.launcher_catalog, &v.backlog, current);
    let picker = l.picker.as_ref().expect("back on the more list");
    assert_eq!(picker.mode, super::agent_launcher::PickerMode::More);
}
fn nokey_steps(
    name: &str,
    provider: &str,
    env: &str,
    step: &str,
) -> super::agent_launcher::ModelChoice {
    let mut m = nokey(name, provider, env);
    m.state = ModelState::NoKey {
        key_env: env.to_string(),
        steps: vec![step.to_string()],
    };
    m
}

fn more_row_action(
    l: &super::agent_launcher::Launcher,
    label: &str,
) -> Option<super::agent_launcher::PickerAction> {
    let picker = l.picker.as_ref()?;
    let row = picker.all_rows.iter().position(|r| {
        matches!(r,
        crate::popup::PopupRow::Entry { label: l, .. } if l == label)
    })?;
    picker.all_actions.get(row)?.clone()
}
#[test]
fn unreachable_row_enter_names_the_gap_and_picks_nothing() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.more = vec![unreachable(
            "glm-5",
            "zai-openai",
            "claude speaks anthropic; zai-openai serves openai",
        )];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    let mut l = v.launcher.take().unwrap();
    let anchor = crate::popup::Anchor::At { row: 4, col: 10 };
    super::agent_launcher::open_more(&mut l, &v.launcher_catalog, anchor);
    let action = more_row_action(&l, "glm-5").expect("the unreachable row carries an action");
    let (title, lines) = match action {
        super::agent_launcher::PickerAction::ShowSteps { title, lines } => (title, lines),
        other => panic!("expected ShowSteps, got {other:?}"),
    };
    assert_eq!(title, "zai-openai on claude");
    assert_eq!(
        lines,
        vec!["claude speaks anthropic; zai-openai serves openai".to_string()]
    );
    super::agent_launcher::show_steps(&mut l, title, lines, anchor);
    assert_eq!(l.draft.model, "", "nothing was picked");
}
#[test]
fn launch_refuses_a_pinned_pick_whose_key_does_not_resolve() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.models = vec![keyed("deepseek-chat", "deepseek", "FNO_TEST_DS_KEY", None)];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        let idx = l
            .draft
            .harnesses
            .iter()
            .position(|h| h == "claude")
            .unwrap();
        l.draft.harness_idx = idx;
    }
    let mut l = v.launcher.take().unwrap();
    let pin = super::agent_launcher::PickerAction::PickRow {
        harness: "claude".to_string(),
        name: "deepseek-chat".to_string(),
        model: "deepseek-chat".to_string(),
        route: String::new(),
        provider: Some("deepseek".to_string()),
    };
    super::agent_launcher::apply_picker_action(&mut l, &v.launcher_catalog, pin, 0, Focus::Model);
    v.launcher = Some(l);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("FNO_TEST_DS_KEY is not set"),
                "the refusal names the env var: {reason}"
            );
        }
        other => panic!("expected a key refusal, got {other:?}"),
    }
    assert!(sock.is_empty(), "nothing went on the wire");
}
fn keyed(
    name: &str,
    provider: &str,
    env: &str,
    key_file: Option<String>,
) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.key_env = Some(env.to_string());
    m.key_file = key_file;
    m
}

// -- KillLeft (Ctrl+U / Cmd+Backspace) --------------------------------------

/// The composer's contract, one walk: the fold maps both Cmd+Backspace
/// spellings (and Shift+Enter's CSI-u), the kill line edits every editor
/// layer, admission refusals map to canned one-sentence heads with the raw
/// text behind Ctrl+O, `?` opens the help sheet on an empty input and stays
/// text once words exist, and the runtime flags parser reads beside and
/// next-line descriptions while never suggesting a flag the doors own.
#[test]
fn composer_keys_refusals_help_and_flag_parsing_contract() {
    // -- the key fold ------------------------------------------------------
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x15"),
        vec![super::agent_launcher::LKey::KillLeft],
        "Ctrl+U"
    );
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b\x7f"),
        vec![super::agent_launcher::LKey::KillWord],
        "ESC+DEL is one key (Option+Backspace, the word kill), never Esc then Backspace"
    );
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b[13;2u"),
        vec![super::agent_launcher::LKey::ShiftEnter]
    );

    // -- the draft and the InputField editors ------------------------------
    let mut v = plain_view();
    open(&mut v);
    let l = v.launcher.as_mut().unwrap();
    l.draft.message = "first\nsecond".to_string();
    l.draft.cursor_chars = "first\nsecond".chars().count();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x15", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "first\n");
    assert_eq!(l.draft.cursor_chars, "first\n".chars().count());

    let mut f = super::input_field::InputField::new("name", 64).with_text("zai");
    f.feed(b"\x15");
    assert_eq!(f.text(), "");
    let mut f = super::input_field::InputField::new("name", 64).with_text("zai");
    f.feed(b"\x1b[D\x1b[D"); // cursor after "z"
    f.feed(b"\x15");
    assert_eq!(f.text(), "ai");

    // -- refusal heads and the Ctrl+O raw toggle ---------------------------
    use super::agent_launcher::first_sentence;
    assert_eq!(
        first_sentence("process admission refused: count=unknown, ceiling=29, reason=measurement-unavailable"),
        "process limit: fno cannot read the machine's load (count=unknown); wait a beat or run it in a terminal",
    );
    assert_eq!(
        first_sentence("process admission refused: machine runaway brake holds (900s left): hot; largest group cargo x40"),
        "process limit: machine runaway brake on; run it in a terminal or wait for the all-clear",
    );
    assert_eq!(
        first_sentence("plain first line\nsecond line"),
        "plain first line",
    );

    let mut v = plain_view();
    open(&mut v);
    let l = v.launcher.as_mut().unwrap();
    l.phase = Phase::Refused {
        request_id: 1,
        reason: "process admission refused: count=unknown, ceiling=29".into(),
    };
    assert_eq!(
        l.footer(),
        "refused: process limit: fno cannot read the machine's load (count=unknown); wait a beat or run it in a terminal (^o raw)",
    );
    l.show_detail = true;
    assert_eq!(
        l.footer(),
        "refused: process admission refused: count=unknown, ceiling=29",
        "Ctrl+O shows the raw text"
    );

    // -- ? opens help on empty input, types otherwise ----------------------
    let mut v = plain_view();
    open(&mut v);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"?", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(
        l.picker
            .as_ref()
            .is_some_and(|p| p.mode == super::agent_launcher::PickerMode::Help),
        "? on an empty input opens the help sheet"
    );
    assert!(l.draft.message.is_empty());
    // Esc closes the sheet outright; a second ? stays text on a non-empty
    // input.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(l.picker.is_none(), "esc closes the help sheet");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"what?", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.draft.message, "what?",
        "? is text once the input holds words"
    );
    assert!(l.picker.is_none());

    // -- the runtime flags parser ------------------------------------------
    let rows = super::harness_flags::parse_help(
        "Usage: claude [options] [prompt]\n\
         \n\
         Options:\n\
         \x20 --version          Show version number\n\
         \x20 --add-dir <directories...>\n\
         \x20                    Directories the session may read\n\
         \x20 --dangerously-skip-permissions\n\
         \x20 --model, -m <model>\n\
         \x20                    Model override\n",
    );
    assert_eq!(
        rows,
        vec![
            (
                "--add-dir <directories...>".to_string(),
                "Directories the session may read".to_string()
            ),
            ("--dangerously-skip-permissions".to_string(), String::new()),
        ],
        "owned flags never suggest; beside and next-line descriptions both land",
    );
}

/// Run `bytes` through the launcher's key folder on a throwaway socket.
fn press(v: &mut View, bytes: &[u8]) {
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(v, bytes, &mut sock).await;
    });
}

/// The labels of the open picker's entry rows, in order.
fn picker_labels(v: &View) -> Vec<String> {
    v.launcher
        .as_ref()
        .and_then(|l| l.picker.as_ref())
        .map(|p| {
            p.popup
                .rows
                .iter()
                .filter_map(|r| match r {
                    crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn with_models(v: &mut View) {
    if let Some(CatalogOutcome::Ok(rows, _, _)) = v.launcher_catalog.as_mut() {
        if let Some(row) = rows.first_mut() {
            row.models = vec![floor("opus"), floor("sonnet")];
        }
    }
}

#[test]
fn composer_complete_sigils_commit_through_chip_actions() {
    let (mut v, _lock, _fx) = pill_harness_view();
    with_models(&mut v);
    // AC1: `+op` lists the opus row first, ahead of the flags that merely
    // contain the letters; Tab pins the model chip and the prompt stays empty.
    press(&mut v, b"+op");
    assert_eq!(picker_labels(&v).first().map(String::as_str), Some("opus"));
    press(&mut v, b"\t");
    let l = v.launcher.as_ref().unwrap();
    assert!(l.picker.is_none());
    assert_eq!(l.draft.model_row.as_deref(), Some("opus"));
    assert!(l.draft.message.is_empty(), "the sigil never lands");
    // AC2: `:wor` Enter runs the worktree box's own toggle.
    let before = v.launcher.as_ref().unwrap().draft.worktree;
    press(&mut v, b":wor\r");
    let after = v.launcher.as_ref().unwrap().draft.worktree;
    assert!(
        after.is_some() && after != before,
        "the worktree box toggled"
    );
    // AC11: a bare `!` is still the shell tab and opens no list.
    let (mut v, _fx3) = pill_view(&_lock);
    press(&mut v, b"!");
    let l = v.launcher.as_ref().unwrap();
    assert!(l.shell && l.picker.is_none());
}

#[test]
fn composer_complete_spaced_sigil_and_recent_rank() {
    let (mut v, _lock, _fx) = pill_harness_view();
    // AC3: a phone sends `+`, space, word, space. One match commits as a pill.
    press(&mut v, b"+ verbose ");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.pills, vec![("--verbose".to_string(), None)]);
    assert!(l.draft.message.is_empty());
    // No match folds the list and gives the text back byte for byte.
    press(&mut v, b"+ Zzz ");
    let l = v.launcher.as_ref().unwrap();
    assert!(l.picker.is_none());
    assert_eq!(l.draft.message, "+ Zzz ");
    // AC5: a recent flag ranks first among the matches.
    let (v, _fx2) = pill_view(&_lock);
    let l = v.launcher.as_ref().unwrap();
    let mut recent = super::agent_launcher::grammar::Recent::new();
    recent.insert("flags:claude".to_string(), vec!["--verbose".to_string()]);
    let (rows, _) = super::agent_launcher::grammar::merged_rows(
        l,
        '+',
        &v.launcher_catalog,
        &v.backlog,
        &recent,
    );
    let first = rows.iter().find_map(|r| match r {
        crate::popup::PopupRow::Entry { label, glyph, .. } => Some((label.clone(), glyph.clone())),
        _ => None,
    });
    assert_eq!(
        first.map(|(label, glyph)| (label, glyph == super::agent_launcher::grammar::RECENT_GLYPH)),
        Some(("--verbose".to_string(), true))
    );
}

#[test]
fn composer_will_run_line_renders_and_edits_back() {
    let (mut v, _lock, _fx) = pill_harness_view();
    with_models(&mut v);
    press(&mut v, b":opus\t");
    // AC6: the line is the spawn command, from the same builder the launch
    // uses, and a draft the card would refuse shows the refusal instead.
    let line = super::agent_launcher::will_run::line(&v).unwrap();
    for want in ["fno agents spawn", "--harness claude", "--model opus"] {
        assert!(line.contains(want), "{want:?} in {line:?}");
    }
    let l = v.launcher.as_mut().unwrap();
    l.draft.provider = "zai".into();
    let model = std::mem::take(&mut l.draft.model);
    let refused = super::agent_launcher::will_run::line(&v).unwrap_err();
    assert!(refused.contains("choose a model"), "{refused}");
    let l = v.launcher.as_mut().unwrap();
    l.draft.provider.clear();
    l.draft.model = model;
    // AC7: an edit parses back into the chips and pills.
    let edit = |v: &mut View, f: &dyn Fn(&str) -> String| {
        v.launcher.as_mut().unwrap().focus = Focus::WillRun;
        super::agent_launcher::will_run::begin(v);
        let w = v.launcher.as_mut().unwrap().will_edit.as_mut().unwrap();
        w.text = f(&w.orig);
        press(v, b"\r");
    };
    edit(&mut v, &|t| {
        t.replace("--model opus", "--model sonnet")
            .replace("--prompt-file", "--search --prompt-file")
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.model_row.as_deref(), Some("sonnet"));
    assert_eq!(l.draft.pills, vec![("--search".to_string(), None)]);
    assert_eq!(l.focus, Focus::Message, "an applied edit folds back");
    // An unknown model refuses inline and leaves the chips as they were.
    edit(&mut v, &|t| t.replace("--model sonnet", "--model nope"));
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.model_row.as_deref(), Some("sonnet"));
    assert_eq!(l.focus, Focus::WillRun);
    let error = l.will_edit.as_ref().unwrap().error.clone().unwrap();
    assert!(error.contains("not in the claude list"), "{error}");
    // Enter on an unedited line changes nothing.
    edit(&mut v, &|t| t.to_string());
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.model_row.as_deref(), Some("sonnet"));
    assert_eq!(l.focus, Focus::Message);
}

#[test]
fn composer_rest_state_unfolds_on_tab_and_keeps_perms_beside_harness() {
    let (mut v, _lock, _fx) = pill_harness_view();
    // `[less]` is the one word an unfolded card always paints; the chip
    // labels drop at a narrow width.
    let folded = |v: &View| !card_texts(v).iter().any(|t| t.contains("[less]"));
    // AC8: Tab from the prompt paints the chip rows, Tab back to it folds them.
    assert!(folded(&v));
    press(&mut v, b"\t");
    assert!(!folded(&v));
    press(&mut v, b"\x1b[Z");
    assert_eq!(v.launcher.as_ref().unwrap().focus, Focus::Message);
    assert!(folded(&v));
    // AC9: at 60 columns the perms token stays on the harness line.
    v.launcher.as_mut().unwrap().focus = Focus::Permission;
    let l = v.launcher.as_ref().unwrap();
    let texts: Vec<String> = l.card_lines(&v, 60).into_iter().map(|c| c.text).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("claude") && t.contains("auto")),
        "perms sits beside harness: {texts:?}"
    );
}

fn two_saved_sets(v: &mut View) {
    use super::agent_launcher::sets::ComposerSet;
    let set = |name: &str, model: &str, effort: &str, place: &str| ComposerSet {
        name: name.to_string(),
        harness: "claude".to_string(),
        model: model.to_string(),
        model_row: Some(model.to_string()),
        effort: effort.to_string(),
        permission: "plan".to_string(),
        placement: place.to_string(),
        worktree: Some(true),
        ..ComposerSet::default()
    };
    if let Some(l) = v.launcher.as_mut() {
        l.sets.list = vec![
            set("daily", "opus", "high", "thread"),
            set("bulk", "sonnet", "low", "tab"),
        ];
    }
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_sets_apply_by_digit_by_name_and_by_tap() {
    // AC12-HP: with two saved sets, a digit off the prompt, `:name` in the
    // prompt and a tap on the more fold's row each land the set's axes.
    let (mut v, _lock, _fx) = pill_harness_view();
    with_models(&mut v);
    two_saved_sets(&mut v);
    v.launcher.as_mut().unwrap().focus = Focus::Harness;
    press(&mut v, b"2");
    let d = &v.launcher.as_ref().unwrap().draft;
    assert_eq!(
        (
            d.model_row.as_deref(),
            d.effort.as_str(),
            d.permission.as_str()
        ),
        (Some("sonnet"), "low", "plan")
    );
    assert_eq!(d.worktree, Some(true));
    assert_eq!(d.placement, super::agent_launcher::Placement::ThreadNewTab);
    v.launcher.as_mut().unwrap().focus = Focus::Message;
    press(&mut v, b":daily ");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        (l.draft.model_row.as_deref(), l.draft.effort.as_str()),
        (Some("opus"), "high")
    );
    assert!(l.draft.message.is_empty(), "the sigil never lands");
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        card_hit(&mut v, CardHit::More, &mut sock).await.unwrap();
        let labels = picker_labels(&v);
        assert_eq!(&labels[..2], ["daily", "bulk"]);
        assert_eq!(labels.last().map(String::as_str), Some("save as set"));
        card_hit(&mut v, CardHit::Pick(1), &mut sock).await.unwrap();
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.model_row.as_deref(), Some("sonnet"));
}

#[allow(clippy::indexing_slicing)]
#[test]
fn composer_sets_save_as_set_replaces_a_name_and_names_a_missing_harness() {
    let (mut v, _lock, _fx) = pill_harness_view();
    with_models(&mut v);
    two_saved_sets(&mut v);
    // Saving under an existing name replaces that set: the edit path.
    let l = v.launcher.as_mut().unwrap();
    l.draft.effort = "medium".to_string();
    super::agent_launcher::sets::begin_naming(l);
    press(&mut v, b"Bulk\r");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.sets.list.len(), 2);
    assert_eq!(l.sets.list[1].effort, "medium");
    assert_eq!(super::agent_launcher::sets::Sets::load().list, l.sets.list);
    // A set naming a harness the catalog lost applies the rest and says so.
    v.launcher.as_mut().unwrap().sets.list[0].harness = "gone".to_string();
    v.launcher.as_mut().unwrap().focus = Focus::Harness;
    press(&mut v, b"1");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.effort, "high");
    assert!(l.sets.notice.as_deref().unwrap().contains("gone"));
}
