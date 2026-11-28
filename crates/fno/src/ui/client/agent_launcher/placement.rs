//! Open as: the placement a launch takes, its list, and its snapshot at
//! Enter. The client names a tab by id and an open portal by index. It never
//! picks the index of a new portal: the server does, when the launch lands.

use super::*;
use crate::proto::{PanePlacement, PaneTarget, TabSel};
use crate::tree::Dir;

/// The four directions, in list order.
const DIRS: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

fn dir_word(dir: Dir) -> &'static str {
    match dir {
        Dir::Left => "left",
        Dir::Right => "right",
        Dir::Up => "up",
        Dir::Down => "down",
    }
}

/// The row menu's split glyph and its key.
fn dir_mark(dir: Dir) -> (&'static str, &'static str) {
    match dir {
        Dir::Left => ("\u{25e7}", "shift+\u{2190}"),
        Dir::Right => ("\u{25e8}", "shift+\u{2192}"),
        Dir::Up => ("\u{2b12}", "shift+\u{2191}"),
        Dir::Down => ("\u{2b13}", "shift+\u{2193}"),
    }
}

/// Where the launched session shows. The thread placements are view choices,
/// each a portal. The one pane entry remains for harness args a thread lane
/// cannot carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Placement {
    /// No pick: the open portal this client focused last, or a new one when
    /// none is open.
    #[default]
    LastPortal,
    /// A new portal in a new tab.
    NewTab,
    /// A new portal split off the pane focused at Enter.
    Split(Dir),
    /// A new portal split off a picked tab. The id is the pick and the
    /// ordinal is only what the list showed.
    Tab { id: u64, ordinal: usize, dir: Dir },
    /// An open portal, shown the new worker in place.
    Portal(u8),
    /// A new portal split off an open one, which keeps showing.
    PortalSplit(u8, Dir),
    /// The one pane entry: a pane-hosted session in the active tab.
    PaneActiveTab,
}

/// Why a placement cannot be taken.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PickError {
    #[error("tab {0} closed; pick where it opens")]
    TabClosed(usize),
    #[error("portal {0} closed; pick where it opens")]
    PortalClosed(u8),
    #[error("this workspace has no tab to split; pick new tab")]
    NothingToSplit,
    #[error("a shell cannot run in a portal; pick a tab or a split")]
    ShellPortal,
    #[error("--split must be left, right, up, or down (got {0:?})")]
    Direction(String),
    #[error("--tab new opens one pane, so drop --tab new or --split")]
    NewTabSplit,
    #[error("no tab {0:?} in this workspace")]
    NoTab(String),
    #[error("--portal takes new or an open portal (got {0:?})")]
    NoPortal(String),
    #[error("portal {0} is open, so drop --tab; --split opens a new portal beside it")]
    PortalGeometry(u8),
    #[error("--as pane opens in the active tab; it takes no portal, split or other tab")]
    PaneGeometry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TabPlace {
    id: u64,
    /// The operator's own name for the tab.
    name: Option<String>,
}

/// What the card can place into: its workspace's tabs and every open portal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Places {
    tabs: Vec<TabPlace>,
    /// Index, then who it shows and where.
    portals: Vec<(u8, String)>,
    active: Option<u64>,
    /// The focused pane and its tab, when they are in this workspace.
    focus: Option<(u64, u64)>,
    /// The portal a launch with no pick takes.
    last: Option<u8>,
    /// The server takes a typed placement and the card's `!` runs. An older
    /// one opens a new tab for both.
    pub(super) typed: bool,
}

impl View {
    /// The portal whose seat has the focus.
    fn focused_portal(&self) -> Option<u8> {
        let focus = self.layout.focus;
        self.layout
            .agents
            .iter()
            .find(|row| row.pane_id == Some(focus) && row.portal.is_some())
            .and_then(|row| row.portal)
    }

    /// Remember the portal this client focused last, for as long as it is open.
    pub(in crate::ui::client) fn note_portal_focus(&mut self) {
        self.last_portal = self.focused_portal().or_else(|| {
            self.last_portal
                .filter(|last| self.open_portal_rows().iter().any(|(_, idx)| idx == last))
        });
    }
}

