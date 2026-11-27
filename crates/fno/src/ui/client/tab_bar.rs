//! The tab bar: its spans, the window that fits the terminal width, and its
//! paint. Moved out of client.rs (file budget).

use super::*;

impl View {
    /// The tab-bar spans in paint order: the active squad's name (inert), its
    /// tabs, then a `+` new-tab affordance. The single source both `draw_tab_bar`
    /// and `chrome_hit` walk, so a click always lands on the glyph under it.
    pub(super) fn tab_bar_spans(&self) -> Vec<TabSpan> {
        let mut spans = Vec::new();
        // The brand mark is pinned at the tab bar's top-left in EVERY
        // workspace, before tab 1 (user ruling, 2026-09-27): `Ｆ` bold,
        // `[no]` the reverse-video stamp (draw_tab_bar splits the two
        // tones). It rides ahead of the workspace label, whatever it is.
        let text: String = wordmark::one_row().iter().map(|(s, _)| *s).collect();
        spans.push(TabSpan {
            text: format!(" {text} "),
            flags: cell_flags::BOLD,
            fg: Color::Default,
            hit: None,
            role: SpanRole::Squad,
        });
        let Some(s) = self
            .layout
            .squads
            .iter()
            .find(|s| s.id == self.layout.active_squad)
        else {
            return spans;
        };
        spans.push(TabSpan {
            text: format!(" {} ", brand_label(&s.name)),
            flags: cell_flags::BOLD,
            fg: Color::Default,
            hit: None,
            role: SpanRole::Squad,
        });
        for (i, t) in s.tabs.iter().enumerate() {
            let label = tab_group_label(tab_label_text(&t.name, i, t.named), t.panes.len());
            // US4: a leading max-severity rollup glyph so a background
            // tab's blocked/working pane reads at the strip without opening it,
            // carrying the lattice's weight and color. `None` (no live panes:
            // empty or all-exited) prepends nothing, so a stateless tab renders
            // byte-identically to today (AC2-EDGE); a live-idle tab shows `○`; a
            // Blocked rollup paints the amber accent on the span (label preceded
            // by `▲`), and the working/blocked/done glyphs keep their BOLD.
            let (glyph_prefix, fg, glyph_flags) =
                match tab_rollup_state(&self.layout.agents, s.id, t.id) {
                    Some(st) => {
                        let style = lattice_style(st, self.theme.needs_you, self.theme.ok);
                        (format!("{} ", style.glyph), style.fg, style.flags)
                    }
                    None => (String::new(), Color::Default, 0),
                };
            let label = format!("{glyph_prefix}{label}");
            let base_flags = if i == s.active_tab {
                cell_flags::INVERSE
            } else {
                0
            };
            // One cell of padding inside each side of every tab label: the
            // inactive shape already reads ` label `, the active brackets get
            // theirs as `[ label ]`.
            let text = if i == s.active_tab {
                format!("[ {label} ]")
            } else {
                format!(" {label} ")
            };
            spans.push(TabSpan {
                text,
                flags: base_flags | glyph_flags,
                fg,
                hit: Some(TabHit::Tab(t.id)),
                role: SpanRole::Tab,
            });
        }
        spans.push(TabSpan {
            text: " + ".to_string(),
            flags: cell_flags::DIM,
            fg: Color::Default,
            hit: Some(TabHit::NewTab),
            role: SpanRole::NewTab,
        });
        spans
    }

