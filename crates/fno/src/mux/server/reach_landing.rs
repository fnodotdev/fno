//! Where does a portal reach land? One resolver answers the pre-birth check of
//! the thread door and the reach itself, so a check cannot pass a placement
//! that the reach then refuses.

use super::*;
use crate::proto::{RemoteRow, ThreadOrigin};

/// Why a reach cannot land. Each refusal is found before a pane exists.
#[derive(Debug, thiserror::Error)]
pub(super) enum LandingRefusal {
    #[cfg(not(feature = "web"))]
    #[error("{0} cannot be shown here: remote links need the full fno build (the web feature)")]
    NoLinks(String),
    #[cfg(feature = "web")]
    #[error("{0}")]
    Link(String),
    #[error("no free portal: every index 0-255 is seated")]
    NoFreePortal,
    #[error("{0}")]
    Anchor(String),
    #[error("{0}")]
    Workspace(String),
    #[error("{reason}: workspace {workspace} has {tabs}; name one of them, or pass --tab new")]
    Tab {
        reason: String,
        workspace: String,
        tabs: String,
    },
    #[error("anchor pane {0} no longer exists; name a live cell with --from")]
    AnchorGone(u64),
    #[error(
        "anchor pane {0} is not in the requested workspace; change --workspace, or name a \
         cell there with --from"
    )]
    AnchorOutsideWorkspace(u64),
    #[error(
        "anchor pane {0} is not in the target tab; change --tab, or name a cell in it with --from"
    )]
    AnchorOutsideTab(u64),
    #[error("a --split needs an anchor: portal {0} is not open, so name --tab N or --from <cell>")]
    SplitNeedsAnchor(u8),
    #[error("portal {portal} is in tab {tab}; pass --tab {tab}, or drop --tab")]
    PortalTab { portal: u8, tab: usize },
    #[error("portal {0} is open, so --split halves it; drop --from, or pass --portal new")]
    PortalCell(u8),
}

/// The flags a reach named that a portal with a live seat does not use, worded
/// for the notice: `--split was not used`.
pub(super) fn unused_geometry(placement: &PanePlacement) -> Option<String> {
    let mut flags = Vec::new();
    if placement.split.is_some() {
        flags.push("--split");
    }
    if placement.tab.is_some() {
        flags.push("--tab");
    }
    if !matches!(placement.target, PaneTarget::CurrentRoute) {
        flags.push("--workspace");
    }
    // A `from` is always typed. An `at` beside a split is often the caller's
    // own pane, filled in as the split's default.
    if placement.from.is_some() || (placement.split.is_none() && placement.at.is_some()) {
        flags.push("--from");
    }
    let was = if flags.len() == 1 { "was" } else { "were" };
    (!flags.is_empty()).then(|| format!("{} {was} not used", flags.join(", ")))
}

impl Core {
    /// The thread door's request. A check answers and changes nothing. A
    /// worker on another machine has no row here, so its workspace comes from
    /// the caller before the ordinary reach runs.
    pub(super) fn thread_door(
        &mut self,
        name: &str,
        portal: u8,
        mut placement: PanePlacement,
        origin: Option<ThreadOrigin>,
        agents: Option<Vec<RegistryAgent>>,
        reply: ControlReply,
    ) {
        let Some(origin) = origin else {
            return self.portal_ctl(name, portal, placement, agents, reply);
        };
        if let Some(rows) = agents {
            self.agents = rows;
        }
        let remote = RemoteRow::from_address(name);
        let owner = self.origin_workspace(&origin);
        if origin.check {
            let answer = match self.check_reach(portal, &placement, owner, remote.as_ref()) {
                Ok(()) => ServerMsg::Notice {
                    text: "placement ok".to_string(),
                },
                Err(refusal) => ServerMsg::Err {
                    code: err_code::BAD_REQUEST,
                    msg: refusal.to_string(),
                },
            };
            // A caller that gave up waiting needs no answer.
            drop(reply.send(answer));
            return;
        }
        // An anchor names its own workspace, and a live seat keeps its own.
        let routed_by_caller = remote.is_some()
            && matches!(placement.target, PaneTarget::CurrentRoute)
            && placement.at.is_none()
            && placement.from.is_none()
            && (placement.portal_new || !self.portal_seated(portal))
            && !self
                .portals
                .iter()
                .any(|(idx, shown)| shown.row_key == name && self.portal_seated(*idx));
        if routed_by_caller {
            placement.target = PaneTarget::SquadId(owner);
        }
        self.portal_ctl(name, portal, placement, None, reply);
    }