/// The portal of a launch with no pick: the focused or last focused one that
/// is open, else the lowest open index, as Enter on a row takes.
fn default_portal(open: &[(u8, String)], last: Option<u8>) -> Option<u8> {
    last.filter(|last| open.iter().any(|(idx, _)| idx == last))
        .or_else(|| open.first().map(|(idx, _)| *idx))
}

impl Places {
    pub(super) fn read(view: &View, squad: u64) -> Self {
        let squads = &view.layout.squads;
        let portals: Vec<(u8, String)> = view
            .open_portal_rows()
            .into_iter()
            .map(|(row, idx)| {
                let ordinal = squads
                    .iter()
                    .find(|sq| Some(sq.id) == row.squad)
                    .and_then(|sq| sq.tabs.iter().position(|t| Some(t.id) == row.tab));
                let shows = match ordinal {
                    Some(at) => format!("{} \u{b7} tab {}", row.name, at + 1),
                    None => row.name.clone(),
                };
                (idx, shows)
            })
            .collect();
        let typed =
            crate::mux_rows::wire_at_least(&view.session, crate::proto::LAUNCH_PLACEMENT_PROTO);
        let last =
            default_portal(&portals, view.focused_portal().or(view.last_portal)).filter(|_| typed);
        let Some(own) = squads.iter().find(|sq| sq.id == squad) else {
            return Self {
                portals,
                last,
                typed,
                ..Self::default()
            };
        };
        let focused = view.layout.focus;
        Self {
            tabs: own
                .tabs
                .iter()
                .map(|t| TabPlace {
                    id: t.id,
                    name: t.named.then(|| t.name.clone()),
                })
                .collect(),
            portals,
            last,
            typed,
            active: own.tabs.get(own.active_tab).map(|t| t.id),
            focus: own
                .tabs
                .iter()
                .find(|t| t.panes.iter().any(|pane| pane.id == focused))
                .map(|t| (t.id, focused)),
        }
    }

