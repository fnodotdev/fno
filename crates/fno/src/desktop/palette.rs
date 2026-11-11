//! Mux cell colors to `0xRRGGBB` for the desktop grid. The ANSI 16 come from
//! the active mux theme, so the app and the TUI paint the same palette.

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
    /// The recommended answer and a seat at work.
    pub ok: u32,
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
}