    /// The span run that actually FITS the strip: a scrolling window over
    /// [`tab_bar_spans`] anchored on the active tab, with a clickable overflow
    /// counter at whichever edge is hiding tabs.
    ///
    /// `draw_tab_bar` used to stop painting at the right edge, and `chrome_hit`
    /// walks the same spans, so a tab past it was invisible AND unclickable.
    ///
    /// Anchoring on the active tab means the existing keys already drive the
    /// viewport, so reaching a twentieth tab needs no new binding. The squad
    /// label and the `+` are pinned outside the scroll region: the `+` is the
    /// only mouse route to a new tab. Below overflow this returns
    /// [`tab_bar_spans`] unchanged.
    pub(super) fn tab_bar_window(&self) -> Vec<TabSpan> {
        // The bell's rightmost seat (plus one gap) is reserved before spans
        // condense, so the `+` affordance never paints or clicks under it.
        let width = (self.term.1 as usize)
            .saturating_sub(self.panel_w() as usize)
            .saturating_sub(bell::button_range(self).len() + 1);
        let span_w = |s: &TabSpan| tab_text_cols(&s.text);
        let full = self.tab_bar_spans();
        if full.iter().map(span_w).sum::<usize>() <= width {
            return full;
        }
        // [squad label][tab..][+]: the label leads and the `+` trails by
        // construction, so both peel off without searching for them.
        let Some(plus) = full
            .last()
            .filter(|s| matches!(s.hit, Some(TabHit::NewTab)))
        else {
            return full;
        };
        let (plus, label) = (plus.clone(), full[0].clone());
        let tabs: Vec<TabSpan> = full[1..full.len() - 1].to_vec();
        let avail = width
            .saturating_sub(span_w(&label))
            .saturating_sub(span_w(&plus));
        // `‹12 ` / ` 12›`: two glyph columns plus the count's digits.
        let marker_w = |n: usize| n.to_string().chars().count() + 2;
        // Pack forward from `start`, always reserving room for the counter that
        // will be needed at each edge, and return one past the last span that
        // fits. Reserving inside the loop (not after it) is what keeps the right
        // counter from being the thing that overflows the strip.
        let fit_end = |start: usize| -> usize {
            let mut used = if start > 0 { marker_w(start) } else { 0 };
            let mut end = start;
            while end < tabs.len() {
                let rest_after = tabs.len() - end - 1;
                let reserve = if rest_after > 0 {
                    marker_w(rest_after)
                } else {
                    0
                };
                if used + span_w(&tabs[end]) + reserve > avail {
                    break;
                }
                used += span_w(&tabs[end]);
                end += 1;
            }
            end
        };
        let active = self
            .layout
            .squads
            .iter()
            .find(|s| s.id == self.layout.active_squad)
            .map(|s| s.active_tab)
            .unwrap_or(0)
            .min(tabs.len().saturating_sub(1));
        // The smallest window that still contains the active tab AND one tab of
        // lookahead past it. Minimal scrolling means stepping back with
        // `prefix+p` does not yank a strip that already showed the target; the
        // lookahead means stepping forward with `prefix+n` slides the strip by
        // one instead of jumping it on every single press.
        let target = (active + 1).min(tabs.len().saturating_sub(1));
        let mut start = 0usize;
        while start < active && fit_end(start) <= target {
            start += 1;
        }
        let end = fit_end(start).max(start + 1).min(tabs.len());
        let mut out = vec![label];
        if start > 0 {
            out.push(TabSpan {
                text: format!("\u{2039}{start} "),
                flags: cell_flags::DIM,
                fg: Color::Default,
                // Clicking the counter selects the nearest tab it is hiding, so
                // the strip walks toward the hidden end one click at a time.
                hit: tabs[start - 1].hit,
                role: SpanRole::OverflowLeft(start),
            });
        }
        out.extend(tabs[start..end].iter().cloned());
        if end < tabs.len() {
            out.push(TabSpan {
                text: format!(" {}\u{203a}", tabs.len() - end),
                flags: cell_flags::DIM,
                fg: Color::Default,
                hit: tabs[end].hit,
                role: SpanRole::OverflowRight(tabs.len() - end),
            });
        }
        out.push(plus);
        // Narrow mode: `fit_end` can return `start` when not one whole tab fits
        // beside the pinned chrome, and forcing it in would push the counter and
        // the `+` off the edge to be clipped. A truncated label still says which
        // tab you are on and still hit-tests; a missing `+` is unreachable.
        condense_to_width(&mut out, width);
        out
    }