    fn ordinal(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id).map(|at| at + 1)
    }

    fn portal_open(&self, idx: u8) -> bool {
        self.portals.iter().any(|(open, _)| *open == idx)
    }

    pub(super) fn label(&self, placement: Placement) -> String {
        match placement {
            Placement::LastPortal => match self.last {
                Some(idx) => format!("portal {idx}"),
                None => "new portal".to_string(),
            },
            Placement::NewTab => "new tab".to_string(),
            Placement::Split(dir) => format!("split {}", dir_word(dir)),
            Placement::Tab { id, ordinal, dir } => {
                let shown = self.ordinal(id).unwrap_or(ordinal);
                format!("tab {shown} {}", dir_word(dir))
            }
            Placement::Portal(idx) => format!("portal {idx}"),
            Placement::PortalSplit(idx, dir) => format!("portal {idx} {}", dir_word(dir)),
            Placement::PaneActiveTab => "pane in active tab".to_string(),
        }
    }

    /// Whether the pick is the portal a launch with no pick takes.
    pub(super) fn is_default(&self, placement: Placement) -> bool {
        matches!(placement, Placement::Portal(idx) if self.last == Some(idx))
    }

    /// The typed placement of a thread launch, or `None` for the pane entry.
    pub(super) fn open_as(
        &self,
        squad: u64,
        placement: Placement,
    ) -> Result<Option<PanePlacement>, PickError> {
        let mut out = PanePlacement {
            portal_new: true,
            ..PanePlacement::default()
        };
        if !self.tabs.is_empty() {
            out.target = PaneTarget::SquadId(squad);
        }
        match placement {
            Placement::PaneActiveTab => return Ok(None),
            Placement::LastPortal => {
                let resolved = self.last.map_or(Placement::NewTab, Placement::Portal);
                return self.open_as(squad, resolved);
            }
            Placement::NewTab => out.tab = Some(TabSel::New),
            Placement::Portal(idx) => {
                if !self.portal_open(idx) {
                    return Err(PickError::PortalClosed(idx));
                }
                // The index alone: an open portal reads no geometry.
                out = PanePlacement {
                    portal: Some(idx),
                    ..PanePlacement::default()
                };
            }
            Placement::PortalSplit(idx, dir) => {
                if !self.portal_open(idx) {
                    return Err(PickError::PortalClosed(idx));
                }
                // The server turns the split into a new portal off this one.
                out = PanePlacement {
                    portal: Some(idx),
                    split: Some(dir),
                    ..PanePlacement::default()
                };
            }
            Placement::Split(dir) => {
                let tab = self
                    .focus
                    .map(|(tab, _)| tab)
                    .or(self.active)
                    .ok_or(PickError::NothingToSplit)?;
                out.tab = Some(TabSel::Id(tab));
                out.at = self.focus.map(|(_, pane)| pane);
                out.split = Some(dir);
            }
            Placement::Tab { id, ordinal, dir } => {
                if self.ordinal(id).is_none() {
                    return Err(PickError::TabClosed(ordinal));
                }
                out.tab = Some(TabSel::Id(id));
                out.at = self.focus.filter(|(tab, _)| *tab == id).map(|(_, p)| p);
                out.split = Some(dir);
            }
        }
        Ok(Some(out))
    }

    /// Where a `!` shell lands: every pick but a portal, in the card's
    /// workspace.
    pub(super) fn shell(
        &self,
        squad: u64,
        placement: Placement,
    ) -> Result<PanePlacement, PickError> {
        let placed = match placement {
            Placement::Portal(_) | Placement::PortalSplit(..) => {
                return Err(PickError::ShellPortal)
            }
            Placement::PaneActiveTab => PanePlacement {
                tab: Some(TabSel::Active),
                ..PanePlacement::default()
            },
            // A shell never takes a portal, so no pick is a new tab.
            Placement::LastPortal => self.open_as(squad, Placement::NewTab)?.unwrap_or_default(),
            other => self.open_as(squad, other)?.unwrap_or_default(),
        };
        Ok(PanePlacement {
            target: PaneTarget::SquadId(squad),
            portal_new: false,
            ..placed
        })
    }

    /// The flags that place a thread like this from a shell. The child of a
    /// launch carries none: the server opens the one portal.
    pub(super) fn flags(&self, placement: Placement) -> Vec<String> {
        let words = match placement {
            Placement::PaneActiveTab => Vec::new(),
            Placement::LastPortal => {
                return self
                    .last
                    .map(|idx| vec!["--portal".to_string(), idx.to_string()])
                    .unwrap_or_default();
            }
            Placement::NewTab => vec!["--tab", "new"],
            Placement::Split(dir) => vec!["--portal", "new", "--split", dir_word(dir)],
            Placement::Portal(idx) => return vec!["--portal".to_string(), idx.to_string()],
            Placement::PortalSplit(idx, dir) => {
                return ["--portal", &idx.to_string(), "--split", dir_word(dir)]
                    .map(str::to_string)
                    .to_vec();
            }
            Placement::Tab { id, ordinal, dir } => {
                let shown = self.ordinal(id).unwrap_or(ordinal).to_string();
                return ["--tab", &shown, "--portal", "new", "--split", dir_word(dir)]
                    .map(str::to_string)
                    .to_vec();
            }
        };
        words.into_iter().map(str::to_string).collect()
    }

    fn tab_named(&self, word: &str) -> Option<(u64, usize)> {
        let at = if let Some(id) = word.strip_prefix("id:") {
            let id: u64 = id.parse().ok()?;
            self.tabs.iter().position(|t| t.id == id)?
        } else if word == "active" {
            self.tabs.iter().position(|t| Some(t.id) == self.active)?
        } else if let Ok(ordinal) = word.parse::<usize>() {
            ordinal.checked_sub(1).filter(|at| *at < self.tabs.len())?
        } else {
            let name = word.strip_prefix("name:").unwrap_or(word);
            self.tabs
                .iter()
                .position(|t| t.name.as_deref() == Some(name))?
        };
        Some((self.tabs.get(at)?.id, at + 1))
    }

    /// The placement an edited will-run line names.
    pub(super) fn from_flags(&self, flags: &Flags) -> Result<Placement, PickError> {
        let (tab, split) = (flags.tab.as_deref(), flags.split.as_deref());
        let portal = flags.portal.as_deref();
        if flags.pane {
            if portal.is_some() || split.is_some() || tab.is_some_and(|t| t != "active") {
                return Err(PickError::PaneGeometry);
            }
            return Ok(Placement::PaneActiveTab);
        }
        let dir = match split {
            Some(word) => Some(
                DIRS.into_iter()
                    .find(|dir| dir_word(*dir) == word)
                    .ok_or_else(|| PickError::Direction(word.to_string()))?,
            ),
            None => None,
        };
        if let Some(word) = portal.filter(|word| *word != "new") {
            let idx = word
                .parse::<u8>()
                .ok()
                .filter(|idx| self.portal_open(*idx))
                .ok_or_else(|| PickError::NoPortal(word.to_string()))?;
            if tab.is_some() {
                return Err(PickError::PortalGeometry(idx));
            }
            return Ok(dir.map_or(Placement::Portal(idx), |dir| {
                Placement::PortalSplit(idx, dir)
            }));
        }
        match tab {
            Some("new") if dir.is_some() => Err(PickError::NewTabSplit),
            Some("new") => Ok(Placement::NewTab),
            Some(word) => {
                let (id, ordinal) = self
                    .tab_named(word)
                    .ok_or_else(|| PickError::NoTab(word.to_string()))?;
                let dir = dir.unwrap_or(Dir::Down);
                Ok(Placement::Tab { id, ordinal, dir })
            }
            None => Ok(match dir {
                Some(dir) => Placement::Split(dir),
                None if portal.is_some() => Placement::NewTab,
                None => Placement::LastPortal,
            }),
        }
    }
}