    /// The workspace a thread born from `origin` belongs to, by the resolver
    /// its row is placed with once it exists.
    fn origin_workspace(&self, origin: &ThreadOrigin) -> u64 {
        let row = RegistryAgent {
            cwd: origin.cwd.clone(),
            spawned_by_session: origin.parent.clone(),
            ..RegistryAgent::default()
        };
        self.thread_workspace(&row)
            .unwrap_or_else(|| observers::passive_anchor(&self.session).0)
    }

    pub(super) fn portal_seated(&self, idx: u8) -> bool {
        self.portals.get(&idx).is_some_and(|p| {
            self.panes.contains_key(&p.seat) && self.session.find_pane(p.seat).is_some()
        })
    }

    /// Would this reach land? Reads only.
    fn check_reach(
        &self,
        portal: u8,
        placement: &PanePlacement,
        owner: u64,
        remote: Option<&RemoteRow>,
    ) -> Result<(), LandingRefusal> {
        if let Some(remote) = remote {
            #[cfg(not(feature = "web"))]
            return Err(LandingRefusal::NoLinks(remote.name.clone()));
            #[cfg(feature = "web")]
            crate::mux_link::pane_argv(remote).map_err(LandingRefusal::Link)?;
        }
        let mut placement = placement.clone();
        self.split_open_portal(&mut placement)?;
        if placement.portal_new {
            if self.next_free_portal().is_none() {
                return Err(LandingRefusal::NoFreePortal);
            }
        } else if self.portal_seated(portal) {
            // An open portal retunes in place and reads no geometry.
            return Ok(());
        }
        let anchor = self.named_anchor(&placement)?;
        self.fresh_landing(owner, &placement, anchor).map(|_| ())
    }

    /// A split beside a typed index N. When N is open its seat is the cell
    /// the split halves, and the worker gets a new portal: N keeps showing.
    /// An `at` gives way, because it is often the caller's own pane.
    pub(super) fn split_open_portal(
        &self,
        placement: &mut PanePlacement,
    ) -> Result<(), LandingRefusal> {
        let (Some(idx), Some(_), false) = (placement.portal, placement.split, placement.portal_new)
        else {
            return Ok(());
        };
        let seat = self.portals.get(&idx).map(|portal| portal.seat);
        let Some(seat) = seat.filter(|_| self.portal_seated(idx)) else {
            let anchored =
                placement.at.is_some() || placement.from.is_some() || placement.tab.is_some();
            return if anchored {
                Ok(())
            } else {
                Err(LandingRefusal::SplitNeedsAnchor(idx))
            };
        };
        if placement.from.is_some() {
            let from = PanePlacement {
                at: None,
                ..placement.clone()
            };
            if self.named_anchor(&from)? != Some(seat) {
                return Err(LandingRefusal::PortalCell(idx));
            }
        }
        let found = self.session.find_pane(seat);
        if let (Some((sid, ti)), Some(tab)) = (found, placement.tab.as_ref()) {
            if self.resolve_tab_index(sid, tab).ok() != Some(ti) {
                return Err(LandingRefusal::PortalTab {
                    portal: idx,
                    tab: ti + 1,
                });
            }
        }
        placement.portal = None;
        placement.portal_new = true;
        placement.from = None;
        placement.at = Some(seat);
        Ok(())
    }