    pub(super) fn draw_tab_bar(&self, cells: &mut [Cell], cols: usize) {
        // (US1) The strip is scoped to the content area: it begins at
        // the content-column origin (`panel_w`) so the sideline column owns row
        // 0 too and tabs read as owned by the active workspace rather than the
        // reverse. `panel_w == 0` (no sideline) -> origin 0, byte-identical to
        // the pre-scoping full-width strip.
        // (G1) While a pane is dragged, the whole strip reads as a break
        // target (accent), brightening to INVERSE the moment the pointer is over
        // it (the "drop here to break into a new tab" affordance). (G2) While a
        // tab cell is dragged, that cell dims to read as lifted - the same
        // origin-marking the pane grip uses during a relocation.
        let break_active = self.pane_drag.map(|d| d.on_strip);
        let lifted_tab = self.tab_drag.map(|d| d.src_tab);
        let mut c = self.panel_w() as usize;
        'spans: for span in self.tab_bar_window() {
            let (fg, flags) = if let Some(on_strip) = break_active {
                let f = if on_strip {
                    cell_flags::INVERSE
                } else {
                    cell_flags::BOLD
                };
                (self.theme.brand, f)
            } else if matches!((lifted_tab, span.hit), (Some(t), Some(TabHit::Tab(tid))) if t == tid)
            {
                (span.fg, span.flags | cell_flags::DIM)
            } else {
                (span.fg, span.flags)
            };
            // The brand mark's `[no]` drops to the reverse-video stamp tone
            // for just those four chars; every other span paints uniform.
            let is_mark = span.role == SpanRole::Squad && span.text.trim() == crate::wordmark::TEXT;
            let (mark_fg, _, mark_flags) = cell_style(crate::theme::Role::Stamp, &self.theme);
            let mut in_no = false;
            for ch in span.text.chars() {
                if ch == '[' {
                    in_no = true;
                }
                let (fg, flags) = if is_mark && in_no {
                    (mark_fg, mark_flags)
                } else {
                    (fg, flags)
                };
                if c >= cols {
                    break 'spans;
                }
                cells[c] = Cell {
                    c: ch,
                    fg,
                    bg: Color::Default,
                    flags,
                };
                // A double-width glyph claims its second column (the spacer
                // every renderer skips), or the rest of the strip desyncs
                // against a standards-compliant terminal.
                let w = glyph_cols(ch);
                if w == 2 {
                    if c + 1 >= cols {
                        break 'spans;
                    }
                    cells[c + 1] = Cell {
                        c: ' ',
                        fg,
                        bg: Color::Default,
                        flags: flags | cell_flags::WIDE_SPACER,
                    };
                }
                c += w;
                if is_mark && in_no && ch == ']' {
                    in_no = false;
                }
            }
        }
        // The bell keeps the bar's rightmost seat (one column in from the
        // edge); painted after the notice.
        bell::paint_button(self, cells, cols);
    }

    /// The hosting-tab context for an agent row, resolved inside-out (
    /// US3): a NAMED tab supplies its name; an unnamed tab supplies the `·N`
    /// ordinal (today's form). `None` = the row has no tab (a paneless /
    /// watch-only row), so the caller falls back to the squad name.
    pub(super) fn agent_tab_context(
        &self,
        squad: Option<u64>,
        tab: Option<TabId>,
    ) -> Option<TabContext> {
        let sid = squad?;
        let tid = tab?;
        let s = self.layout.squads.iter().find(|s| s.id == sid)?;
        let (i, t) = s.tabs.iter().enumerate().find(|(_, t)| t.id == tid)?;
        Some(if t.named {
            TabContext::Named(t.name.clone())
        } else {
            TabContext::Ordinal(i + 1)
        })
    }
}
