//! Box drawing (U+2500-257F) and block elements (U+2580-259F) as filled
//! rects, so lines meet across cells. A font glyph for these leaves gaps
//! when the cell is taller than the font's ascent plus descent.

/// One filled rect inside a cell, in pixels from the cell's top-left, with
/// the alpha the shade blocks use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

/// The arm weights of U+2500-257F, four digits per char: up, right, down,
/// left. 0 none, 1 light, 2 heavy, 3 double. `x` is a diagonal, drawn by the
/// font. Dashed lines draw solid; rounded corners draw square.
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "0101", "0202", "1010", "2020", // 2500
    "0101", "0202", "1010", "2020", "0110", "0210", "0120", "0220", // 2508
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", // 2510
    "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120", // 2518
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", // 2520
    "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212", // 2528
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", // 2530
    "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212", // 2538
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", // 2540
    "1222", "2122", "2221", "2222", "0101", "0202", "1010", "2020", // 2548
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // 2550
    "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // 2558
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", // 2560
    "3101", "3303", "1313", "3131", "3333", "0110", "0011", "1001", // 2568
    "1100", "x", "x", "x", "0001", "1000", "0100", "0010", // 2570
    "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010", // 2578
];

/// The rects for `c` in a `w` x `h` cell with light lines `light` px wide,
/// or `None` for a char the font draws.
pub fn quads(c: char, w: f32, h: f32, light: f32) -> Option<Vec<Quad>> {
    let code = c as u32;
    match code {
        0x2500..=0x257f => lines(ARMS[(code - 0x2500) as usize], w, h, light),
        0x2580..=0x259f => Some(blocks(code, w, h)),
        _ => None,
    }
}

fn quad(x: f32, y: f32, w: f32, h: f32) -> Quad {
    Quad {
        x,
        y,
        w,
        h,
        alpha: 1.0,
    }
}

fn lines(arms: &str, w: f32, h: f32, light: f32) -> Option<Vec<Quad>> {
    let weights: Vec<u8> = arms.bytes().map(|b| b.wrapping_sub(b'0')).collect();
    if weights.len() != 4 || weights.iter().any(|&v| v > 3) {
        return None;
    }
    let (cx, cy) = ((w / 2.0).floor(), (h / 2.0).floor());
    // (offset of a stroke's near edge from the centre, its width)
    let strokes = |weight: u8| -> Vec<(f32, f32)> {
        match weight {
            0 => vec![],
            1 => vec![(-(light / 2.0).floor(), light)],
            2 => vec![(-light, light * 2.0)],
            _ => vec![(-light * 1.5, light), (light * 0.5, light)],
        }
    };
    // The span the crossing strokes cover around the centre. An arm reaches
    // exactly across it, so a corner closes with no notch and no nub.
    let span = |a: u8, b: u8| -> (f32, f32) {
        let all: Vec<(f32, f32)> = strokes(a).into_iter().chain(strokes(b)).collect();
        let lo = all.iter().map(|s| s.0).fold(0.0, f32::min);
        let hi = all.iter().map(|s| s.0 + s.1).fold(0.0, f32::max);
        (lo, hi)
    };
    let (vlo, vhi) = span(weights[0], weights[2]);
    let (hlo, hhi) = span(weights[1], weights[3]);
    let mut out = Vec::new();
    for (dir, &weight) in weights.iter().enumerate() {
        for (off, thick) in strokes(weight) {
            out.push(match dir {
                0 => quad(cx + off, 0.0, thick, cy + hhi),
                1 => quad(cx + vlo, cy + off, w - cx - vlo, thick),
                2 => quad(cx + off, cy + hlo, thick, h - cy - hlo),
                _ => quad(0.0, cy + off, cx + vhi, thick),
            });
        }
    }
    Some(out)
}

fn blocks(code: u32, w: f32, h: f32) -> Vec<Quad> {
    let eighth = |n: u32| n as f32 / 8.0;
    let (hw, hh) = (w / 2.0, h / 2.0);
    let ul = quad(0.0, 0.0, hw, hh);
    let ur = quad(hw, 0.0, w - hw, hh);
    let ll = quad(0.0, hh, hw, h - hh);
    let lr = quad(hw, hh, w - hw, h - hh);
    match code {
        0x2580 => vec![quad(0.0, 0.0, w, hh)],
        // Lower 1/8 .. full block.
        0x2581..=0x2588 => {
            let part = h * eighth(code - 0x2580);
            vec![quad(0.0, h - part, w, part)]
        }
        // Left 7/8 .. left 1/8.
        0x2589..=0x258f => vec![quad(0.0, 0.0, w * eighth(0x2590 - code), h)],
        0x2590 => vec![quad(hw, 0.0, w - hw, h)],
        0x2591..=0x2593 => vec![Quad {
            alpha: (code - 0x2590) as f32 * 0.25,
            ..quad(0.0, 0.0, w, h)
        }],
        0x2594 => vec![quad(0.0, 0.0, w, h * eighth(1))],
        0x2595 => vec![quad(w * eighth(7), 0.0, w * eighth(1), h)],
        0x2596 => vec![ll],
        0x2597 => vec![lr],
        0x2598 => vec![ul],
        0x2599 => vec![ul, ll, lr],
        0x259a => vec![ul, lr],
        0x259b => vec![ul, ur, ll],
        0x259c => vec![ul, ur, lr],
        0x259d => vec![ur],
        0x259e => vec![ur, ll],
        _ => vec![ur, ll, lr],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_reach_every_edge_they_name_and_blocks_fill_their_part() {
        let (w, h) = (8.0, 17.0);
        // ─ spans the cell; │ spans it top to bottom; ┌ touches only the
        // right and bottom edges.
        let horiz = quads('─', w, h, 1.0).unwrap();
        assert_eq!(horiz.len(), 2);
        let left = horiz.iter().map(|q| q.x).fold(f32::MAX, f32::min);
        let right = horiz.iter().map(|q| q.x + q.w).fold(0.0, f32::max);
        assert_eq!((left, right), (0.0, w));
        let vert = quads('│', w, h, 1.0).unwrap();
        let top = vert.iter().map(|q| q.y).fold(f32::MAX, f32::min);
        let bottom = vert.iter().map(|q| q.y + q.h).fold(0.0, f32::max);
        assert_eq!((top, bottom), (0.0, h));
        let corner = quads('┌', w, h, 1.0).unwrap();
        assert!(corner.iter().all(|q| q.x > 0.0 && q.y > 0.0));
        // ═ draws two strokes per arm; ━ one stroke twice as wide.
        assert_eq!(quads('═', w, h, 1.0).unwrap().len(), 4);
        assert_eq!(quads('━', w, h, 1.0).unwrap()[0].h, 2.0);
        // ▄ is the lower half, █ the whole cell, ▒ a half shade.
        assert_eq!(quads('▄', w, h, 1.0).unwrap(), vec![quad(0.0, 8.5, w, 8.5)]);
        assert_eq!(quads('█', w, h, 1.0).unwrap(), vec![quad(0.0, 0.0, w, h)]);
        assert_eq!(quads('▒', w, h, 1.0).unwrap()[0].alpha, 0.5);
        // A diagonal and plain text stay with the font.
        assert_eq!(quads('╱', w, h, 1.0), None);
        assert_eq!(quads('a', w, h, 1.0), None);
    }
}
