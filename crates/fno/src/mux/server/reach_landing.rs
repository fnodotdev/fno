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
    #[error(
        "{reason}: workspace {workspace} has {count} tabs; name one of them, or pass --tab new"
    )]
    Tab {
        reason: String,
        workspace: String,
        count: usize,
    },
    #[error("anchor pane {0} no longer exists")]
    AnchorGone(u64),
    #[error("anchor pane {0} is not in the requested workspace")]
    AnchorOutsideWorkspace(u64),
    #[error("anchor pane {0} is not in the target tab")]
    AnchorOutsideTab(u64),
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
    // An anchor beside a split is the split's own cell, often a default.
    if placement.split.is_none() && (placement.at.is_some() || placement.from.is_some()) {
        flags.push("--from");
    }
    let verb = if flags.len() == 1 { "was" } else { "were" };
    (!flags.is_empty()).then(|| format!("{} {verb} not used", flags.join(", ")))
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
            let _ = reply.send(
                match self.check_reach(portal, &placement, owner, remote.as_ref()) {
                    Ok(()) => ServerMsg::Notice {
                        text: "placement ok".to_string(),
                    },
                    Err(refusal) => ServerMsg::Err {
                        code: err_code::BAD_REQUEST,
                        msg: refusal.to_string(),
                    },
                },
            );
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

    fn portal_seated(&self, idx: u8) -> bool {
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
        if placement.portal_new {
            if self.next_free_portal().is_none() {
                return Err(LandingRefusal::NoFreePortal);
            }
        } else if self.portal_seated(portal) {
            // An open portal retunes in place and reads no geometry.
            return Ok(());
        }
        let anchor = self.named_anchor(placement)?;
        self.fresh_landing(owner, placement, anchor).map(|_| ())
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
                            count: self.session.squad(sid).map_or(0, |s| s.tabs.len()),
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
        squads[at]
            .name
            .clone()
            .unwrap_or_else(|| derived[at].clone())
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
        if let Some((sq, ti)) = self.session.squad(sid).and_then(|sq| {
            let ti = sq.tabs.iter().position(|t| t.id == tid)?;
            Some((sq, ti))
        }) {
            let tab = &sq.tabs[ti];
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
