//! Mux cell colors to `0xRRGGBB` for the desktop grid. The ANSI 16 come from
//! the active mux theme, so the app and the TUI paint the same palette.
//!
//! The chrome takes three surfaces that step by luminance (ground, panel,
//! raised), three text levels (fg, muted, faint) and one hued accent. Status
//! hues go on glyphs, never on words.

use std::path::{Path, PathBuf};

use super::link::Layout;
use fno::proto::Color;
use fno::theme::{terminal16_slot, Theme};

/// xterm's own ANSI 16, for a theme that defines no terminal palette.
const XTERM16: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Primary text.
    pub fg: u32,
    /// The window ground: the main area.
    pub bg: u32,
    /// The one hued slot: links, the installed check, the recommended answer,
    /// the one primary control and the terminal selection. Never the text
    /// white.
    pub accent: u32,
    /// Hairlines: the sidebar edge, the title bar baseline, split dividers.
    pub border: u32,
    /// A seat at work and an answered question.
    pub ok: u32,
    /// The status line's text when the connection has closed.
    pub error: u32,
    /// Secondary text: labels and facts. The values beside them keep `fg`.
    pub muted: u32,
    /// Tertiary text: column heads and key hints, the quietest level.
    pub faint: u32,
    /// The sidebar's surface: one luminance step off the ground.
    pub panel: u32,
    /// Fields, the user's chat turns, popovers and tags: a step above `panel`.
    pub raised: u32,
    /// A row that needs the user.
    pub warn: u32,
    /// The fill of a hovered row: the lightest step off `panel`.
    pub hover: u32,
    /// The fill of the selected row: a step above `hover`, so two rows never
    /// read as selected.
    pub row_fill: u32,
    /// The fill under a press: a step above `row_fill`. Every label color is
    /// held at 4.5:1 on this, the strongest fill, so it holds on the rest.
    pub pressed: u32,
    ansi: [u32; 16],
}

fn pack(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// `a` moved toward `b` by `t`, per channel.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch =
        |v: u32, s: u32| ((v >> s & 0xff) as f32 * (1.0 - t) + (b >> s & 0xff) as f32 * t) as u8;
    pack(ch(a, 16), ch(a, 8), ch(a, 0))
}