/// The placement flags read off an edited will-run line.
#[derive(Debug, Default)]
pub(super) struct Flags {
    pub pane: bool,
    pub tab: Option<String>,
    pub split: Option<String>,
    pub portal: Option<String>,
}

/// The word a saved set keeps. A tab or portal number means nothing in a
/// later session, so those picks save the default.
pub(super) fn set_word(placement: Placement) -> &'static str {
    match placement {
        Placement::Split(Dir::Right) => "split",
        Placement::Split(Dir::Left) => "split-left",
        Placement::Split(Dir::Up) => "split-up",
        Placement::Split(Dir::Down) => "split-down",
        Placement::PaneActiveTab => "pane",
        Placement::NewTab => "tab",
        Placement::LastPortal
        | Placement::Tab { .. }
        | Placement::Portal(_)
        | Placement::PortalSplit(..) => "thread",
    }
}

pub(super) fn from_set_word(word: &str) -> Placement {
    match word {
        "split" => Placement::Split(Dir::Right),
        "split-left" => Placement::Split(Dir::Left),
        "split-up" => Placement::Split(Dir::Up),
        "split-down" => Placement::Split(Dir::Down),
        "pane" => Placement::PaneActiveTab,
        "tab" => Placement::NewTab,
        _ => Placement::LastPortal,
    }
}

/// Refresh the open card's snapshot.
pub(super) fn sync(view: &mut View) {
    let Some(squad) = view.launcher.as_ref().map(|l| l.squad) else {
        return;
    };
    let places = Places::read(view, squad);
    if let Some(l) = view.launcher.as_mut() {
        l.places = places;
    }
}

/// The Open as list, one entry a line. Shell mode has no portal rows.
pub(super) fn rows(l: &Launcher) -> (Vec<PopupRow>, Vec<Option<PickerAction>>) {
    let (mut rows, mut actions) = (Vec::new(), Vec::new());
    let picked = l.draft.placement;
    let mut add = |glyph: &str, label: &str, hint: &str, checked: bool, action: PickerAction| {
        let glyph = if checked { "\u{2713}" } else { glyph };
        push_entry(
            &mut rows,
            &mut actions,
            glyph,
            label,
            hint,
            true,
            Some(action),
        );
    };
    let place = PickerAction::Place;
    let last = Placement::LastPortal;
    if l.shell {
        let word = super::shell_box::default_word(l);
        add("!", word, "default", picked == last, place(last));
    } else {
        let shows = l.places.label(last);
        add(
            "\u{25eb}",
            "last portal",
            &shows,
            picked == last,
            place(last),
        );
    }
    let new_tab = Placement::NewTab;
    add("+", "new tab", "", picked == new_tab, place(new_tab));
    for dir in DIRS {
        let (glyph, key) = dir_mark(dir);
        let split = Placement::Split(dir);
        let label = l.places.label(split);
        add(glyph, &label, key, picked == split, place(split));
    }
    for (at, tab) in l.places.tabs.iter().enumerate() {
        let ordinal = at + 1;
        let checked = matches!(picked, Placement::Tab { id, .. } if id == tab.id);
        let action = PickerAction::PickTab {
            id: tab.id,
            ordinal,
        };
        let hint = tab.name.as_deref().unwrap_or("");
        add("\u{203a}", &format!("tab {ordinal}"), hint, checked, action);
    }
    if !l.shell {
        for (idx, shows) in &l.places.portals {
            let portal = Placement::Portal(*idx);
            let label = l.places.label(portal);
            add("\u{25eb}", &label, shows, picked == portal, place(portal));
        }
    }
    let pane = Placement::PaneActiveTab;
    add(
        "\u{2022}",
        "pane in active tab",
        "",
        picked == pane,
        place(pane),
    );
    (rows, actions)
}

