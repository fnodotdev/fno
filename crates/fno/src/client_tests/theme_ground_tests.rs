//! The theme-switch ground repaint and user-theme tests, moved out of
//! client_tests.rs for the shrink-only budget.

use super::tests::two_pane_view;
use super::*;

#[test]
fn apply_theme_stages_the_ground_repaint_and_the_drain_applies_it() {
    // Paint on: a named theme stages OSC 11 bytes and the base as the
    // compositor ground; the drain consumes it, moves the ground, and
    // latches the exit restore.
    let theme = Theme::from_name("footnote-superscript").0;
    let staged = theme_ground::ground_repaint(&theme, true).expect("paint on stages a repaint");
    assert!(
        staged.osc.starts_with(b"\x1b]11;"),
        "OSC 11 leads the set bytes: {:?}",
        String::from_utf8_lossy(&staged.osc[..24.min(staged.osc.len())])
    );
    let hex = crate::theme::color_hex(theme.base).unwrap();
    assert!(
        String::from_utf8_lossy(&staged.osc).contains(hex.as_str()),
        "the theme base rides the set bytes"
    );
    assert_eq!(staged.ground, Some(theme.base));

    // terminal paints no ground: the switch RESTORES the user's scheme.
    let term = Theme::from_name("terminal").0;
    let staged = theme_ground::ground_repaint(&term, true).unwrap();
    assert_eq!(staged.osc, crate::theme::GROUND_RESTORE.to_vec());
    assert_eq!(staged.ground, None);

    // The kill switch off stages nothing - the takeover never ran.
    assert!(theme_ground::ground_repaint(&theme, false).is_none());

    // The INFERRED light pick (no config named a theme; COLORFGBG
    // picked paper) never repaints the ground - the terminal's own light bg
    // and fg stay. An explicit paper pick and the dark default keep it.
    let paper = Theme::from_name("footnote-paper").0;
    assert!(
        !theme_ground::ground_paint_allowed(true, true, &paper),
        "inferred paper must not paint"
    );
    assert!(
        theme_ground::ground_paint_allowed(true, false, &paper),
        "explicit paper still paints"
    );
    assert!(
        theme_ground::ground_paint_allowed(true, true, &theme),
        "the dark default keeps its paint"
    );
    assert!(
        !theme_ground::ground_paint_allowed(false, false, &paper),
        "the kill switch still wins"
    );

    // The drain applies all three effects.
    let mut v = two_pane_view();
    v.pending_ground = Some(theme_ground::PendingGround {
        osc: crate::theme::GROUND_RESTORE.to_vec(),
        ground: Some(theme.base),
    });
    let mut comp = compositor::Compositor::new(None);
    assert_eq!(comp.ground(), None);
    let mut guard = launch::TerminalGuard::test_guard();
    theme_ground::drain_pending_ground(&mut v, &mut comp, &mut guard);
    assert!(v.pending_ground.is_none(), "the drain consumes the stage");
    assert_eq!(comp.ground(), Some(theme.base), "the ground moved");
    assert!(guard.ground_latched(), "exit restore latched");
}

#[test]
fn osc11_replies_parse_to_an_srgb_triple() {
    use crate::digest_overlay::parse_osc11_rgb;
    // The two terminators every answering emulator uses, four-digit rgb.
    assert_eq!(
        parse_osc11_rgb(b"\x1b]11;rgb:1c1c/1c1c/1c1c\x07"),
        Some((0x1c, 0x1c, 0x1c))
    );
    assert_eq!(
        parse_osc11_rgb(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
        Some((255, 255, 255))
    );
    // Two-digit channels scale by 17, and rgba's alpha is read and dropped.
    assert_eq!(
        parse_osc11_rgb(b"\x1b]11;rgb:ff/00/88\x07"),
        Some((255, 0, 136))
    );
    assert_eq!(
        parse_osc11_rgb(b"\x1b]11;rgba:3333/4444/5555/ffff\x07"),
        Some((0x33, 0x44, 0x55))
    );
    // A prefixed reply (queued bytes ahead of it) still parses.
    assert_eq!(
        parse_osc11_rgb(b"\x1b[?1;2cx1b]11;rgb:abab/cdcd/efef\x07"),
        Some((0xab, 0xcd, 0xef))
    );
    // Junk never invents a color.
    assert_eq!(parse_osc11_rgb(b""), None);
    assert_eq!(parse_osc11_rgb(b"\x1b]10;rgb:11/22/33\x07"), None);
    assert_eq!(parse_osc11_rgb(b"\x1b]11;rgb:zz/22/33\x07"), None);
    assert_eq!(parse_osc11_rgb(b"\x1b]11;rgb:11111/22/33\x07"), None);
}

#[test]
fn the_ground_signal_ladder_prefers_the_osc_answer() {
    // The seam the client feeds: an OSC 11 answer decides, and only a
    // terminal that never answers leaves the COLORFGBG ladder to run.
    let scratch = tempfile::tempdir().expect("scratch");
    let (t, _, inferred) = crate::digest_overlay::theme_resolved(scratch.path(), Some(true));
    assert!(inferred, "no config named a theme");
    assert_eq!(t.name, "footnote-paper", "an OSC light answer picks paper");
    let (t, _, _) = crate::digest_overlay::theme_resolved(scratch.path(), Some(false));
    assert_eq!(
        t.name, "footnote-superscript",
        "an OSC dark answer picks superscript"
    );
}

#[test]
fn rgb_luminance_splits_the_theme_pair() {
    use crate::digest_overlay::rgb_is_light;
    // The shipped band pair: the paper band reads light, the superscript
    // band dark, and both sit far enough from the 0.5 line that a terminal
    // reporting either lands on the intended side.
    assert!(rgb_is_light((0xff, 0xff, 0xff)));
    assert!(rgb_is_light((0xdc, 0xdc, 0xdc)));
    assert!(!rgb_is_light((0x33, 0x33, 0x33)));
    assert!(!rgb_is_light((0x00, 0x00, 0x00)));
    assert!(!rgb_is_light((0x80, 0x80, 0x80)));
}
