//! Tab labels: display width, condensing to fit, the brand and group labels,
//! and home-relative paths. Moved out of client.rs (file budget).

use super::*;

/// Display columns a sideline glyph occupies. The client draws chrome one glyph
/// per cell, so a double-width glyph must claim two columns (plus a WIDE_SPACER)
/// or it desyncs the rest of the row against a standards-compliant terminal.
/// ponytail: only the menu trigram block (U+2630..U+2637) and the mark's
/// full-width `Ｆ` (U+FF26) are wide today; widen this if a CJK/emoji glyph
/// ever lands here.
pub(super) fn glyph_cols(ch: char) -> usize {
    if ch == '\u{FF26}' || ('\u{2630}'..='\u{2637}').contains(&ch) {
        2
    } else {
        1
    }
}

/// Display columns a tab-strip span paints, wide glyphs counted - the one
/// width both the painter, the scroll-window fitter and the hit-test walk,
/// so a span's painted width and its clickable width can never disagree.
pub(super) fn tab_text_cols(text: &str) -> usize {
    text.chars().map(glyph_cols).sum()
}

/// Shrink `spans` until they fit `width`.
///
/// Columns come off in order of what the operator loses by it:
///
/// 1. the widest tab label, down to one character plus its brackets;
/// 2. then the squad label, the only span nothing navigates by;
/// 3. only then does a tab stop being shown at all.
///
/// Having 2 and 3 the wrong way round was not a matter of taste: on a supported
/// 40-column strip a long enough workspace name dropped the ACTIVE tab while its
/// own name sat there at full length.
///
/// The counters and the `+` are never shortened or shed. A counter squeezed into
/// `‹1 ` states a wrong count as confidently as a right one, and the `+` is the
/// only mouse route to a new tab. When step 3 does hide a tab the right-hand
/// counter takes it on, because a count computed before the hiding is precisely
/// the confidently-wrong number the rest of this function exists to avoid.
pub(super) fn condense_to_width(spans: &mut Vec<TabSpan>, width: usize) {
    let w = |s: &TabSpan| tab_text_cols(&s.text);
    let total = |v: &Vec<TabSpan>| v.iter().map(w).sum::<usize>();
    // Keeps the first and last character: the brackets or padding spaces that
    // mark the active tab, and the squad label's own surrounding spaces.
    let shrink = |s: &mut TabSpan| {
        let mut chars: Vec<char> = s.text.chars().collect();
        // A group marker must not outlive the name it marks. Columns
        // come off the right, so the `·N` count goes early - correct, it is the
        // least of the three. The leading glyph would otherwise survive to the
        // floor and leave a strip of identical nameless cells, which is worse
        // than no marker at all: before the marker existed each tab still kept
        // its first character. Once the count is gone and the name is down to
        // its last couple of characters, the glyph goes instead.
        let glyph_at = chars.iter().position(|&c| c == TAB_GROUP_GLYPH);
        let count_gone = !chars.contains(&TAB_GROUP_SEP);
        match glyph_at {
            Some(g) if count_gone && chars.len() <= GROUP_GLYPH_FLOOR => {
                // The glyph and the space after it.
                if chars.get(g + 1) == Some(&' ') {
                    chars.remove(g + 1);
                }
                chars.remove(g);
            }
            _ => {
                chars.remove(chars.len() - 2);
            }
        }
        s.text = chars.into_iter().collect();
    };
    while total(spans) > width {
        let widest_tab = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.role == SpanRole::Tab && w(s) > 3)
            .max_by_key(|(_, s)| w(s))
            .map(|(i, _)| i);
        if let Some(i) = widest_tab {
            shrink(&mut spans[i]);
            continue;
        }
        if let Some(squad) = spans
            .iter_mut()
            .find(|s| s.role == SpanRole::Squad && s.text.chars().count() > 3)
        {
            shrink(squad);
            continue;
        }
        let Some(i) = spans.iter().rposition(|s| s.role == SpanRole::Tab) else {
            break;
        };
        // The tab did not disappear, it became hidden, so it has to start being
        // counted. Its hit comes along: it is now the nearest hidden tab, and
        // the counter is what walks the strip back to it.
        let dropped = spans.remove(i);
        match spans.get_mut(i) {
            Some(counter) if matches!(counter.role, SpanRole::OverflowRight(_)) => {
                let SpanRole::OverflowRight(n) = counter.role else {
                    unreachable!()
                };
                counter.role = SpanRole::OverflowRight(n + 1);
                counter.text = format!(" {}\u{203a}", n + 1);
                counter.hit = dropped.hit;
            }
            _ => spans.insert(
                i,
                TabSpan {
                    text: " 1\u{203a}".to_string(),
                    flags: cell_flags::DIM,
                    fg: Color::Default,
                    hit: dropped.hit,
                    role: SpanRole::OverflowRight(1),
                },
            ),
        }
    }
}

/// One clickable span in the tab bar: label, render flags, and what a click does
/// (`None` = inert, e.g. the squad-name label).
#[derive(Clone)]
pub(super) struct TabSpan {
    pub(super) text: String,
    pub(super) flags: u8,
    /// (US4) The span's foreground: the accent when the tab's rollup is a
    /// Blocked pane, else `Color::Default`.
    pub(super) fg: Color,
    pub(super) hit: Option<TabHit>,
    pub(super) role: SpanRole,
}