/// A picked tab's second step: the side of its focused pane.
pub(super) fn open_directions(l: &mut Launcher, id: u64, ordinal: usize, anchor: Anchor) {
    let mut all_rows = vec![PopupRow::Header(format!("tab {ordinal}: split"))];
    let mut all_actions: Vec<Option<PickerAction>> = vec![None];
    for dir in DIRS {
        let (glyph, key) = dir_mark(dir);
        let tab = Placement::Tab { id, ordinal, dir };
        push_entry(
            &mut all_rows,
            &mut all_actions,
            if l.draft.placement == tab {
                "\u{2713}"
            } else {
                glyph
            },
            dir_word(dir),
            key,
            true,
            Some(PickerAction::Place(tab)),
        );
    }
    let (popup, actions) = filtered_popup(Focus::Opens, &all_rows, &all_actions, "", anchor);
    l.picker = Some(Picker {
        popup,
        actions,
        all_rows,
        all_actions,
        field: Focus::Opens,
        anchor,
        filter: String::new(),
        mode: PickerMode::Direction { id, ordinal },
    });
}

/// A shift+arrow in the Open as list takes that direction at once. `false`
/// when no such list is open, and the key is then a plain arrow.
pub(super) fn shift_pick(view: &mut View, dir: Dir) -> bool {
    let Some(l) = view.launcher.as_mut() else {
        return false;
    };
    let placed = match l.picker.as_ref().filter(|pk| pk.field == Focus::Opens) {
        Some(pk) => match pk.mode {
            PickerMode::Direction { id, ordinal } => Placement::Tab { id, ordinal, dir },
            _ => Placement::Split(dir),
        },
        None => return false,
    };
    l.picker = None;
    l.draft.placement = placed;
    l.draft.bump();
    true
}

/// The Open as states a snapshot can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shot {
    List,
    TabStep,
    Placed,
    /// No pick, with the chip naming the portal Enter takes.
    NoPick,
    ShellRunning,
    ShellFailed,
    Shell,
}

/// Snapshot seam: put the open card in one Open as state, by the same calls
/// the keys make.
pub(crate) fn for_snapshot(view: &mut View, shot: Shot) {
    sync(view);
    let Some(l) = view.launcher.as_mut() else {
        return;
    };
    let first = l.places.tabs.first().map(|tab| (tab.id, 1));
    let anchor = Anchor::At { row: 0, col: 0 };
    l.focus = Focus::Opens;
    match (shot, first) {
        (Shot::TabStep, Some((id, ordinal))) => open_directions(l, id, ordinal, anchor),
        (Shot::Placed, Some((id, ordinal))) => {
            let dir = Dir::Down;
            l.draft.placement = Placement::Tab { id, ordinal, dir };
            l.focus = Focus::Message;
        }
        (Shot::Placed, None) => l.focus = Focus::Message,
        (Shot::NoPick, _) => {}
        (Shot::ShellRunning | Shot::ShellFailed, _) => {
            l.shell = true;
            l.focus = Focus::Message;
            l.draft.message = "make check".to_string();
            l.draft.cursor_chars = l.draft.message.chars().count();
            let failed = shot == Shot::ShellFailed;
            let (cwd, line) = (l.draft.cwd(), l.draft.message.clone());
            l.run = Some(super::shell_box::staged(cwd, line, failed));
        }
        _ => {
            if shot == Shot::Shell {
                l.shell = true;
                l.draft.message = "make check".to_string();
                l.draft.cursor_chars = l.draft.message.chars().count();
            }
            let catalog = &view.launcher_catalog;
            open_picker_at(l, catalog, &view.backlog, Some((0, 0)), Focus::Opens);
        }
    }
}