    /// The cell a placement names: `at`, else the resolved `from`.
    pub(super) fn named_anchor(
        &self,
        placement: &PanePlacement,
    ) -> Result<Option<u64>, LandingRefusal> {
        if placement.at.is_some() {
            return Ok(placement.at);
        }
        match placement
            .from
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(from) => self
                .resolve_from_anchor(from)
                .map(Some)
                .map_err(LandingRefusal::Anchor),
            None => Ok(None),
        }
    }

    /// The workspace and the pure geometry of a fresh open. An anchor brings
    /// its own workspace, and its own tab when none is named. An anchor
    /// outside a named tab or workspace refuses, and so does a tab that does
    /// not resolve, both before a pane exists.
    pub(super) fn fresh_landing(
        &self,
        owner: u64,
        placement: &PanePlacement,
        anchor: Option<u64>,
    ) -> Result<(Option<u64>, PanePlacement), LandingRefusal> {
        let found = match anchor {
            Some(pane) => Some((
                pane,
                self.session
                    .find_pane(pane)
                    .ok_or(LandingRefusal::AnchorGone(pane))?,
            )),
            None => None,
        };
        let current = found.map_or(owner, |(_, (sid, _))| sid);
        let dest = self
            .resolve_placement_target(&placement.target, Some(current))
            .map_err(LandingRefusal::Workspace)?;
        // The portal fields address the reach and are never geometry:
        // place_with reads none of them.
        let mut eff = PanePlacement {
            portal: None,
            portal_new: false,
            thread_pane: false,
            view: false,
            from: None,
            at: anchor,
            ..placement.clone()
        };
        // A new tab is born with the pane, so it has nothing to resolve.
        if matches!(placement.tab, Some(TabSel::New)) {
            return Ok((dest, eff));
        }
        let named_tab = match (dest, placement.tab.as_ref()) {
            (Some(sid), Some(sel)) => {
                Some(
                    self.resolve_tab_index(sid, sel)
                        .map_err(|reason| LandingRefusal::Tab {
                            reason,
                            workspace: self.workspace_label(sid),
                            tabs: match self.session.squad(sid).map_or(0, |s| s.tabs.len()) {
                                1 => "1 tab".to_string(),
                                count => format!("{count} tabs"),
                            },
                        })?,
                )
            }
            _ => None,
        };
        if let Some((pane, (sid, ti))) = found {
            if dest != Some(sid) {
                return Err(LandingRefusal::AnchorOutsideWorkspace(pane));
            }
            match named_tab {
                Some(named) if named != ti => {
                    return Err(LandingRefusal::AnchorOutsideTab(pane));
                }
                Some(_) => {}
                None => {
                    eff.tab = self
                        .session
                        .squad(sid)
                        .and_then(|s| s.tabs.get(ti))
                        .map(|t| TabSel::Id(t.id));
                }
            }
        }
        Ok((dest, eff))
    }

    fn workspace_label(&self, sid: u64) -> String {
        let squads = &self.session.squads;
        let Some(at) = squads.iter().position(|s| s.id == sid) else {
            return sid.to_string();
        };
        let cwds: Vec<String> = squads
            .iter()
            .map(|s| s.canonical_cwd().to_string())
            .collect();
        let derived = squad::display_names(&cwds);
        squads
            .get(at)
            .and_then(|s| s.name.clone())
            .or_else(|| derived.get(at).cloned())
            .unwrap_or_else(|| sid.to_string())
    }

    /// The one landing line: `thread pane -> w1 (portal 2, workspace app, tab 1, split down)`.
    pub(super) fn landing_line(
        &self,
        name: &str,
        idx: u8,
        (sid, tid, fell_back): (u64, TabId, bool),
        asked: Option<Dir>,
    ) -> String {
        let mut at = vec![
            format!("portal {idx}"),
            format!("workspace {}", self.workspace_label(sid)),
        ];
        if let Some((ti, tab)) = self
            .session
            .squad(sid)
            .and_then(|sq| sq.tabs.iter().enumerate().find(|(_, t)| t.id == tid))
        {
            at.push(match tab.name.as_deref() {
                Some(named) => format!("tab {} ·{named}", ti + 1),
                None => format!("tab {}", ti + 1),
            });
            if !fell_back && tree::leaves(&tab.root).len() > 1 {
                let dir = match asked.unwrap_or(Dir::Down) {
                    Dir::Left => "left",
                    Dir::Right => "right",
                    Dir::Up => "up",
                    Dir::Down => "down",
                };
                at.push(format!("split {dir}"));
            }
        }
        format!("thread pane -> {name} ({})", at.join(", "))
    }
}
