//! Mux cell colors to `0xRRGGBB` for the desktop grid. The ANSI 16 come from
//! the active mux theme, so the app and the TUI paint the same palette.

use std::path::Path;

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

impl Palette {
    pub fn from_theme(t: &Theme) -> Self {
        let mut ansi = XTERM16;
        for (slot, out) in ansi.iter_mut().enumerate() {
            if let Some(c) = terminal16_slot(slot as u8, t) {
                *out = rgb_or(c, *out);
            }
        }
        Palette {
            fg: rgb_or(t.stamp, 0xe8e8e8),
            bg: rgb_or(t.base, 0x141414),
            accent: rgb_or(t.border, 0xe8e8e8),
            border: rgb_or(t.sel, 0x2b2b2b),
            focus: mix(rgb_or(t.sel, 0x2b2b2b), rgb_or(t.stamp, 0xe8e8e8), 0.3),
            ok: rgb_or(t.ok, 0x9cc49c),
            recommend: ansi[4],
            error: rgb_or(t.error, 0xe1a6a3),
            muted: rgb_or(t.dim, 0x8a8a8a),
            panel: mix(rgb_or(t.base, 0x141414), rgb_or(t.stamp, 0xe8e8e8), 0.05),
            warn: rgb_or(t.needs_you, 0xe5c07b),
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

    fn contrast(a: u32, b: u32) -> f32 {
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
    }

    #[test]
    fn recommend_is_hued_and_readable_on_both_twins() {
        for light in [false, true] {
            let p = Palette::from_theme(&Theme::default_for(light));
            assert!(contrast(p.recommend, p.bg) >= 3.0, "light={light}");
            assert!(saturation(p.recommend) >= 0.3, "light={light}");
        }
    }

    #[test]
    fn recommend_falls_back_to_xterm_without_an_ansi_table() {
        let p = Palette::from_theme(&Theme::from_name("terminal").0);
        assert_eq!(p.recommend, XTERM16[4]);
    }
}