impl LKey {
    /// A shift+arrow is a plain arrow everywhere but the Open as list.
    pub(crate) fn plain(self) -> Self {
        match self {
            Self::Shift(Dir::Left) => Self::Left,
            Self::Shift(Dir::Right) => Self::Right,
            Self::Shift(Dir::Up) => Self::Up,
            Self::Shift(Dir::Down) => Self::Down,
            key => key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_placement_table() {
        let places = Places {
            tabs: [11, 22, 33].map(|id| TabPlace { id, name: None }).to_vec(),
            portals: vec![(2, "kite".to_string())],
            active: Some(11),
            focus: Some((11, 7)),
            last: Some(2),
            typed: true,
        };
        let third = Placement::Tab {
            id: 33,
            ordinal: 3,
            dir: Dir::Up,
        };
        // (pick, tab, anchor, portal)
        let cases = [
            (Placement::NewTab, Some(TabSel::New), None, None),
            (
                Placement::Split(Dir::Left),
                Some(TabSel::Id(11)),
                Some(7),
                None,
            ),
            (third, Some(TabSel::Id(33)), None, None),
            (Placement::Portal(2), None, None, Some(2)),
            (Placement::PortalSplit(2, Dir::Down), None, None, Some(2)),
        ];
        for (pick, tab, at, portal) in cases {
            let placed = places
                .open_as(1, pick)
                .expect("it lands")
                .expect("a thread");
            assert_eq!((placed.tab, placed.at, placed.portal), (tab, at, portal));
            assert_eq!(placed.portal_new, portal.is_none(), "the server picks");
            let mut flags = Flags::default();
            for pair in places.flags(pick).chunks(2) {
                let [flag, value] = pair else {
                    panic!("a flag with no value: {pair:?}");
                };
                let slot = match flag.as_str() {
                    "--tab" => &mut flags.tab,
                    "--split" => &mut flags.split,
                    _ => &mut flags.portal,
                };
                *slot = Some(value.clone());
            }
            assert_eq!(places.from_flags(&flags), Ok(pick), "the line reads back");
        }
        let gone = Places::default();
        assert_eq!(gone.open_as(1, third), Err(PickError::TabClosed(3)));
        assert_eq!(
            gone.open_as(1, Placement::Portal(2)),
            Err(PickError::PortalClosed(2))
        );
        assert_eq!(
            places.shell(1, Placement::Portal(2)),
            Err(PickError::ShellPortal)
        );
        assert_eq!(from_set_word(set_word(third)), Placement::LastPortal);
        assert_eq!(from_set_word("thread"), Placement::LastPortal);
        assert_eq!(from_set_word("split"), Placement::Split(Dir::Right));
    }

    #[test]
    fn composer_decided_defaults_table() {
        let open = || vec![(1, "one".to_string()), (4, "four".to_string())];
        // (case, the open portals, the one focused last, where no pick lands)
        let cases = [
            ("the one focused last", open(), Some(4), Some(4)),
            ("the last one closed", open(), Some(9), Some(1)),
            ("none focused yet", open(), None, Some(1)),
            ("none open", Vec::new(), Some(4), None),
        ];
        for (case, portals, focused, want) in cases {
            let last = default_portal(&portals, focused);
            assert_eq!(last, want, "{case}");
            let places = Places {
                portals,
                last,
                ..Places::default()
            };
            let pick = Placement::LastPortal;
            let placed = places
                .open_as(1, pick)
                .expect("it lands")
                .expect("a thread");
            assert_eq!(placed.portal, want, "{case}");
            assert_eq!(placed.portal_new, want.is_none(), "{case}");
            let chip = want.map_or("new portal".to_string(), |idx| format!("portal {idx}"));
            assert_eq!(places.label(pick), chip, "{case}: the chip names it");
            let shell = places.shell(1, pick).expect("a shell lands");
            assert_eq!(shell.tab, Some(TabSel::New), "{case}: never a portal");
        }
    }
}
