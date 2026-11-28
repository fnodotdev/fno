//! Placement of a spawned pane: the tab, anchor and split it lands at, and the
//! exact-anchor (`--at`) road. Parent helpers resolve through the glob.
use super::*;

impl Core {
    /// Placement that honors `placement.tab` / `placement.at` (v41), reaping
    /// `pid` on any hard error so a bad anchor never orphans a pane. The
    /// pre-v41 no-tab/no-anchor path delegates to [`Self::place_spawned_pane`]
    /// unchanged.
    pub(super) fn place_with(
        &mut self,
        dest: Option<u64>,
        squad_key: &str,
        pid: u64,
        placement: &PanePlacement,
    ) -> Result<(u64, TabId, bool), (u32, String)> {
        if placement.fit {
            // Server-chosen tab; the tab/at/split combination is
            // refused in run_pane, so the strict-anchor branch is unreachable.
            return self.place_with_fit(dest, squad_key, pid, placement);
        }
        // Strict origin placement (--at current, fallback=Refuse): locate the
        // anchor pane, infer its squad+tab, refuse a conflicting explicit
        // selector, and never fall back to a new tab. Handled before the
        // selector-resolved path, which assumes the active tab rather than the
        // anchor's real location.
        if placement.at.is_some() && placement.fallback == PlacementFallback::Refuse {
            return self.place_strict_at_anchor(pid, placement);
        }
        if placement.tab.is_none() && placement.at.is_none() {
            return self
                .place_spawned_pane(dest, squad_key, pid, placement.split)
                .map_err(|e| (err_code::SPAWN_FAILED, e));
        }
        let Some(sid) = dest else {
            self.reap_pane(pid);
            return Err((
                err_code::BAD_REQUEST,
                "a --tab/--at placement needs a resolved workspace".into(),
            ));
        };
        let Some(sq) = self.session.squad(sid) else {
            self.reap_pane(pid);
            return Err((err_code::SPAWN_FAILED, "selected workspace vanished".into()));
        };
        // An explicit `New` tab ignores any anchor - it is born with this pane.
        if matches!(placement.tab, Some(TabSel::New)) {
            return Ok((sid, self.tab_born_with(sid, pid), false));
        }
        let ti = match &placement.tab {
            Some(sel) => match self.resolve_tab_index(sid, sel) {
                Ok(ti) => ti,
                Err(e) => {
                    self.reap_pane(pid);
                    return Err((err_code::BAD_REQUEST, e));
                }
            },
            None => sq.active_tab.min(sq.tabs.len().saturating_sub(1)),
        };
        let Some(tab) = self.session.squad(sid).and_then(|s| s.tabs.get(ti)) else {
            self.reap_pane(pid);
            return Err((err_code::SPAWN_FAILED, "selected tab vanished".into()));
        };
        let (tid, focus, cells) = (tab.id, tab.focus, tree::leaves(&tab.root));
        // Same pane cap as the strict path: a numeric anchor or explicit
        // --tab resolves here instead, and the cap the caller believes in
        // must not silently stop at one spelling of --at. The New-tab branch
        // above is born with a single pane and needs no check.
        if let Some(cap) = placement.max_panes {
            let pane_count = cells.len();
            if pane_count >= cap {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!(
                        "placement refused: target tab has {pane_count} panes and the cap is {cap}. Use --workspace <name> to place this worker elsewhere"
                    ),
                ));
            }
        }
        let vp = self.tab_rect(tid);
        let anchor = placement.at.unwrap_or(focus);
        let dir = placement.split.unwrap_or(Dir::Down);
        if !cells.contains(&anchor) {
            self.reap_pane(pid);
            return Err((
                err_code::BAD_REQUEST,
                format!("anchor pane {anchor} is not in the target tab"),
            ));
        }
        let squad = self.session.squad_mut(sid);
        let Some(tab) = squad.and_then(|s| s.tabs.get_mut(ti)) else {
            self.reap_pane(pid);
            return Err((err_code::SPAWN_FAILED, "selected tab vanished".into()));
        };
        match tree::split_at(tab, vp, anchor, dir, pid) {
            Ok(()) => {
                self.maybe_reflow(sid, tid);
                Ok((sid, tid, false))
            }
            // Min-size refusal falls back to a fresh tab, never a dead-end -
            // mirroring place_spawned_pane. Strict placement never reaches here
            // (it routes through place_strict_at_anchor), but a Refuse policy on
            // any selector-resolved path still fails closed rather than minting.
            Err(tree::SplitError::TooSmall { .. }) => {
                if placement.fallback == PlacementFallback::Refuse {
                    self.reap_pane(pid);
                    return Err((
                        err_code::BAD_REQUEST,
                        "placement cannot fit: a resulting pane would be below the minimum size"
                            .into(),
                    ));
                }
                Ok((sid, self.tab_born_with(sid, pid), true))
            }
            Err(e) => {
                self.reap_pane(pid);
                Err((err_code::BAD_REQUEST, e.to_string()))
            }
        }
    }

    /// A new tab of workspace `sid` whose one cell is `pid`.
    fn tab_born_with(&mut self, sid: u64, pid: u64) -> TabId {
        let id = self.session.mint_tab_id();
        if let Some(sq) = self.session.squad_mut(sid) {
            sq.tabs.push(Tab {
                name: None,
                id,
                root: Node::Leaf(pid),
                focus: pid,
            });
        }
        id
    }

    /// Strict origin placement for `--at current` (v44): the anchor is
    /// pinned to the calling pane id, so focus races cannot redirect it. The
    /// server locates the anchor once inside this (serialized) turn, infers its
    /// squad+tab, refuses a conflicting explicit selector, splits beside it, and
    /// never creates a tab or moves focus. Any failure reaps the pre-spawned
    /// child and leaves the tree, focus, and registry untouched.
    pub(super) fn place_strict_at_anchor(
        &mut self,
        pid: u64,
        placement: &PanePlacement,
    ) -> Result<(u64, TabId, bool), (u32, String)> {
        let anchor = placement.at.expect("strict placement carries an anchor");
        let dir = placement.split.unwrap_or(Dir::Down);
        let (sid, ti) = match self.session.find_pane(anchor) {
            Some(loc) => loc,
            None => {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} no longer exists"),
                ));
            }
        };
        // An explicit workspace/tab selector must name the anchor's actual
        // location; anything else is refused rather than redirecting placement.
        if !matches!(placement.target, PaneTarget::CurrentRoute) {
            let ok = matches!(self.resolve_placement_target(&placement.target, None), Ok(s) if s == Some(sid));
            if !ok {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} is not in the requested workspace"),
                ));
            }
        }
        if let Some(sel) = placement.tab.as_ref() {
            if matches!(sel, TabSel::New) {
                // Strict placement pins the anchor's tab and never mints one;
                // `--tab new` asks for the opposite, so the combination is
                // refused rather than silently dropping the new-tab request.
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    "exact placement cannot combine --at with a new-tab selector".into(),
                ));
            }
            let ok = matches!(self.resolve_tab_index(sid, sel), Ok(rti) if rti == ti);
            if !ok {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} is not in the requested tab"),
                ));
            }
        }
        if let Some(cap) = placement.max_panes {
            let pane_count = tree::leaves(
                &self
                    .session
                    .squad(sid)
                    .expect("find_pane returned a live squad")
                    .tabs[ti]
                    .root,
            )
            .len();
            if pane_count >= cap {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!(
                        "exact placement refused: target tab has {pane_count} panes and the cap is {cap}. Use --workspace <name> to place this worker elsewhere"
                    ),
                ));
            }
        }
        let tid = self
            .session
            .squad(sid)
            .expect("find_pane returned a live squad")
            .tabs[ti]
            .id;
        let vp = self.tab_rect(tid);
        let res = {
            let tab = &mut self
                .session
                .squad_mut(sid)
                .expect("find_pane returned a live squad")
                .tabs[ti];
            tree::split_at(tab, vp, anchor, dir, pid)
        };
        match res {
            // No auto-reflow: strict placement commits the exact split it
            // was asked for, and a reflow would re-tile it and make room a
            // second strict spawn must be refused.
            Ok(()) => Ok((sid, tid, false)),
            Err(tree::SplitError::TooSmall { .. }) => {
                self.reap_pane(pid);
                Err((
                    err_code::BAD_REQUEST,
                    "exact placement cannot fit: a resulting pane would be below the minimum size"
                        .into(),
                ))
            }
            Err(e) => {
                self.reap_pane(pid);
                Err((err_code::BAD_REQUEST, e.to_string()))
            }
        }
    }
}