/// WCAG contrast ratio of two `0xRRGGBB` colors.
pub fn contrast(a: u32, b: u32) -> f32 {
    let lin = |v: u32| {
        let c = (v & 0xff) as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = |c: u32| 0.2126 * lin(c >> 16) + 0.7152 * lin(c >> 8) + 0.0722 * lin(c);
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `from` moved toward `to` in 5% steps until it reads `min`:1 on `ground`.
/// The result stays as dim as that allows.
fn mix_to_contrast(from: u32, to: u32, ground: u32, min: f32) -> u32 {
    (0..=20)
        .map(|i| mix(from, to, i as f32 * 0.05))
        .find(|c| contrast(*c, ground) >= min)
        .unwrap_or(to)
}

fn rgb_or(c: Color, fallback: u32) -> u32 {
    match c {
        Color::Rgb(r, g, b) => pack(r, g, b),
        _ => fallback,
    }
}

/// HSL saturation of a `0xRRGGBB` color.
fn saturation(c: u32) -> f32 {
    let ch = |s: u32| (c >> s & 0xff) as f32 / 255.0;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    let l = (hi + lo) / 2.0;
    if hi == lo {
        return 0.0;
    }
    (hi - lo) / (1.0 - (2.0 * l - 1.0).abs())
}

/// The theme's one hue: its terminal blue, else its own hued title, border
/// or brand, else xterm blue. A gray candidate is skipped, so a monochrome
/// brand never makes the text white the accent.
fn accent(t: &Theme, bg: u32) -> u32 {
    if let Some(c) = terminal16_slot(4, t) {
        return rgb_or(c, XTERM16[4]);
    }
    [t.title, t.border, t.brand]
        .into_iter()
        .filter_map(|c| match c {
            Color::Rgb(r, g, b) => Some(pack(r, g, b)),
            _ => None,
        })
        .find(|c| saturation(*c) >= 0.3 && contrast(*c, bg) >= 3.0)
        .unwrap_or(XTERM16[4])
}

/// Slots 16-231 are the 6x6x6 cube, 232-255 the 24-step gray ramp.
fn xterm256(i: u8) -> u32 {
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        return pack(v, v, v);
    }
    let i = i - 16;
    let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
    pack(level(i / 36), level((i / 6) % 6), level(i % 6))
}

/// Whether a macOS window appearance is one of the light twins.
pub fn appearance_is_light(a: gpui::WindowAppearance) -> bool {
    matches!(
        a,
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
    )
}

/// The palette for a window appearance. The mux theme ladder decides, so a
/// `mux.theme` pin and the user's own themes win over `light`, as in the TUI.
/// The second value is the ladder's warning text, if any.
pub fn for_appearance(light: bool, cwd: &Path) -> (Palette, Option<String>) {
    let (theme, warn, _) = fno::app_api::digest_overlay::theme_resolved(cwd, Some(light));
    (Palette::from_theme(&theme), warn.map(|w| w.0))
}

/// The directory the active session works in: where its project's
/// `mux.theme` pin lives. None until the first layout names a squad.
pub fn project_dir(layout: &Layout) -> Option<PathBuf> {
    layout
        .squads
        .iter()
        .find(|s| s.id == layout.active_squad)
        .map(|s| s.canonical_cwd.as_str())
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

impl Palette {
    pub fn from_theme(t: &Theme) -> Self {
        let mut ansi = XTERM16;
        for (slot, out) in ansi.iter_mut().enumerate() {
            if let Some(c) = terminal16_slot(slot as u8, t) {
                *out = rgb_or(c, *out);
            }
        }
        let (fg, bg) = (rgb_or(t.stamp, 0xe8e8e8), rgb_or(t.base, 0x141414));
        let (panel, raised) = (mix(bg, fg, 0.04), mix(bg, fg, 0.08));
        let (hover, row_fill, pressed) = (
            mix(panel, fg, 0.03),
            mix(panel, fg, 0.07),
            mix(panel, fg, 0.11),
        );
        // A fill only lowers a label's contrast, and `pressed` lowers it
        // most: lift each label to 4.5:1 there when the theme's own falls short.
        let on_fills = |c: u32| mix_to_contrast(c, fg, pressed, 4.5);
        Palette {
            fg,
            bg,
            accent: on_fills(accent(t, bg)),
            border: mix(bg, fg, 0.12),
            ok: on_fills(rgb_or(t.ok, 0x9cc49c)),
            error: on_fills(rgb_or(t.error, 0xe1a6a3)),
            // Secondary walks up from the ground, not down from the theme's
            // dim, so it sits well below `fg` and still reads 4.5:1.
            muted: on_fills(mix(bg, fg, 0.4)),
            faint: mix_to_contrast(mix(bg, fg, 0.3), fg, pressed, 3.0),
            panel,
            raised,
            warn: on_fills(rgb_or(t.needs_you, 0xe5c07b)),
            hover,
            row_fill,
            pressed,
            ansi,
        }
    }

    fn resolve(&self, c: Color, default: u32) -> u32 {
        match c {
            Color::Default => default,
            Color::Indexed(i) if i < 16 => self.ansi[i as usize],
            Color::Indexed(i) => xterm256(i),
            Color::Rgb(r, g, b) => pack(r, g, b),
        }
    }

    /// A status glyph's color: blocked reads as an error, work in progress
    /// as ok, the rest muted. The word beside it stays muted text.
    pub fn status(&self, word: &str) -> u32 {
        match word {
            "blocked" => self.error,
            "writing" | "working" => self.ok,
            "parked" => self.warn,
            _ => self.faint,
        }
    }

    pub fn fg(&self, c: Color) -> u32 {
        self.resolve(c, self.fg)
    }

    pub fn bg(&self, c: Color) -> u32 {
        self.resolve(c, self.bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_resolve_through_theme_cube_and_ramp() {
        let t = Theme::default_theme();
        let p = Palette::from_theme(&t);
        assert_eq!(p.fg(Color::Default), rgb_or(t.stamp, 0));
        assert_eq!(p.bg(Color::Default), rgb_or(t.base, 0));
        let red = terminal16_slot(1, &t)
            .map(|c| rgb_or(c, 0))
            .unwrap_or(XTERM16[1]);
        assert_eq!(p.fg(Color::Indexed(1)), red);
        assert_eq!(p.fg(Color::Indexed(16)), 0x000000);
        assert_eq!(p.fg(Color::Indexed(196)), 0xff0000);
        assert_eq!(p.fg(Color::Indexed(232)), 0x080808);
        assert_eq!(p.fg(Color::Indexed(255)), 0xeeeeee);
        assert_eq!(p.bg(Color::Rgb(1, 2, 3)), 0x010203);
    }

    #[test]
    fn appearance_picks_the_twin_and_a_pin_wins() {
        let paper = Palette::from_theme(&Theme::default_for(true));
        let dark = Palette::from_theme(&Theme::default_for(false));
        assert!(appearance_is_light(gpui::WindowAppearance::VibrantLight));
        assert!(!appearance_is_light(gpui::WindowAppearance::Dark));
        assert_eq!((paper.bg, paper.fg), (0xf7f7f7, 0x141414));
        assert_ne!(paper.bg, dark.bg);
        let pinned = Palette::from_theme(&Theme::from_name("gruvbox").0);
        assert_ne!(pinned.bg, paper.bg);

        // The appearance path itself, with a pin in the project's config: a
        // light window takes the pin, read from the directory it is given.
        let dir = std::env::temp_dir().join(format!("fno-palette-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".fno")).unwrap();
        std::fs::write(dir.join(".fno/config.toml"), "[mux]\ntheme = \"gruvbox\"\n").unwrap();
        let (live, _) = for_appearance(true, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(live.bg, pinned.bg);

        // The pin's directory is the active squad's, never the process cwd.
        let squad = |id, cwd: &str| fno::proto::SquadMeta {
            id,
            name: String::new(),
            canonical_cwd: cwd.into(),
            tabs: Vec::new(),
            active_tab: 0,
            panes: 0,
        };
        let mut layout = Layout::default();
        assert_eq!(project_dir(&layout), None);
        layout.squads = vec![squad(1, "/a"), squad(2, "/b")];
        layout.active_squad = 2;
        assert_eq!(project_dir(&layout), Some(PathBuf::from("/b")));
    }

    /// The footnote twins: three surfaces a step apart, a secondary level
    /// that reads 4.5:1 on every one and sits well below `fg`, and a hued
    /// accent at 3:1 on the ground.
    #[test]
    fn surfaces_step_secondary_reads_and_the_accent_is_hued() {
        for name in ["footnote-superscript", "footnote-paper"] {
            let p = Palette::from_theme(&Theme::from_name(name).0);
            let surfaces = [p.bg, p.panel, p.raised, p.hover, p.row_fill, p.pressed];
            for s in surfaces {
                let c = contrast(p.muted, s);
                assert!(c >= 4.5, "{name}: muted on {s:06x} is {c:.2}:1");
                let c = contrast(p.faint, s);
                assert!(c >= 3.0, "{name}: faint on {s:06x} is {c:.2}:1");
            }
            for (a, b) in [(p.bg, p.panel), (p.panel, p.raised)] {
                let c = contrast(a, b);
                assert!(c >= 1.06, "{name}: {a:06x} and {b:06x} step {c:.3}:1");
            }
            let step = contrast(p.fg, p.bg) / contrast(p.muted, p.bg);
            assert!(step >= 2.0, "{name}: fg over muted is only {step:.2}x");
            assert!(contrast(p.accent, p.bg) >= 3.0, "{name}");
            assert!(saturation(p.accent) >= 0.3, "{name}");
        }
    }

    /// `terminal` has no terminal table and no hued title, border or brand:
    /// the accent falls back to a defined hue, never the text white.
    #[test]
    fn a_theme_with_no_hued_slot_still_gets_a_hued_readable_accent() {
        let t = Theme::from_name("terminal").0;
        assert!(terminal16_slot(4, &t).is_none());
        let p = Palette::from_theme(&t);
        assert_ne!(p.accent, p.fg);
        assert!(saturation(p.accent) >= 0.3);
        assert!(contrast(p.accent, p.bg) >= 3.0);
    }

    /// `terminal` inherits the emulator's palette, so it has no ground to
    /// measure against; every other built-in theme carries its own.
    #[test]
    fn built_in_themes_keep_the_accent_and_labels_readable() {
        for name in [
            "footnote-superscript",
            "footnote-paper",
            "catppuccin",
            "tokyo-night",
            "gruvbox",
        ] {
            let p = Palette::from_theme(&Theme::from_name(name).0);
            let c = contrast(p.accent, p.bg);
            assert!(c >= 4.5, "{name}: accent {:06x} is {c:.2}:1", p.accent);
            for (fill, what) in [
                (p.panel, "panel"),
                (p.raised, "raised"),
                (p.hover, "hover"),
                (p.row_fill, "row_fill"),
                (p.pressed, "pressed"),
            ] {
                for (label, text) in [
                    (p.fg, "fg"),
                    (p.muted, "muted"),
                    (p.warn, "warn"),
                    (p.error, "error"),
                    (p.ok, "ok"),
                    (p.accent, "accent"),
                ] {
                    let c = contrast(label, fill);
                    assert!(c >= 4.5, "{name}: {text} on {what} is {c:.2}:1");
                }
            }
        }
    }
}