/// What a strip span IS, rather than what its click happens to do.
///
/// Two review findings in a row came from reading a span's role off `hit` and
/// its length: an overflow counter carries a `Tab` hit, because clicking it
/// walks the strip, so by behaviour it is indistinguishable from a short tab
/// label. It is not, and [`condense_to_width`] treats the two very differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SpanRole {
    /// The workspace name. Inert, and the only span nothing navigates by, which
    /// is what makes it the right thing to shorten under pressure.
    Squad,
    /// A tab's own label.
    Tab,
    /// `‹N `: N tabs hidden to the left of the window.
    OverflowLeft(usize),
    /// ` N›`: N tabs hidden to the right. Carries its count so a strip that has
    /// to hide one more tab can say so, instead of reporting a number that was
    /// computed before the hiding happened.
    OverflowRight(usize),
    /// The `+`: the only mouse route to a new tab.
    NewTab,
}

#[derive(Clone, Copy)]
pub(super) enum TabHit {
    Tab(TabId),
    NewTab,
}

/// A named tab's visible label width in the tab bar / sideline;
/// keeps ~4 labeled tabs visible at 100 cols.
pub(super) const TAB_LABEL_W: usize = 14;

/// A tab span's label body: the bare 1-based ordinal when the
/// server-derived name carries no signal (the name IS the ordinal -
/// byte-for-byte today's render for a plain shell tab, AC1-EDGE), else
/// `{ordinal}:{name}` with the name truncated to [`TAB_LABEL_W`] chars. The
/// ordinal stays visible in every span because the `1-9 select tab` keys
/// key off it (Locked 5).
/// The mux's home workspace surfaces the bare brand `fno` in the tab strip
/// (: derived from the cwd basename); render it bracketed as `F[no]`.
/// Any other workspace name passes through unchanged.
pub(super) fn brand_label(name: &str) -> String {
    if name == "fno" {
        "F[no]".to_string()
    } else {
        name.to_string()
    }
}

pub fn tab_label_text(name: &str, i: usize, named: bool) -> String {
    let ordinal = (i + 1).to_string();
    // Collapse (AC7): a name equal to its own ordinal renders as
    // the bare digit, byte-identical to an unnamed ordinal - even a chosen one.
    if name == ordinal {
        return ordinal;
    }
    let short: String = name.chars().take(TAB_LABEL_W).collect();
    if named {
        // (US2, supersedes Locked 5) A chosen name renders alone,
        // never with a forced `{ordinal}:` prefix.
        short
    } else {
        // A pane-derived or ordinal fallback keeps today's `{ordinal}:{label}`.
        format!("{ordinal}:{short}")
    }
}

/// Mark a tab that holds more than one pane: the stacked glyph in front, the
/// pane count behind. A tab holding four panes rendered identically to one
/// holding a single pane, so an operator could not tell what a tab-level close
/// would destroy before pressing it.
///
/// Separate from [`tab_label_text`] rather than a parameter on it: that helper
/// is layout-free and its tests pin the ordinal-collapse rules, while a pane
/// count is a layout fact. One implementation, shared by the strip and the tab
/// menu's header, so the menu can never describe a different tab than the cell
/// the operator pressed.
///
/// A single-pane tab passes through unchanged, byte-identical to the render
/// before this existed.
pub(super) fn tab_group_label(label: String, panes: usize) -> String {
    if panes > 1 {
        format!("{TAB_GROUP_GLYPH} {label}{TAB_GROUP_SEP}{panes}")
    } else {
        label
    }
}

/// The stacked glyph a grouped tab leads with, and the separator before its
/// count. Named because [`condense_to_width`] has to recognize them: a marker
/// that outlives the name it marks turns a crowded strip into identical
/// nameless cells.
pub(super) const TAB_GROUP_GLYPH: char = '▤';

pub(super) const TAB_GROUP_SEP: char = '·';

/// Total span width at or below which [`condense_to_width`] drops a group glyph
/// rather than another name character: one pad, the glyph and its space, two
/// characters of name, one pad.
pub(super) const GROUP_GLYPH_FLOOR: usize = 6;

/// Abbreviate `$HOME` to `~` for the status row; only at a path-component
/// boundary so `/home/user2/...` never reads as `~2/...`.
pub(super) fn abbrev_home(p: &str) -> String {
    // var_os, not var: HOME is a path, and the idiomatic read for a path env
    // var avoids assuming UTF-8 up front. A non-UTF-8 HOME yields None and the
    // path renders unabbreviated. Cached in a OnceLock: this runs on every
    // frame compose (a hot path during output floods) and HOME is fixed for
    // the process lifetime, so the env lookup + global env lock happens once
    // (gemini).
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| std::env::var_os("HOME").and_then(|s| s.into_string().ok()));
    abbrev_home_in(p, home.as_deref())
}

pub(super) fn abbrev_home_in(p: &str, home: Option<&str>) -> String {
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        if let Some(rest) = p.strip_prefix(h) {
            if rest.is_empty() || rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}
