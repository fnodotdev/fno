//! Mux cell colors to `0xRRGGBB` for the desktop grid. The ANSI 16 come from
//! the active mux theme, so the app and the TUI paint the same palette.

use std::path::{Path, PathBuf};

use super::link::Layout;
use crate::proto::Color;
use crate::theme::{terminal16_slot, Theme};

/// xterm's own ANSI 16, for a theme that defines no terminal palette.
const XTERM16: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub fg: u32,
    pub bg: u32,
    /// The focused pane's border.
    pub accent: u32,
    /// Every other pane border.
    pub border: u32,
    /// The focused pane's border: a step above `border`, never the text
    /// white.
    pub focus: u32,
    /// A seat at work and an answered question.
    pub ok: u32,
    /// The recommended answer's border and word: a hued slot, so it never
    /// reads as the `ok` state.
    pub recommend: u32,
    /// The status line's text when the connection has closed.
    pub error: u32,
    /// Labels and secondary text: the values beside them keep `fg`.
    pub muted: u32,
    /// The left pane's and the strip's background.
    pub panel: u32,
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
    /// The user's own turns in chat mode.
    pub bubble: u32,
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
    let (theme, warn, _) = crate::digest_overlay::theme_resolved(cwd, Some(light));
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
        let panel = mix(bg, fg, 0.05);
        let sel = rgb_or(t.sel, 0x2b2b2b);
        // A theme with no terminal table has no blue slot: take the theme's
        // own hued title, border or brand, the first that reads 4.5:1.
        let recommend = match terminal16_slot(4, t) {
            Some(c) => rgb_or(c, ansi[4]),
            None => [t.title, t.border, t.brand]
                .into_iter()
                .filter_map(|c| match c {
                    Color::Rgb(r, g, b) => Some(pack(r, g, b)),
                    _ => None,
                })
                .find(|c| contrast(*c, bg) >= 4.5)
                .unwrap_or(ansi[4]),
        };
        let (hover, row_fill, pressed) = (
            mix(panel, fg, 0.03),
            mix(panel, fg, 0.06),
            mix(panel, fg, 0.10),
        );
        // A fill only lowers a label's contrast, and `pressed` lowers it
        // most: lift each label to 4.5:1 there when the theme's own falls short.
        let on_fills = |c: u32| mix_to_contrast(c, fg, pressed, 4.5);
        Palette {
            fg,
            bg,
            accent: rgb_or(t.border, 0xe8e8e8),
            border: sel,
            // Dim, but a focus ring reads 3:1 on the ground.
            focus: mix_to_contrast(mix(sel, fg, 0.3), fg, bg, 3.0),
            ok: on_fills(rgb_or(t.ok, 0x9cc49c)),
            recommend: on_fills(recommend),
            error: on_fills(rgb_or(t.error, 0xe1a6a3)),
            muted: on_fills(rgb_or(t.dim, 0x8a8a8a)),
            panel,
            warn: on_fills(rgb_or(t.needs_you, 0xe5c07b)),
            hover,
            row_fill,
            pressed,
            bubble: mix(bg, fg, 0.12),
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

    /// A seat or agent status word's color: blocked reads as an error, work
    /// in progress as ok, the rest muted.
    pub fn status(&self, word: &str) -> u32 {
        match word {
            "blocked" => self.error,
            "writing" | "working" => self.ok,
            "parked" => self.warn,
            _ => self.muted,
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
        let squad = |id, cwd: &str| crate::proto::SquadMeta {
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

    #[test]
    fn recommend_is_hued_and_readable_on_both_twins() {
        for light in [false, true] {
            let p = Palette::from_theme(&Theme::default_for(light));
            assert!(contrast(p.recommend, p.bg) >= 3.0, "light={light}");
            assert!(saturation(p.recommend) >= 0.3, "light={light}");
        }
    }

    /// `terminal` inherits the emulator's palette, so it has no ground to
    /// measure against; every other built-in theme carries its own.
    #[test]
    fn built_in_themes_keep_recommend_focus_and_labels_readable() {
        for name in [
            "footnote-superscript",
            "footnote-paper",
            "catppuccin",
            "tokyo-night",
            "gruvbox",
        ] {
            let p = Palette::from_theme(&Theme::from_name(name).0);
            let c = contrast(p.recommend, p.bg);
            assert!(
                c >= 4.5,
                "{name}: recommend {:06x} is {c:.2}:1",
                p.recommend
            );
            let c = contrast(p.focus, p.bg);
            assert!(c >= 3.0, "{name}: focus {:06x} is {c:.2}:1", p.focus);
            for (fill, what) in [
                (p.panel, "panel"),
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
                    (p.recommend, "recommend"),
                ] {
                    let c = contrast(label, fill);
                    assert!(c >= 4.5, "{name}: {text} on {what} is {c:.2}:1");
                }
            }
        }
    }
}
