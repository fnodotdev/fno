//! `fno mux serve --snapshot`: compose one mux frame through the real
//! [`View::compose`] and write it as html, svg or png, with no terminal,
//! shell, scrollback or cursor in the picture.
//!
//! `--server <name>` names the server. The shot goes through the read-only
//! observer attach the web bridge uses: `rows: 0, cols: 0` never resizes a PTY
//! and the write half is dropped after the attach. A public shot comes from
//! `scripts/ops/mux-demo-snapshot.sh`, which builds a throwaway server with
//! invented work and shoots it here, so no real session text can leak.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{theme_import_ui, LayoutView, View};
use crate::client::messages_view::{ChatTab, Col};
use crate::frame_html::{self, Theme};
use crate::popup::Anchor;
use crate::proto::{self, ClientMsg, Frame, ServerMsg, BUILD_VERSION, PROTO_VERSION};
use serde_json::Value;

const USAGE: &str =
    "usage: fno mux serve --snapshot --server <name> --out <path> [--squad <name>] \
[--theme dark|light|macchiato] [--format html|svg|png] [--size <cols>x<rows> [--fit]] [--peer <cols>x<rows>] \
[--keys-footer] [--font <family>] [--message <fmail-id>] [--edge backlog|messages-tree|messages-part|org|dock=<size>] \
[--view report|report-preview|bell|bell-wide|bell-full|feed|row-menu|hover|cursor|tab-menu|sideline-menu|composer|split-menu|settings|settings-config|settings-config-filter|settings-config-toggle|settings-config-cycle|settings-config-page|settings-config-detail|settings-config-error|settings-config-paths|mods|mods-gallery|mods-confirm|keys|keys-tags|org-block|org-table|org-recent|org-overview|org-graph|org-health|org-seat|org-feed|org-system|org-remote|org-peek|org-spawn|org-keys|messages-tree|messages-chats|messages-thread|messages-system|messages-archive|backlog-list|backlog-search|backlog-kanban|backlog-graph|review-diff|review-panel|review-panel-drag|navigator-pr|navigator-model|navigator-menu|feed-detail|question-detail|persona-detail|persona-question|persona-decision|persona-list-before|persona-list|persona-edit-after|persona-note-composer|persona-note-after|persona-editor-pane|persona-drill|bell-personas|connections|connections-use|needs|dock|machines]";

#[derive(Debug, PartialEq)]
pub enum Format {
    Html,
    Svg,
    Png,
}

/// A client-local surface a snapshot opens before composing. These live in
/// the client's own state, so the server frame never carries them; the seam
/// flips the same state the live keys flip, then paints.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ViewKind {
    Report,
    ReportPreview,
    /// The notifications bell panel, with one synchronous gather like the
    /// live panel's `maybe_kick`.
    Bell,
    /// The bell panel wide (the `w` toggle), on the Questions tab with the
    /// first question marked, so one shot shows width, marks and the
    /// marked-count footer together.
    BellWide,
    /// The bell panel full screen (the `F` toggle), covering the panes.
    BellFull,
    /// The activity feed panel, with one synchronous page fold like the
    /// live panel's run-loop kick.
    Feed,
    /// The sideline row context menu on the first agent row.
    RowMenu,
    /// The pointer resting on the third agent row, away from the chosen one.
    Hover,
    /// The selector cursor on the fourth agent row, away from the chosen one.
    Cursor,
    /// The tab-strip context menu on the first tab.
    TabMenu,
    /// The sideline with the pinned Needs input section open, as a tap on the
    /// strip count opens it.
    NeedsInput,
    /// The sideline strip with the need-input mark frozen pointing up.
    NeedsInputPulseOn,
    /// The same strip frozen pointing down, half a turn on.
    NeedsInputPulseOff,
    /// The sideline menu popup.
    SidelineMenu,
    /// HQ with no seat: the designed empty state, full screen.
    HqEmpty,
    /// The composer opened from the empty HQ, with a task typed.
    HqComposer,
    /// The new-workspace form, with a name typed.
    NewWorkspace,
    /// A workspace section's menu.
    WorkspaceMenu,
    /// The first workspace's draft card, open with a seeded draft so the
    /// editor text and the real terminal cursor's cell are in the picture.
    Composer,
    /// The same draft folded: the ghost row under the workspace header.
    ComposerGhost,
    /// The card after a refused launch: the reason stays in the card.
    ComposerRefused,
    /// The open card after n presses of Tab from the prompt.
    ComposerChip(usize),
    /// The open card with the model chip's list unfolded.
    ComposerPicker,
    /// The rest card with `+op` typed: the completion list under the prompt.
    ComposerComplete,
    /// The mode chip's sandbox list, on codex.
    ComposerMode,
    /// The approval list after a sandbox pick.
    ComposerApproval,
    /// The row context menu on the first PANELESS bg row: the Split
    /// Direction group with its pane|portal toggle, the branch the first
    /// agent row (pane-hosted) never renders.
    SplitMenu,
    /// The settings modal at its top level: the tab strip the tab cycle
    /// walks.
    Settings,
    /// The settings modal's Config tab: the full schema list behind the
    /// filter box.
    SettingsConfig,
    /// The Config tab with the query narrowed to one toggle row, flipped,
    /// the notice naming the write.
    SettingsConfigFilter,
    SettingsConfigToggle,
    SettingsConfigCycle,
    SettingsConfigPage,
    SettingsConfigDetail,
    SettingsConfigError,
    /// The Config tab with the Paths group open: the folded path roots,
    /// each painting its resolved default (dim) or its shortened override.
    SettingsConfigPaths,
    /// Settings on the Remote hosts tab, two seeded hosts showing both
    /// state words.
    RemoteHosts,
    /// The Remote hosts add view: header, field and help lines.
    RemoteHostsAdd,
    /// The Remote hosts list with one host armed for removal, so the
    /// confirm row is in the picture.
    RemoteHostsRemove,
    /// The Remote hosts list with one host's row rendering as the inline
    /// edit input, prefilled with the stored address.
    RemoteHostsEdit,
    /// The Remote hosts add view holding the folded duplicate name with
    /// the refusal notice up.
    RemoteHostsDup,
    /// Settings on the Mods tab: a first-party pack and a URL pack.
    Mods,
    /// The Mods tab with the published gallery drawn under it.
    ModsGallery,
    /// The second press that turns on a mod that runs code.
    ModsConfirm,
    /// The which-key table, opened FROM settings: the printed key labels
    /// (`ctrl+b`) live here.
    Keys,
    /// The which-key table filtered to `split`, under the keymap the demo
    /// root's `keybindings.json` resolves: preset and yours rows carry tags.
    KeysTags,
    /// The sideline with the other-machines layer on, after one synchronous
    /// read of the fleet map like the live loop's first one.
    Machines,
    /// The expanded org fold block on the sideline (the prefix+C surface),
    /// with one inline machine fold so the lines carry real readings.
    OrgBlock,
    /// The org view at one of its five tabs over the whole fleet.
    OrgTable,
    OrgRecent,
    OrgOverview,
    OrgGraph,
    OrgHealth,
    /// The org view's first seat page, its FEED page or its SYSTEM page.
    OrgSeat,
    OrgFeed,
    OrgSystem,
    /// A seat on another machine with its relay keys, its peek page, the
    /// spawn prompt open on that machine, and the board's keys page.
    OrgRemote,
    OrgPeek,
    OrgSpawn,
    OrgKeys,
    /// The Messages board with focus on column 1, 2 or 3.
    MessagesTree,
    MessagesChats,
    MessagesThread,
    /// The Messages board with column 2's System or Archive tab active.
    MessagesSystem,
    MessagesArchive,
    /// The Messages board on the You tab: the operator's typed-turn threads.
    MessagesYou,
    /// The board on the newest You thread's detail, pairs right and left.
    MessagesYouThread,
    /// The board on the pinned `Your asks` row: every session's typed asks
    /// flattened, pending first.
    MessagesAsks,
    /// The board on the asks row in topic mode: one row per topic.
    MessagesAsksTopics,
    /// The PR review overlay over the fixture diff, with two staged comments.
    ReviewDiff,
    /// The PR review as a right split beside the chat panes.
    ReviewPanel,
    /// The review split mid-drag at 60 percent, its edge in the accent.
    ReviewPanelDrag,
    /// The backlog board full screen in list view, details pane beside it.
    BacklogList,
    BacklogSearch,
    /// The backlog board full screen in kanban view, details pane beside it.
    BacklogKanban,
    /// The backlog board full screen in graph view, details pane beside it.
    BacklogGraph,
    /// The prefix+f navigator filtered by the first agent row's PR number.
    NavigatorPr,
    /// The prefix+f navigator filtered by the model name `opus`.
    NavigatorModel,
    /// The prefix+f navigator with the sideline's row menu (Ctrl-o) open on
    /// the first agent row.
    NavigatorMenu,
    /// The event provenance modal on the first feed row that is not a
    /// question, with a mouse selection dragged across its longest line.
    FeedDetail,
    /// The question page on the first open question, with a mouse selection
    /// dragged across its longest line.
    QuestionDetail,
    PersonaDetail,
    PersonaQuestion,
    PersonaDecision,
    /// The full board in the list view, the cursor on the customer row
    /// whose painted audience chip is visible: the before frame of the
    /// chip press.
    PersonaListBefore,
    /// The same staged board with the persona overlay the press opens:
    /// the after frame.
    PersonaList,
    /// The detail page after a simulated user edit round-trips: a job
    /// landed in the file, the poll reloaded the page in place.
    PersonaEditAfter,
    /// The detail page with the note composer open, text typed.
    PersonaNoteComposer,
    /// The detail page after the note saved: status draft, the note line
    /// in notes.md, the approve key back on the footer.
    PersonaNoteAfter,
    /// The detail page with the editor pane open beside it: the pane `e`
    /// spawns, showing the persona file in the editor.
    PersonaEditorPane,
    /// The drill: the persona column docked right of its host with the
    /// first serving record open in the board's detail column beside it.
    PersonaDrill,
    /// The drill past the three-column cap: a fourth drill folds the
    /// oldest record into the breadcrumb strip riding the oldest visible
    /// record pane.
    PersonaDrill4,
    PersonaBellDock,
    PersonaSidelineChip,
    BellPersonas,
    /// The connections overlay (prefix+A) on its Accounts tab, with the
    /// reads the live open kicks run inline.
    Connections,
    /// The connections overlay with the `u` confirm staged on the first
    /// account that is not active.
    ConnectionsUse,
    /// The connections overlay with the add wizard open on its provider
    /// picker.
    ConnectionsAdd,
    /// The add picker with a search typed into it.
    ConnectionsAddSearch,
    /// The add wizard on a local server's base URL step.
    ConnectionsAddLocal,
    /// The sweep-threads choice modal over a staged fleet's counts.
    Sweep,
    /// The update modal on a wheel install a newer release can move.
    UpdateWheel,
    /// The update modal on a binary install with a newer release.
    UpdateBinary,
    /// The update modal on an install below the supported release floor.
    UpdateUnsupported,
    /// The server-skew popup against the pair the 2026-11-03 repro named:
    /// server v120, client v122.
    ServerSkew,
    /// The needs-me overlay with its first row picked, after the same
    /// questions gather the bell shots run.
    Needs,
    /// The sideline list with the first agent's reply dock open under it.
    Dock,
    /// The Decisions tab over a fixed set of rows, two of them unattributed.
    Decisions,
    /// The Decisions tab in book mode: standing law grouped by section, one
    /// current ruling per topic, no ids.
    LawBook,
    /// The bell panel with the audit queue row on top of its All tab.
    BellAudit,
    /// The bell panel on its Decisions tab.
    BellDecisions,
    /// The bell panel after a tap on the audit alert: decisions with no author.
    BellAuditTab,
}

/// The seeded demo mail id a Messages view opens on when no --message
/// names one (scripts/ops/mux-demo-snapshot.sh writes it).
const DEMO_MESSAGE_ID: &str = "fmail-demo-2";

/// Open the navigator the way prefix+f does, with `query` typed. A query
/// that matches no row would shoot `no matches`, which proves nothing.
fn open_navigator(view: &mut View, query: String) -> Result<(), String> {
    let nav = super::NavView {
        query,
        state_filter: None,
        cursor: 0,
    };
    if view.nav_filtered(&nav).is_empty() {
        return Err(format!("navigator query {:?} matched no rows", nav.query));
    }
    view.nav = Some(nav);
    view.nav_esc.clear();
    Ok(())
}

/// Select the longest line of the open modal's body, the way a drag does:
/// compose once so the frame is recorded, then select. The caller's compose
/// paints the selection.
fn select_modal_line(view: &mut View) -> Result<(), String> {
    let _ = view.compose();
    crate::modal_select::select_longest_line()
        .map(|_| ())
        .ok_or_else(|| "the modal painted no body text to select".to_string())
}

/// Select the first seat on another machine, then open what the shot names
/// the way its key does. A peek page is the runner's own message fed in, so
/// no shot reaches over ssh.
fn remote_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
) -> Result<(), String> {
    use super::org_board::{host_key, seat_key, Act, Done, OrgMsg};
    let b = view.org_board.as_mut().ok_or("org view did not open")?;
    let remote = b.fleet.as_ref().and_then(|f| {
        let m = f.machines.iter().find(|m| !m.local)?;
        m.leads.iter().find_map(|p| {
            let s = p.seats.iter().find(|s| s.local.is_none())?;
            Some((
                host_key(m),
                seat_key(m, p, s),
                s.short.clone(),
                m.host.clone(),
                s.harness.clone().unwrap_or_default(),
            ))
        })
    });
    let (host, seat, short, on, agent) = remote.ok_or(
        "org view has no seat on another machine: shoot a root whose fleet map \
has one (scripts/ops/mux-demo-snapshot.sh seeds one)",
    )?;
    let spawn = kind == Some(&ViewKind::OrgSpawn);
    b.select(if spawn { &host } else { &seat });
    let gen = b.gen;
    let typed: &[&[u8]] = match kind {
        Some(ViewKind::OrgSpawn) => &[b"s", b"fix the flaky login test"],
        Some(ViewKind::OrgKeys) => &[b"?"],
        _ => &[],
    };
    let mut sink = tokio::io::sink();
    for bytes in typed {
        runtime.block_on(super::org_board::keys(view, bytes, &mut sink))?;
    }
    if kind == Some(&ViewKind::OrgPeek) {
        let lines = [
            format!("peer {short}: agent={agent} short_id={short} recorded_cwd=~/code/checkout"),
            "user: fix the retry budget in the payments client".into(),
            "assistant: The budget resets on every reconnect, so a flapping link retries forever."
                .into(),
            "assistant: Moving the reset to the first good response, then a test for the flap."
                .into(),
        ];
        let done = Done {
            act: Act::Peek,
            label: format!("peek {short}@{on}"),
            key: seat,
            result: Ok(lines.into()),
        };
        super::org_board::apply(view, gen, OrgMsg::Action(done));
    }
    Ok(())
}

fn parse_view(v: &str) -> Option<ViewKind> {
    if let Some(n) = v.strip_prefix("composer-tab-") {
        return n.parse().ok().map(ViewKind::ComposerChip);
    }
    VIEW_TABLE
        .iter()
        .find(|(name, _)| *name == v)
        .map(|(_, kind)| *kind)
}

/// The `--view` names parse_view answers, as one table. The
/// composer-tab-N prefix is handled in the function itself.
const VIEW_TABLE: &[(&str, ViewKind)] = &[
    ("composer-rest", ViewKind::ComposerChip(0)),
    ("composer-unfolded", ViewKind::ComposerChip(1)),
    ("composer-complete", ViewKind::ComposerComplete),
    ("bell", ViewKind::Bell),
    ("bell-wide", ViewKind::BellWide),
    ("bell-full", ViewKind::BellFull),
    ("feed", ViewKind::Feed),
    ("row-menu", ViewKind::RowMenu),
    ("hover", ViewKind::Hover),
    ("cursor", ViewKind::Cursor),
    ("needs-input", ViewKind::NeedsInput),
    ("needs-input-pulse-on", ViewKind::NeedsInputPulseOn),
    ("needs-input-pulse-off", ViewKind::NeedsInputPulseOff),
    ("tab-menu", ViewKind::TabMenu),
    ("sideline-menu", ViewKind::SidelineMenu),
    ("new-workspace", ViewKind::NewWorkspace),
    ("workspace-menu", ViewKind::WorkspaceMenu),
    ("hq-empty", ViewKind::HqEmpty),
    ("hq-composer", ViewKind::HqComposer),
    ("composer", ViewKind::Composer),
    ("composer-ghost", ViewKind::ComposerGhost),
    ("composer-refused", ViewKind::ComposerRefused),
    ("composer-picker", ViewKind::ComposerPicker),
    ("composer-mode", ViewKind::ComposerMode),
    ("composer-approval", ViewKind::ComposerApproval),
    ("split-menu", ViewKind::SplitMenu),
    ("settings", ViewKind::Settings),
    ("settings-config", ViewKind::SettingsConfig),
    ("settings-config-filter", ViewKind::SettingsConfigFilter),
    ("settings-config-toggle", ViewKind::SettingsConfigToggle),
    ("settings-config-cycle", ViewKind::SettingsConfigCycle),
    ("settings-config-page", ViewKind::SettingsConfigPage),
    ("settings-config-detail", ViewKind::SettingsConfigDetail),
    ("settings-config-error", ViewKind::SettingsConfigError),
    ("settings-config-paths", ViewKind::SettingsConfigPaths),
    ("remote-hosts", ViewKind::RemoteHosts),
    ("remote-hosts-add", ViewKind::RemoteHostsAdd),
    ("remote-hosts-remove", ViewKind::RemoteHostsRemove),
    ("remote-hosts-edit", ViewKind::RemoteHostsEdit),
    ("remote-hosts-dup", ViewKind::RemoteHostsDup),
    ("mods", ViewKind::Mods),
    ("mods-gallery", ViewKind::ModsGallery),
    ("mods-confirm", ViewKind::ModsConfirm),
    ("report", ViewKind::Report),
    ("report-preview", ViewKind::ReportPreview),
    ("keys", ViewKind::Keys),
    ("keys-tags", ViewKind::KeysTags),
    ("org-block", ViewKind::OrgBlock),
    ("org-table", ViewKind::OrgTable),
    ("org-recent", ViewKind::OrgRecent),
    ("org-overview", ViewKind::OrgOverview),
    ("org-graph", ViewKind::OrgGraph),
    ("org-health", ViewKind::OrgHealth),
    ("org-seat", ViewKind::OrgSeat),
    ("org-feed", ViewKind::OrgFeed),
    ("org-system", ViewKind::OrgSystem),
    ("org-remote", ViewKind::OrgRemote),
    ("org-peek", ViewKind::OrgPeek),
    ("org-spawn", ViewKind::OrgSpawn),
    ("org-keys", ViewKind::OrgKeys),
    ("messages-tree", ViewKind::MessagesTree),
    ("messages-chats", ViewKind::MessagesChats),
    ("messages-thread", ViewKind::MessagesThread),
    ("messages-system", ViewKind::MessagesSystem),
    ("messages-archive", ViewKind::MessagesArchive),
    ("messages-you", ViewKind::MessagesYou),
    ("messages-you-thread", ViewKind::MessagesYouThread),
    ("messages-asks", ViewKind::MessagesAsks),
    ("messages-asks-topics", ViewKind::MessagesAsksTopics),
    ("review-diff", ViewKind::ReviewDiff),
    ("review-panel", ViewKind::ReviewPanel),
    ("review-panel-drag", ViewKind::ReviewPanelDrag),
    ("backlog-list", ViewKind::BacklogList),
    ("backlog-search", ViewKind::BacklogSearch),
    ("backlog-kanban", ViewKind::BacklogKanban),
    ("backlog-graph", ViewKind::BacklogGraph),
    ("navigator-pr", ViewKind::NavigatorPr),
    ("navigator-model", ViewKind::NavigatorModel),
    ("navigator-menu", ViewKind::NavigatorMenu),
    ("feed-detail", ViewKind::FeedDetail),
    ("question-detail", ViewKind::QuestionDetail),
    ("persona-detail", ViewKind::PersonaDetail),
    ("persona-question", ViewKind::PersonaQuestion),
    ("persona-decision", ViewKind::PersonaDecision),
    ("persona-list-before", ViewKind::PersonaListBefore),
    ("persona-list", ViewKind::PersonaList),
    ("persona-edit-after", ViewKind::PersonaEditAfter),
    ("persona-note-composer", ViewKind::PersonaNoteComposer),
    ("persona-note-after", ViewKind::PersonaNoteAfter),
    ("persona-editor-pane", ViewKind::PersonaEditorPane),
    ("persona-drill", ViewKind::PersonaDrill),
    ("persona-drill4", ViewKind::PersonaDrill4),
    ("persona-bell-dock", ViewKind::PersonaBellDock),
    ("persona-sideline-chip", ViewKind::PersonaSidelineChip),
    ("bell-personas", ViewKind::BellPersonas),
    ("sweep", ViewKind::Sweep),
    ("update-wheel", ViewKind::UpdateWheel),
    ("update-binary", ViewKind::UpdateBinary),
    ("update-unsupported", ViewKind::UpdateUnsupported),
    ("server-skew", ViewKind::ServerSkew),
    ("connections", ViewKind::Connections),
    ("connections-use", ViewKind::ConnectionsUse),
    ("connections-add", ViewKind::ConnectionsAdd),
    ("connections-add-search", ViewKind::ConnectionsAddSearch),
    ("connections-add-local", ViewKind::ConnectionsAddLocal),
    ("needs", ViewKind::Needs),
    ("dock", ViewKind::Dock),
    ("decisions", ViewKind::Decisions),
    ("law-book", ViewKind::LawBook),
    ("bell-audit", ViewKind::BellAudit),
    ("bell-decisions", ViewKind::BellDecisions),
    ("bell-audit-tab", ViewKind::BellAuditTab),
    ("machines", ViewKind::Machines),
];

#[derive(Debug)]
pub struct SnapshotArgs {
    pub out: PathBuf,
    pub server: String,
    pub squad: Option<String>,
    pub theme: Theme,
    /// The shooter passed --theme: an explicit ask that outranks the config
    /// ladder's mux.theme in the paint.
    pub theme_given: bool,
    pub format: Format,
    /// `(rows, cols)` of the whole picture; live mode defaults to the server's.
    pub size: Option<(u16, u16)>,
    /// Attach as a sizing client, so the server lays its panes out at `size`.
    /// It resizes every pane, so it is for a throwaway server only.
    pub fit: bool,
    /// `(rows, cols)` of a second window held attached as a sizing client
    /// while the shot is taken, so the shot sees a tab two windows share.
    pub peer: Option<(u16, u16)>,
    /// The font family drawn first, ahead of the common monospace stack: an
    /// image has no terminal, so its font is named here or left to the stack.
    pub font: Option<String>,
    /// Open the Messages tab on this fmail id: the paint gate's shots of a
    /// client-local board (law d-9f18c4d5). The board gathers via a client
    /// subprocess, so the snapshot runs one gather synchronously; the id
    /// resolves to its thread and positions the cursor there.
    pub message: Option<String>,
    /// The client-local surface to open before composing, for shots of a
    /// bell panel or a menu no server frame ever carries.
    pub view: Option<ViewKind>,
    /// Open the `^B ?` footer grid over the view, as the chord does.
    pub keys_footer: bool,
    /// Splits held at a dragged size, as `name=size` pairs: the state a live
    /// drag leaves, so a shot can show a mid-drag frame.
    pub edges: Vec<(String, usize)>,
}

/// True when a `serve` tail asks for a snapshot rather than the web bridge.
pub fn wants_snapshot(tail: &[OsString]) -> bool {
    tail.iter().any(|a| a == "--snapshot")
}

/// Parse the `serve --snapshot` tail. `Err` is the usage line (exit 2).
pub fn parse(tail: &[OsString]) -> Result<SnapshotArgs, String> {
    let mut out = None;
    let mut server = None;
    let mut squad = None;
    let mut theme = frame_html::theme_by_name("dark").expect("dark is a snapshot theme");
    let mut theme_given = false;
    let mut format = None;
    let mut size = None;
    let mut fit = false;
    let mut peer = None;
    let mut font = None;
    let mut message = None;
    let mut view = None;
    let mut keys_footer = false;
    let mut edges: Vec<(String, usize)> = Vec::new();
    let mut it = tail.iter();
    while let Some(a) = it.next() {
        let a = a.to_str().ok_or_else(|| USAGE.to_string())?;
        let mut value = || -> Result<String, String> {
            it.next()
                .and_then(|v| v.to_str())
                .map(str::to_string)
                .ok_or_else(|| format!("fno mux serve --snapshot: {a} needs a value\n{USAGE}"))
        };
        match a {
            "--snapshot" => {}
            "--fit" => fit = true,
            "--keys-footer" => keys_footer = true,
            "--font" => {
                let f = value()?;
                if f.is_empty() || f.contains(['\'', '"', '<', '>', '&']) {
                    return Err(format!(
                        "fno mux serve --snapshot: bad --font {f:?}\n{USAGE}"
                    ));
                }
                font = Some(f);
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            "--message" => message = Some(value()?),
            "--edge" => {
                let v = value()?;
                let pair = v
                    .split_once('=')
                    .and_then(|(n, s)| Some((n.to_string(), s.parse::<usize>().ok()?)))
                    .filter(|(n, _)| super::edge_drag::Edge::from_name(n).is_some())
                    .ok_or_else(|| {
                        format!("fno mux serve --snapshot: --edge wants backlog|messages-tree|messages-part|org|dock=<size>, got {v:?}")
                    })?;
                edges.push(pair);
            }
            "--view" => {
                let v = value()?;
                view = Some(parse_view(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: unknown view {v:?}; use bell, bell-wide, bell-full, feed, row-menu, tab-menu, sideline-menu, new-workspace, workspace-menu, composer, composer-ghost, composer-refused, composer-tab-N, composer-picker, composer-mode, composer-approval, split-menu, settings, settings-config, settings-config-filter, settings-config-toggle, settings-config-cycle, settings-config-page, settings-config-detail, settings-config-error, settings-config-paths, keys, org-block, org-table, org-recent, org-overview, org-graph, org-health, org-seat, org-feed, org-system, org-remote, org-peek, org-spawn, org-keys, messages-tree, messages-chats, messages-thread, messages-system, messages-archive, messages-you, messages-you-thread, messages-asks, messages-asks-topics, law-book, backlog-list, backlog-search, backlog-kanban, backlog-graph, review-diff, review-panel, review-panel-drag, navigator-pr, navigator-menu, navigator-model, feed-detail, question-detail, connections-use or connections")
                })?)
            }
            tok @ ("--server" | "--session") => {
                crate::mux_cli::note_server_flag(tok);
                server = Some(value()?)
            }
            "--squad" => squad = Some(value()?),
            "--theme" => {
                let v = value()?;
                theme = frame_html::theme_by_name(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: unknown theme {v:?}; use dark, light or macchiato")
                })?;
                theme_given = true;
            }
            "--format" => {
                format = Some(match value()?.as_str() {
                    "html" => Format::Html,
                    "svg" => Format::Svg,
                    "png" => Format::Png,
                    v => {
                        return Err(format!(
                            "fno mux serve --snapshot: unknown format {v:?}; use html, svg or png"
                        ))
                    }
                })
            }
            "--peer" => {
                let v = value()?;
                peer = Some(parse_size(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: --peer wants <cols>x<rows>, got {v:?}")
                })?)
            }
            "--size" => {
                let v = value()?;
                size = Some(parse_size(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: --size wants <cols>x<rows>, got {v:?}")
                })?)
            }
            other => {
                return Err(format!(
                    "fno mux serve --snapshot: unknown flag {other:?}\n{USAGE}"
                ))
            }
        }
    }
    let out = out.ok_or_else(|| format!("fno mux serve --snapshot: --out is required\n{USAGE}"))?;
    let server = server.ok_or_else(|| {
        "fno mux serve --snapshot: --server is required; for a public shot with no real \
session text, run scripts/ops/mux-demo-snapshot.sh"
            .to_string()
    })?;
    if fit && size.is_none() {
        return Err(format!(
            "fno mux serve --snapshot: --fit needs --size\n{USAGE}"
        ));
    }
    // The extension names the format when the flag does not.
    let format = match format {
        Some(f) => f,
        None => match out.extension().and_then(|e| e.to_str()) {
            Some("html") => Format::Html,
            Some("png") => Format::Png,
            _ => Format::Svg,
        },
    };
    Ok(SnapshotArgs {
        out,
        server,
        squad,
        theme,
        theme_given,
        format,
        size,
        fit,
        peer,
        font,
        message,
        view,
        keys_footer,
        edges,
    })
}

fn parse_size(v: &str) -> Option<(u16, u16)> {
    let (c, r) = v.split_once('x')?;
    let (cols, rows) = (c.parse::<u16>().ok()?, r.parse::<u16>().ok()?);
    (cols >= 40 && rows >= 10).then_some((rows, cols))
}

pub fn run(mut args: SnapshotArgs) -> i32 {
    // --theme is the shooter's explicit ask and wins. Without it the paint
    // resolves from the config ladder (mux.theme) the way a live client
    // latches at launch; a snapshot has no terminal to ask the light
    // question, so inference is out. The lens and the cells follow the one
    // resolution, so a shot never mixes two palettes.
    let (lens, chrome) = if args.theme_given {
        let chrome = match args.theme.name {
            "dark" => crate::theme::theme_footnote_superscript(),
            "light" => crate::theme::theme_footnote_paper(),
            _ => crate::theme::Theme::default_theme(),
        };
        (args.theme, chrome)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let (chrome, _, _) = crate::digest_overlay::theme_resolved(&cwd, None);
        let lens = frame_html::theme_by_name(if chrome.name == "footnote-paper" {
            "light"
        } else {
            "dark"
        })
        .unwrap_or(args.theme);
        (lens, chrome)
    };
    args.theme = lens;
    let frame = live_frame(
        &args.server,
        args.squad.as_deref(),
        args.size,
        args.fit,
        args.peer,
        chrome,
        args.message.as_deref(),
        args.view.as_ref(),
        args.keys_footer,
        &args.edges,
    );
    match frame.and_then(|f| write(&f, &args)) {
        Ok(()) => {
            println!("{}", args.out.display());
            0
        }
        Err(e) => {
            eprintln!("fno mux serve --snapshot: {e}");
            1
        }
    }
}

fn write(frame: &Frame, args: &SnapshotArgs) -> Result<(), String> {
    let font = |page: String| match &args.font {
        Some(f) => page.replace("'SF Mono',", &format!("'{f}','SF Mono',")),
        None => page,
    };
    let svg = || font(frame_html::frame_svg(frame, args.theme));
    let io = |e: std::io::Error| format!("cannot write {}: {e}", args.out.display());
    match args.format {
        Format::Html => {
            std::fs::write(&args.out, font(frame_html::screen_html(frame, args.theme))).map_err(io)
        }
        Format::Svg => std::fs::write(&args.out, svg()).map_err(io),
        Format::Png => {
            png(&svg(), frame, &args.out)?;
            if super::image_layer::painted() {
                stamp_no_image_protocol(&args.out)?;
            }
            Ok(())
        }
    }
}

/// The `tEXt` chunk a snapshot png carries when its frame painted an image.
/// The snapshot speaks no image protocol, so such a shot shows the half-block
/// path only. The question intake refuses on it (`question_intake.rs`).
const NO_IMAGE_PROTOCOL_STAMP: (&str, &str) = ("fno-snapshot", "no-image-protocol");

/// Insert the stamp as a `tEXt` chunk right after the png's `IHDR`.
fn stamp_no_image_protocol(out: &Path) -> Result<(), String> {
    let io = |e: std::io::Error| format!("stamp {}: {e}", out.display());
    let mut png = std::fs::read(out).map_err(io)?;
    // Signature (8) + IHDR: length (4), type (4), data (13), crc (4).
    let at = 33;
    if png.len() < at || &png[12..16] != b"IHDR" {
        return Err(format!(
            "stamp {}: not a png with IHDR first",
            out.display()
        ));
    }
    let (key, text) = NO_IMAGE_PROTOCOL_STAMP;
    let mut body = b"tEXt".to_vec();
    body.extend_from_slice(key.as_bytes());
    body.push(0);
    body.extend_from_slice(text.as_bytes());
    let mut chunk = ((body.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    png.splice(at..at, chunk);
    std::fs::write(out, png).map_err(io)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Rasterize through headless Chrome: the svg in a zero-margin page, a window
/// the exact size of the image, at 2x.
fn png(svg: &str, frame: &Frame, out: &Path) -> Result<(), String> {
    let chrome = find_chrome()
        .ok_or("png needs Chrome or Chromium: set FNO_CHROME to its binary, or use --format svg")?;
    let w = (frame.cols as f64 * frame_html::SVG_CELL_W).ceil() as u32;
    let h = (frame.rows as f64 * frame_html::SVG_CELL_H).ceil() as u32;
    let dir = std::env::temp_dir().join(format!("fno-snapshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let page = dir.join("shot.html");
    std::fs::write(&page, page_html(svg)).map_err(|e| format!("temp page: {e}"))?;
    let out_abs = std::path::absolute(out).map_err(|e| format!("{}: {e}", out.display()))?;
    // The png on disk is the success signal below, so an earlier shot at the
    // same path would read as this one landing and pass off the stale image.
    match std::fs::remove_file(&out_abs) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", out_abs.display()));
        }
        _ => {}
    }
    let mut cmd = std::process::Command::new(&chrome);
    // Chrome hangs on macOS under a HOME that is not the user's, and the demo
    // script sets one so paths read `~/...`. Hand it the passwd home back.
    if let Some(home) = crate::live_store_fence::passwd_home() {
        cmd.env("HOME", home);
    }
    let mut child = cmd
        .arg("--headless=new")
        // Without its own profile dir a headless shot exits 2 whenever the
        // user's own Chrome is running, which is most of the time.
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--force-device-scale-factor=2")
        .arg(format!("--window-size={w},{h}"))
        .arg(format!("--screenshot={}", out_abs.display()))
        .arg(format!("file://{}", page.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", chrome.display()))?;
    // Headless chrome can write the screenshot and then hang forever under
    // load, wedging the whole shot run. The png on disk is the success
    // signal: a short grace after it lands, then kill. A render with no png
    // gets a hard two-minute bound.
    let hard = Instant::now() + Duration::from_secs(120);
    let mut grace: Option<Instant> = None;
    let status;
    loop {
        if out_abs.exists() && grace.is_none() {
            grace = Some(Instant::now() + Duration::from_secs(5));
        }
        let bound = grace.map_or(hard, |g| g.min(hard));
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) if Instant::now() >= bound => {
                let png_landed = out_abs.exists();
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);
                if png_landed {
                    return Ok(());
                }
                return Err(format!(
                    "{} hung past 120s and wrote no png",
                    chrome.display()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(format!("cannot wait for {}: {e}", chrome.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    match status.expect("loop breaks on exit, bound, or a wait error") {
        s if s.success() && out_abs.exists() => Ok(()),
        s => Err(format!("{} exited {s} and wrote no png", chrome.display())),
    }
}

/// The zero-margin page png() rasterizes. overflow:hidden: the last row's
/// glyph descenders poke a fraction of a px past the svg height, and an
/// exact-fit window then screenshots a composite whose bottom tile repeats
/// the page's first rows (the doubled chrome the iMac reported). Clamping
/// the page kills the repeat.
fn page_html(svg: &str) -> String {
    format!(
        "<!doctype html><style>html,body{{margin:0;overflow:hidden}}svg{{display:block}}</style>{svg}"
    )
}

fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FNO_CHROME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mac = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.exists() {
        return Some(mac.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    ["google-chrome", "chromium", "chromium-browser"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |d| d.join(name)))
        .find(|p| p.is_file())
}

/// The split views open the fixture review beside the panes; the drag view
/// holds its edge mid-drag at 60 percent.
/// A few catalog providers for the add picker's Other section.
const DEMO_CATALOG: &str = r#"{
  "xai": {"name": "xAI", "env": ["XAI_API_KEY"], "api": "https://api.x.ai/v1", "npm": "@ai-sdk/xai", "models": {"grok-4": {}}},
  "groq": {"name": "Groq", "env": ["GROQ_API_KEY"], "api": "https://api.groq.com/openai/v1", "npm": "@ai-sdk/groq", "models": {"llama-3.3-70b": {}}},
  "mistral": {"name": "Mistral", "env": ["MISTRAL_API_KEY"], "api": "https://api.mistral.ai/v1", "npm": "@ai-sdk/mistral", "models": {"mistral-large": {}}},
  "togetherai": {"name": "Together AI", "env": ["TOGETHER_API_KEY"], "api": "https://api.together.xyz/v1", "npm": "@ai-sdk/togetherai", "models": {"qwen3": {}}},
  "fireworks-ai": {"name": "Fireworks AI", "env": ["FIREWORKS_API_KEY"], "api": "https://api.fireworks.ai/inference/v1", "npm": "@ai-sdk/openai-compatible", "models": {"kimi": {}}},
  "cerebras": {"name": "Cerebras", "env": ["CEREBRAS_API_KEY"], "api": "https://api.cerebras.ai/v1", "npm": "@ai-sdk/cerebras", "models": {"gpt-oss": {}}}
}"#;

fn split_fixture(view: &mut View, kind: Option<&ViewKind>) {
    if matches!(
        kind,
        Some(ViewKind::ReviewPanel | ViewKind::ReviewPanelDrag)
    ) {
        view.viewer.review = review_fixture().ok();
    }
    if matches!(kind, Some(ViewKind::ReviewPanelDrag)) {
        view.viewer.dragging_at(60);
    }
}

fn review_fixture() -> Result<super::backlog_board::pr_review::Review, String> {
    super::backlog_board::pr_review::fixture(include_str!("tests/fixtures/pr_review.diff"))
}

/// Everything one observer attach delivers before it goes quiet.
struct Observed {
    layout: LayoutView,
    frames: HashMap<u64, Frame>,
}

fn live_frame(
    server: &str,
    squad: Option<&str>,
    size: Option<(u16, u16)>,
    fit: bool,
    peer: Option<(u16, u16)>,
    chrome: crate::theme::Theme,
    message: Option<&str>,
    kind: Option<&ViewKind>,
    keys_footer: bool,
    edges: &[(String, usize)],
) -> Result<Frame, String> {
    let socket = proto::socket_path(server)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("runtime: {e}"))?;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A sizing client reports the content area, as a real client does. The
    // review split takes its columns before the server sizes the panes.
    let dims = size.filter(|_| fit).map(|t| {
        let mut sizing = View::new(t, server.into(), LayoutView::default());
        split_fixture(&mut sizing, kind);
        sizing.content_dims()
    });
    // The peer attaches first and stays until the shot is composed.
    let _peer = peer
        .map(|t| View::new(t, server.into(), LayoutView::default()).content_dims())
        .map(|d| runtime.block_on(hold_peer(&socket, cwd.clone(), d)))
        .transpose()?;
    let mut seen = runtime.block_on(observe(&socket, cwd.clone(), dims, &[]))?;
    switch_squad(&runtime, &socket, server, squad, dims, &mut seen)?;
    let mut view = base_view(server, seen, chrome, &cwd, size, edges);
    runtime.block_on(super::backlog_board::fold_once(&mut view));
    stage_messages(&runtime, &mut view, message, kind)?;
    stage_view(&runtime, &mut view, &socket, kind)?;
    if matches!(
        kind,
        Some(ViewKind::PersonaEditorPane | ViewKind::PersonaDrill4)
    ) {
        // The editor pane or the drill's record panes opened server-side
        // during staging, after the one observe at the top. Settle (vi
        // paints on its own tick), re-observe, and fold the fresh layout
        // and frames in.
        let fresh = runtime
            .block_on(async {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                observe(&socket, cwd.clone(), staged_dims(dims, &view), &[]).await
            })
            .map_err(|e| format!("persona-editor-pane: {e}"))?;
        view.layout = fresh.layout;
        view.frames = fresh.frames;
    } else if dims.is_some() && dims != Some(view.content_dims()) {
        // Staging can move the chrome (the chip shot widens the board
        // panel): the panes must re-tile for the staged viewport, or the
        // right column crops at the canvas edge with its border clipped.
        let fresh = runtime
            .block_on(observe(&socket, cwd.clone(), staged_dims(dims, &view), &[]))
            .map_err(|e| format!("staged viewport: {e}"))?;
        view.layout = fresh.layout;
        view.frames = fresh.frames;
    }
    // The persona column's width claim reads post-split rects, so the
    // fresh layout is when the seams can be computed and sent; the
    // follow-up observe folds the row they shaped.
    let seams = crate::client::persona_overlay::persona_widening_seams(&mut view);
    if !seams.is_empty() {
        let fresh = runtime
            .block_on(observe(
                &socket,
                cwd.clone(),
                staged_dims(dims, &view),
                &seams,
            ))
            .map_err(|e| format!("persona seam observe: {e}"))?;
        view.layout = fresh.layout;
        view.frames = fresh.frames;
    }
    if keys_footer {
        reinstall_keymap();
        super::key_grid::toggle(&mut view);
    }
    crate::lattice::freeze_spin();
    Ok(view.compose())
}

/// The observe dims after staging: the staged view's own content dims
/// when a fit-sized shot's chrome moved, else the sizing dims unchanged.
fn staged_dims(dims: Option<(u16, u16)>, view: &View) -> Option<(u16, u16)> {
    if dims.is_some() {
        Some(view.content_dims())
    } else {
        None
    }
}

/// A shot that cannot be staged; the text is the error `live_frame` returns.
struct ShotError(String);

impl From<String> for ShotError {
    fn from(e: String) -> Self {
        Self(e)
    }
}

impl From<&str> for ShotError {
    fn from(e: &str) -> Self {
        Self(e.to_string())
    }
}

impl From<ShotError> for String {
    fn from(e: ShotError) -> Self {
        e.0
    }
}

/// Re-attach from the named squad's cwd when the server picked another.
fn switch_squad(
    runtime: &tokio::runtime::Runtime,
    socket: &Path,
    server: &str,
    squad: Option<&str>,
    dims: Option<(u16, u16)>,
    seen: &mut Observed,
) -> Result<(), ShotError> {
    if let Some(name) = squad {
        let target = seen
            .layout
            .squads
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| {
                let names: Vec<&str> = seen.layout.squads.iter().map(|s| s.name.as_str()).collect();
                format!(
                    "server {server:?} has no squad {name:?}; it has {}",
                    names.join(", ")
                )
            })?;
        if target.id != seen.layout.active_squad {
            // The server picks the squad from the attach cwd.
            let cwd = target.canonical_cwd.clone();
            *seen = runtime.block_on(observe(socket, cwd, dims, &[]))?;
        }
    }
    Ok(())
}

/// The client view over what the observer saw, sized and themed the way a
/// live client in this cwd would be.
fn base_view(
    server: &str,
    seen: Observed,
    chrome: crate::theme::Theme,
    cwd: &str,
    size: Option<(u16, u16)>,
    edges: &[(String, usize)],
) -> View {
    let area = seen.layout.area;
    let mut view = View::new(area, server.into(), seen.layout);
    view.theme = chrome;
    // The snapshot paints what a live client in this cwd would, so the
    // card-graph config latches here the way attach_and_run does.
    view.card_graph = crate::digest_overlay::card_graph(Path::new(cwd));
    view.frames = seen.frames;
    view.resource_meter_text = super::meter::snapshot_meter_text(
        crate::digest_overlay::load_readout_detailed(Path::new(cwd)),
    );
    view.resource_meter_on = view.resource_meter_text.is_some();
    view.org.load_capacity();
    view.term = match size {
        Some(t) => t,
        None => {
            // Grow the picture by exactly the chrome, so the content area
            // matches the rects the server computed. Twice, because the
            // sideline width depends on the terminal width.
            for _ in 0..2 {
                let (cr, cc) = view.content_dims();
                view.term = (
                    view.term.0 + area.0.saturating_sub(cr),
                    view.term.1 + area.1.saturating_sub(cc),
                );
            }
            view.term
        }
    };
    for (name, size) in edges {
        if let Some(edge) = super::edge_drag::Edge::from_name(name) {
            view.edge_sizes.set(edge, *size);
        }
    }
    view
}

/// The Messages board a --message or Messages view opens.
fn stage_messages(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    message: Option<&str>,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    // The Messages board gathers via a client-side subprocess and the
    // snapshot has no event loop, so the tab would paint empty. --message
    // opens the board on that fmail id and runs one gather synchronously;
    // the paint gate's shots come from here (law d-9f18c4d5).
    let messages_shot = messages_focus(kind);
    let message = message.or(messages_shot.map(|_| DEMO_MESSAGE_ID));
    if let Some(id) = message {
        gather_messages(runtime, view, kind, id)?;
        position_messages(view, kind, messages_shot);
    }
    Ok(())
}

/// A Messages view: the column that holds focus and column 2's tab.
fn messages_focus(kind: Option<&ViewKind>) -> Option<(Col, ChatTab)> {
    match kind {
        Some(ViewKind::MessagesTree) => Some((Col::Tree, ChatTab::Chats)),
        Some(ViewKind::MessagesChats) => Some((Col::Chats, ChatTab::Chats)),
        Some(ViewKind::MessagesThread) => Some((Col::Thread, ChatTab::Chats)),
        Some(ViewKind::MessagesSystem) => Some((Col::Chats, ChatTab::System)),
        Some(ViewKind::MessagesArchive) => Some((Col::Chats, ChatTab::Archive)),
        Some(ViewKind::MessagesYou) => Some((Col::Chats, ChatTab::Chats)),
        Some(ViewKind::MessagesYouThread) => Some((Col::Thread, ChatTab::Chats)),
        Some(ViewKind::MessagesAsks) => Some((Col::Thread, ChatTab::Chats)),
        Some(ViewKind::MessagesAsksTopics) => Some((Col::Thread, ChatTab::Chats)),
        _ => None,
    }
}

/// Open the board on `id` and run one synchronous gather into it.
fn gather_messages(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
    id: &str,
) -> Result<(), ShotError> {
    crate::client::messages_view::open_message(view, id.to_string());
    let gen = view
        .messages_board
        .as_ref()
        .map(|b| b.gen)
        .unwrap_or_default();
    let mail = runtime.block_on(crate::messages_model::gather());
    // A board with no threads paints a pane of chrome around nothing -
    // exactly the shot law d-9f18c4d5 exists to stop. Read the gather
    // before apply moves it, and refuse.
    let board_empty = mail
        .as_ref()
        .ok()
        .and_then(|p| p.get("threads"))
        .and_then(Value::as_array)
        .is_none_or(|t| t.is_empty());
    // The You views refuse the same way: a gather with no `you` rows
    // paints "(no You threads)" and proves nothing (law d-9f18c4d5).
    let you_empty = matches!(
        kind,
        Some(
            ViewKind::MessagesYou
                | ViewKind::MessagesYouThread
                | ViewKind::MessagesAsks
                | ViewKind::MessagesAsksTopics
        )
    ) && mail
        .as_ref()
        .ok()
        .and_then(|p| p.get("you"))
        .and_then(Value::as_array)
        .is_none_or(|t| t.is_empty());
    crate::client::messages_view::apply_gather(view, gen, mail.map(std::sync::Arc::new));
    if board_empty || you_empty {
        return Err(
            "messages board rendered empty: mail-threads returned no threads; \
shoot a root whose chats store has rows (scripts/ops/mux-demo-snapshot.sh seeds one)"
                .into(),
        );
    }
    Ok(())
}

/// Put the board's focus and selection where the named Messages view shows them.
fn position_messages(
    view: &mut View,
    kind: Option<&ViewKind>,
    messages_shot: Option<(Col, ChatTab)>,
) {
    if let (Some((col, tab)), Some(b)) = (messages_shot, view.messages_board.as_mut()) {
        b.col = col;
        b.col2 = tab;
    }
    // The You views select the first agent with a Me row (the user's own
    // thread one step down); the thread view then opens that Me thread.
    let you_view = matches!(
        kind,
        Some(ViewKind::MessagesYou | ViewKind::MessagesYouThread)
    );
    if let (true, Some(b)) = (you_view, view.messages_board.as_mut()) {
        let rows = b.tree_rows();
        let pick = rows.iter().enumerate().find_map(|(i, r)| match r {
            crate::client::messages_view::TreeRow::Agent { key, .. }
                if b.chat_rows(key).iter().any(|c| {
                    matches!(c, crate::client::messages_view::ChatRow::YouThread { .. })
                }) =>
            {
                Some((i, key.clone()))
            }
            _ => None,
        });
        if let Some((i, key)) = pick {
            b.cursors[0] = i;
            b.sel_agent = Some(key);
        }
    }
    if let (Some(ViewKind::MessagesYouThread), Some(b)) = (kind, view.messages_board.as_mut()) {
        if let Some(sid) = b.active_chat_rows().iter().find_map(|row| match row {
            crate::client::messages_view::ChatRow::YouThread { session_id, .. } => {
                Some(session_id.clone())
            }
            _ => None,
        }) {
            b.sel_thread = Some(format!("you:{sid}"));
            b.cursors[2] = b.you_items(&sid).len().saturating_sub(1);
        }
    }
    if let (Some(ViewKind::MessagesAsks), Some(b)) = (kind, view.messages_board.as_mut()) {
        b.sel_thread = Some("asks".to_string());
        b.cursors[2] = 0;
        b.col = crate::client::messages_view::Col::Thread;
    }
    if let (Some(ViewKind::MessagesAsksTopics), Some(b)) = (kind, view.messages_board.as_mut()) {
        b.sel_thread = Some("asks".to_string());
        b.asks_view = crate::client::messages_view::AsksView::Topics;
        b.cursors[2] = 0;
        b.col = crate::client::messages_view::Col::Thread;
    }
}

/// The client state the --view shot flips before compose.
fn stage_view(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    // The messages seam above is the shape: flip the client state the live
    // keys flip, run one synchronous gather where the live panel would kick
    // one, then compose. Menus build from the layout the server sent, so
    // they need no gather.
    if stage_gathered(runtime, view, socket, kind)?
        || stage_menu(view, kind)?
        || stage_modal(view, kind)?
    {
        return Ok(());
    }
    stage_board(view, kind)
}

/// The views whose overlay reads a gather the live loop would kick. False
/// when the view is not one of them.
fn stage_gathered(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
    kind: Option<&ViewKind>,
) -> Result<bool, ShotError> {
    match kind {
        Some(ViewKind::Bell)
        | Some(ViewKind::BellWide)
        | Some(ViewKind::BellFull)
        | Some(ViewKind::BellAudit)
        | Some(ViewKind::BellDecisions)
        | Some(ViewKind::BellAuditTab)
        | Some(ViewKind::BellPersonas) => bell_shot(runtime, view, kind)?,
        Some(ViewKind::Feed) | Some(ViewKind::FeedDetail) => feed_shot(runtime, view, kind)?,
        Some(ViewKind::QuestionDetail) => question_detail_shot(runtime, view)?,
        Some(ViewKind::PersonaBellDock) => persona_bell_dock_shot(runtime, view)?,
        Some(
            kind @ (ViewKind::PersonaDetail
            | ViewKind::PersonaQuestion
            | ViewKind::PersonaDecision
            | ViewKind::PersonaListBefore
            | ViewKind::PersonaList
            | ViewKind::PersonaEditAfter
            | ViewKind::PersonaNoteComposer
            | ViewKind::PersonaNoteAfter
            | ViewKind::PersonaEditorPane
            | ViewKind::PersonaDrill
            | ViewKind::PersonaDrill4
            | ViewKind::PersonaSidelineChip),
        ) => persona_chip_shot(runtime, view, socket, kind)?,
        Some(kind @ (ViewKind::NeedsInputPulseOn | ViewKind::NeedsInputPulseOff)) => {
            pulse_shot(runtime, view, kind)?;
        }
        Some(ViewKind::NeedsInput) => needs_input_shot(runtime, view)?,
        Some(ViewKind::Needs) => {
            let questions = runtime.block_on(crate::needs_overlay::questions_now());
            view.apply_questions_fold(questions);
            view.answers = Some(0);
        }
        Some(ViewKind::Machines) => machines_shot(runtime, view)?,
        Some(ViewKind::OrgBlock) => org_block_shot(runtime, view)?,
        Some(
            kind @ (ViewKind::Connections
            | ViewKind::ConnectionsUse
            | ViewKind::ConnectionsAdd
            | ViewKind::ConnectionsAddSearch
            | ViewKind::ConnectionsAddLocal),
        ) => connections_shot(runtime, view, kind)?,
        Some(
            ViewKind::OrgTable
            | ViewKind::OrgRecent
            | ViewKind::OrgOverview
            | ViewKind::OrgGraph
            | ViewKind::OrgHealth
            | ViewKind::OrgSeat
            | ViewKind::OrgFeed
            | ViewKind::OrgSystem
            | ViewKind::OrgRemote
            | ViewKind::OrgPeek
            | ViewKind::OrgSpawn
            | ViewKind::OrgKeys,
        ) => org_view_shot(runtime, view, kind)?,
        Some(ViewKind::Report) => runtime.block_on(super::report::snapshot(view, false))?,
        Some(ViewKind::ReportPreview) => runtime.block_on(super::report::snapshot(view, true))?,
        Some(ViewKind::BacklogList)
        | Some(ViewKind::BacklogSearch)
        | Some(ViewKind::BacklogKanban)
        | Some(ViewKind::BacklogGraph) => backlog_shot(runtime, view, kind)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn bell_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    let gen = crate::client::bell::open(view);
    let mut projection = runtime.block_on(crate::messages_model::gather());
    if let (
        Some(
            ViewKind::BellAudit
            | ViewKind::BellDecisions
            | ViewKind::BellAuditTab
            | ViewKind::BellPersonas,
        ),
        Ok(Value::Object(m)),
    ) = (kind, &mut projection)
    {
        m.insert(
                    "decision_facts".into(),
                    serde_json::json!({"audit": 3, "total": 4, "audit_rows": [
                        {"text": "2026-10-30 expired push: Retry after the rate limit", "question_id": null},
                        {"text": "2026-10-29 live ci: Keep the tier keeper on", "question_id": null},
                        {"text": "2026-10-28 live mux: Use the shared pane for retract", "question_id": "q-demo-2"}
                    ], "rows": [
                        {"text": "2026-11-03 live mux: Ship the Decisions tab whole", "question_id": "q-demo-1"},
                        {"text": "2026-11-02 live ci: Hold CI until the ranked PRs land", "question_id": null},
                        {"text": "2026-10-30 expired push: Retry after the rate limit", "question_id": null},
                        {"text": "2026-10-28 live mux: Use the shared pane for retract", "question_id": "q-demo-2"}
                    ]}),
                );
    }
    // Same refusal as the messages board: an announcements-less
    // gather paints the bell as "nothing here", a shot with nothing
    // to judge (law d-9f18c4d5).
    let bell_empty = projection
        .as_ref()
        .ok()
        .and_then(|p| p.get("announcements"))
        .and_then(Value::as_array)
        .is_none_or(|a| a.is_empty());
    crate::client::bell::apply(view, gen, projection);
    if bell_empty {
        return Err(
            "bell view rendered empty: the messages projection has no announcements; \
shoot a root whose chats store has rows (scripts/ops/mux-demo-snapshot.sh seeds one)"
                .into(),
        );
    }
    // The Questions tab reads the fold the live loop kicks, and the
    // snapshot has no loop: without this gather the tab paints its
    // loading skeleton and the wide shot has no row to mark.
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    match kind {
        Some(ViewKind::BellWide) => {
            view.bell.size = super::bell::Size::Wide;
            view.bell.tab = super::bell::Tab::Questions;
            view.bell.selected = 0;
            if let Some(id) = crate::client::bell::first_question_id(view) {
                view.bell.marks.insert(id);
            }
        }
        Some(ViewKind::BellFull) => {
            view.bell.size = super::bell::Size::Full;
        }
        Some(ViewKind::BellDecisions) => {
            view.bell.tab = super::bell::Tab::Decisions;
            view.bell.size = super::bell::Size::Wide;
        }
        Some(ViewKind::BellAuditTab) => {
            view.bell.tab = super::bell::Tab::Decisions;
            view.bell.size = super::bell::Size::Wide;
            view.bell.audit_only = true;
        }
        Some(ViewKind::BellPersonas) => {
            view.bell.tab = super::bell::Tab::Personas;
            view.bell.size = super::bell::Size::Wide;
        }
        _ => {}
    }
    Ok(())
}

fn feed_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    // The `e` key's state flip plus the fold the live panel's run
    // loop kicks, run inline: the whole history lands.
    view.feed = Some(super::feed_view::open_overlay(None, 0));
    let outcome = runtime.block_on(crate::feed_overlay::fetch_history());
    super::feed_view::apply_fold(view, 0, outcome.map(crate::feed_overlay::Land::Rows));
    // A fresh root has no feed rows, so the changed view rendered
    // with nothing to judge. Refuse the shot.
    if view.feed.as_ref().is_none_or(|f| f.win.items.is_empty()) {
        return Err("feed view rendered empty: this root has no feed rows; \
shoot a root seeded with questions, mail or backlog activity \
(scripts/ops/mux-demo-snapshot.sh seeds one)"
            .into());
    }
    if let Some(ViewKind::FeedDetail) = kind {
        // A question row opens the question page, so the provenance
        // modal opens on the first row that is not one.
        let item = view
            .feed
            .as_ref()
            .and_then(|f| f.win.items.iter().find(|i| i.kind != "question_asked"))
            .cloned()
            .ok_or("feed-detail: the feed holds only question rows to open")?;
        view.feed_detail = Some(super::feed_detail::modal(view, item));
        select_modal_line(view)?;
    }
    Ok(())
}

fn question_detail_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
) -> Result<(), ShotError> {
    // The same gather the bell shots run, then the page the bell
    // opens on a row.
    // The live bell reads the index first and the projection after, so
    // a failed projection read leaves the index row on the page.
    if let Some(index) = runtime.block_on(super::questions::questions_index_now()) {
        view.apply_questions_index(index);
    }
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    // FNO_SNAPSHOT_QUESTION names the question to open: an id, a bare
    // id or a number like Q3554. Unset, the first open question opens.
    let want = std::env::var("FNO_SNAPSHOT_QUESTION").ok();
    let id = match want.as_deref() {
        Some(want) => view
            .questions_merged()
            .and_then(|f| {
                f.items
                    .into_iter()
                    .find(|q| {
                        q.id == want
                            || q.id.split('@').next() == Some(want)
                            || q.number.as_deref() == Some(want)
                    })
                    .map(|q| q.id)
            })
            .ok_or_else(|| format!("question-detail: no question matches {want:?}"))?,
        None => super::bell::first_question_id(view).ok_or(
            "question-detail: no open question to show; shoot a root seeded with \
questions (scripts/ops/mux-demo-snapshot.sh seeds one)",
        )?,
    };
    view.open_detail_on(&id);
    if let Some(d) = &view.question_detail {
        d.wait_decoded(Duration::from_secs(10));
    }
    select_modal_line(view)?;
    Ok(())
}

/// The three chip surfaces with their overlays, and the bell's Personas
/// tab. Every shot shows the chip that opened the overlay: the node
/// detail's audience row, the question page's head chip, the decisions
/// table's audience cell. Demo rows carry a customer audience; the file
/// reads come from the seeded demo profile (the script exports the
/// customers dir at them).

/// Stage the board for the list chip-press pair: the full-screen board in
/// the list view, the cursor moved onto the first customer row so its
/// painted audience chip shows. Returns that row's audience.
fn stage_list_customer(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
) -> Result<Option<String>, ShotError> {
    view.sideline_view = crate::view_store::SidelineView::Backlog;
    runtime.block_on(super::backlog_board::fold_once(view));
    view.board_full = true;
    if let Some(b) = view.backlog_board.as_mut() {
        b.query.view = crate::backlog_model::View::List;
    }
    let found = view.backlog_board.as_ref().and_then(|b| {
        let body = b.body.as_ref()?;
        for (li, lane) in body.lanes.iter().enumerate() {
            for cell in &lane.cells {
                let Some(ci) = b
                    .layout
                    .columns
                    .iter()
                    .position(|n| *n == cell.column.as_str())
                else {
                    continue;
                };
                for (ri, card) in cell.cards.iter().enumerate() {
                    if super::persona_overlay::personas_in(card.audience.as_deref())
                        .first()
                        .is_some()
                    {
                        return Some((li, ci, ri, card.audience.clone()));
                    }
                }
            }
        }
        None
    });
    let Some((li, ci, ri, aud)) = found else {
        return Ok(None);
    };
    if let Some(b) = view.backlog_board.as_mut() {
        b.lane = li;
        b.col = ci;
        b.row = ri;
    }
    Ok(aud)
}

fn persona_chip_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
    kind: &ViewKind,
) -> Result<(), ShotError> {
    // The question arm reads the fold the live loop kicks; the snapshot
    // has no loop, so gather it here.
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    match kind {
        ViewKind::PersonaDetail => persona_detail_shot(runtime, view)?,
        ViewKind::PersonaList | ViewKind::PersonaListBefore => {
            persona_list_shot(runtime, view, kind)?
        }
        ViewKind::PersonaQuestion => {
            let picked = view.questions_merged().and_then(|fold| {
                fold.items
                    .iter()
                    .find(|q| {
                        q.state == "open"
                            && q.audience
                                .as_deref()
                                .map(|a| a.trim().starts_with("customer:"))
                                .unwrap_or(false)
                    })
                    .map(|q| (q.id.clone(), q.audience.clone()))
            });
            let (id, audience) =
                picked.ok_or("persona-question: no customer question in the demo fold")?;
            view.open_detail_on(&id);
            view.persona =
                super::persona_overlay::open_from(audience.as_ref(), None).map(|mut o| {
                    o.home = super::persona_overlay::Home::Question;
                    o
                });
        }
        ViewKind::PersonaDecision => persona_decision_shot(view)?,
        ViewKind::PersonaEditorPane => persona_editor_pane_shot(runtime, view, socket)?,
        ViewKind::PersonaDrill => persona_drill_shot(runtime, view)?,
        ViewKind::PersonaDrill4 => persona_drill4_shot(runtime, view, socket)?,
        ViewKind::PersonaSidelineChip => persona_sideline_chip_shot(runtime, view)?,
        ViewKind::PersonaEditAfter | ViewKind::PersonaNoteComposer | ViewKind::PersonaNoteAfter => {
            persona_edit_note_shot(runtime, view, kind)?
        }
        _ => {}
    }
    // The before frame of the chip-press pair is the board alone; the
    // persona overlay is the press's outcome, not a presence check.
    if !matches!(kind, ViewKind::PersonaListBefore) && view.persona.is_none() {
        return Err("persona chip shot: the surface carries no persona chip".into());
    }
    Ok(())
}

/// The canned serving question a persona demo frame shows beside the
/// board's own rows.
fn stage_serving_question(view: &mut View, aud: &str) {
    let Some(slug) = super::persona_overlay::personas_in(Some(aud))
        .first()
        .cloned()
    else {
        return;
    };
    view.questions_fold = Some(crate::needs_overlay::QuestionsFold {
        items: vec![crate::needs_overlay::QuestionItem {
            id: "q-demo".into(),
            title: "Approve the persona page?".into(),
            audience: Some(format!("customer:{slug}")),
            state: "open".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
}

fn persona_detail_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
) -> Result<(), ShotError> {
    // The host reads live demo state: the board fold fills the inputs,
    // the same read the live view runs. fold_once opens the board only on
    // the backlog sideline, so the shot sets the field the live gesture
    // sets first.
    view.sideline_view = crate::view_store::SidelineView::Backlog;
    runtime.block_on(super::backlog_board::fold_once(view));
    let id = view
        .backlog_board
        .as_ref()
        .and_then(|b| b.inputs.as_ref())
        .and_then(|inputs| {
            inputs
                .rows
                .iter()
                .find(|r| {
                    r.audience
                        .as_deref()
                        .map(|a| a.trim().starts_with("customer:"))
                        .unwrap_or(false)
                })
                .and_then(|r| r.id.clone())
        })
        .ok_or("persona-detail: no customer card in the demo board read")?;
    let nv = view
        .backlog_board
        .as_ref()
        .and_then(|b| b.inputs.as_ref())
        .and_then(|inputs| crate::backlog_model::node(inputs, &id))
        .ok_or("persona-detail: the customer card is not in the read")?;
    let card = nv.card.clone();
    // The live host is the full-screen board with the detail overlay
    // open; the persona overlay paints on top of it.
    view.board_full = true;
    if let Some(b) = view.backlog_board.as_mut() {
        b.detail = Some(super::node_detail::NodeDetailOverlay {
            node_id: id,
            trail: Vec::new(),
            sel: 0,
            scroll: 0,
        });
    }
    view.persona = super::persona_overlay::open_from_card(&card);
    // The serving-now section reads the same view: the board row the chip
    // came from, plus one canned question naming the same persona.
    let aud = card.audience.clone().unwrap_or_default();
    stage_serving_question(view, &aud);
    // An approved fixture is the after-approve frame; the toast is what
    // the real key sets beside the re-rendered page.
    if let Some(o) = &view.persona {
        if !o.is_draft() {
            view.set_notice(format!("persona approved: {}", o.title()));
        }
    }
    // The frame shows the serving cursor: j selects the first row.
    super::persona_overlay::keys(view, b"j");
    Ok(())
}

fn persona_list_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: &ViewKind,
) -> Result<(), ShotError> {
    let aud = stage_list_customer(runtime, view)?
        .ok_or("persona-list: no customer row in the demo board read")?;
    if matches!(kind, ViewKind::PersonaList) {
        let project = super::persona_overlay::session_project(view);
        view.persona = super::persona_overlay::open_from(Some(&aud), Some(&project));
        stage_serving_question(view, &aud);
        // The frame shows the serving cursor: j selects the first row.
        super::persona_overlay::keys(view, b"j");
    }
    Ok(())
}

fn persona_decision_shot(view: &mut View) -> Result<(), ShotError> {
    // The decisions convention: a snapshot is a demo frame, so the rows
    // are canned (decisions_shot). d-1 carries the audience.
    let rows = serde_json::from_value(serde_json::json!([
        {"decision_id":"d-1","decision":"Meter usage in whole units","subject":"x-aaaa","subject_node":"x-aaaa","subject_pr":"","subject_text":"","decided_by":"ada","authority_source":"operator","quote":"count whole units only","ts":"2026-11-03T09:00:00Z","lifecycle":"live","lane":"user","audience":"customer:solo-founder,customer:vibe-coder","job":["solo-founder/ship-first-feature"],"question":"Round partial units up?"},
        {"decision_id":"d-2","decision":"Hold the build until the queue drains","subject":"example/app#118","subject_node":"","subject_pr":"#118","subject_text":"","decided_by":"7c1e9a02-1111-4222-8333-444455556666","decided_by_name":"bo","authority_source":"role","ts":"2026-11-02T15:30:00Z","lifecycle":"live","lane":"agent"}
    ]))
    .map_err(|e| e.to_string())?;
    crate::client::decisions_view::open_rows(view, rows);
    let audience = view
        .decisions_board
        .as_ref()
        .and_then(|b| b.selected_row())
        .and_then(|r| r.audience.clone());
    view.persona = super::persona_overlay::open_from(audience.as_ref(), None).map(|mut o| {
        o.home = super::persona_overlay::Home::Decision;
        o
    });
    Ok(())
}

/// The drill frame: the page staged, then the first serving row
/// selected and drilled. The persona column stays; the record opens in
/// the board's detail column beside it.
fn persona_drill_shot(runtime: &tokio::runtime::Runtime, view: &mut View) -> Result<(), ShotError> {
    persona_detail_shot(runtime, view)?;
    super::persona_overlay::keys(view, b"j");
    super::persona_overlay::keys(view, b"\r");
    Ok(())
}

/// The drill past the three-column cap: four drills over four serving
/// node rows. The fourth drill folds the second record (the oldest
/// visible) into the breadcrumb strip riding the oldest visible pane.
/// The synthetic rows join the board read so the trail has the rows to
/// drill; the panes spawn through the same control round-trip the live
/// key sends, and the re-observe in live_frame folds their layout.
fn persona_drill4_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
) -> Result<(), ShotError> {
    // The record panes append beside a docked persona pane, so the shot
    // stages the sideline dock the live chip press opens.
    let aud = stage_list_customer(runtime, view)?
        .ok_or("persona-drill4: no customer row in the demo board read")?;
    view.board_full = false;
    view.panel_on = true;
    view.sideline_width = 80;
    let project = super::persona_overlay::session_project(view);
    view.persona = super::persona_overlay::open_from(Some(&aud), Some(&project))
        .map(super::persona_overlay::dock_sideline);
    super::persona_overlay::spawn_persona_pane(view);
    if view.persona.as_ref().is_some_and(|o| o.pane.is_none()) {
        return Err(format!(
            "persona-drill4: the persona pane never opened: {:?}",
            view.notice
        )
        .into());
    }
    settle_persona_layout(runtime, view, socket)?;
    seed_drill_rows(view);
    for _ in 0..4 {
        super::persona_overlay::keys(view, b"j");
        super::persona_overlay::keys(view, b"\r");
        settle_persona_layout(runtime, view, socket)?;
    }
    Ok(())
}

/// The server's next Layout push, as the live loop receives it between two
/// keypresses. A drill's split anchors on the rightmost pane of the layout
/// this client holds, so a stale layout puts each record left of the last.
/// The width claim stays armed: it answers once, on the final layout.
fn settle_persona_layout(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
) -> Result<(), ShotError> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fresh = runtime
        .block_on(async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            observe(socket, cwd, None, &[]).await
        })
        .map_err(|e| format!("persona-drill4 settle: {e}"))?;
    view.layout = fresh.layout;
    view.frames = fresh.frames;
    Ok(())
}

/// Three synthetic customer rows past the two the demo board already
/// serves, so the trail can drill four deep. Cloned from an existing
/// customer row so every field the board read carries stays valid.
fn seed_drill_rows(view: &mut View) {
    let seed = view
        .backlog_board
        .as_ref()
        .and_then(|b| b.inputs.as_ref())
        .and_then(|i| {
            i.rows.iter().find(|r| {
                r.audience
                    .as_deref()
                    .map(|a| a.trim().starts_with("customer:"))
                    .unwrap_or(false)
            })
        })
        .cloned();
    let Some(mut row) = seed else { return };
    let Some(inputs) = view.backlog_board.as_mut().and_then(|b| b.inputs.as_mut()) else {
        return;
    };
    for (i, title) in [
        "Rate the demo flow for a first-run founder",
        "Ship the receipt polish for a first-run founder",
        "Turn the checkout keys into one clap",
    ]
    .into_iter()
    .enumerate()
    {
        row.id = Some(format!("ab-drill{i}"));
        row.title = Some(title.into());
        inputs.rows.push(row.clone());
    }
}

/// The bell personas tab with one persona docked beside it: the bell
/// door frame, the page a Row::Persona press opens now.
fn persona_bell_dock_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
) -> Result<(), ShotError> {
    bell_shot(runtime, view, Some(&ViewKind::BellPersonas))?;
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    let aud = view.questions_merged().and_then(|fold| {
        fold.items
            .iter()
            .find(|q| {
                q.audience
                    .as_deref()
                    .map(|a| a.trim().starts_with("customer:"))
                    .unwrap_or(false)
            })
            .and_then(|q| q.audience.clone())
    });
    let slug = super::persona_overlay::personas_in(aud.as_deref())
        .into_iter()
        .next()
        .ok_or("persona-bell-dock: no customer persona in the demo fold")?;
    let project = super::persona_overlay::session_project(view);
    view.persona = Some(super::persona_overlay::open_docked_bell(&project, &slug));
    Ok(())
}

/// The sideline board chip door: the board lives in the left panel (not
/// full-screen), the press opens the page docked inside that panel.
fn persona_sideline_chip_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
) -> Result<(), ShotError> {
    let aud = stage_list_customer(runtime, view)?
        .ok_or("persona-sideline-chip: no customer row in the demo board read")?;
    view.board_full = false;
    view.panel_on = true;
    view.sideline_width = 80;
    let project = super::persona_overlay::session_project(view);
    view.persona = super::persona_overlay::open_from(Some(&aud), Some(&project))
        .map(super::persona_overlay::dock_sideline);
    super::persona_overlay::spawn_persona_pane(view);
    if let Some(o) = view.persona.as_ref() {
        if o.pane.is_none() {
            return Err(format!(
                "persona-sideline-chip: the persona pane never opened: {:?}",
                view.notice
            )
            .into());
        }
    }
    stage_serving_question(view, &aud);
    super::persona_overlay::keys(view, b"j");
    Ok(())
}
/// The editor-pane frame: the detail page staged, then the pane `e`
/// spawns, through the same PaneRun round-trip the live key sends.
fn persona_editor_pane_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    socket: &std::path::Path,
) -> Result<(), ShotError> {
    persona_detail_shot(runtime, view)?;
    super::persona_overlay::keys(view, b"e");
    let path = super::persona_overlay::take_edit_spawn(view)
        .ok_or("persona-editor-pane: the edit arm never fired")?;
    // The PaneRun rides an ATTACHED session (hold_peer), the way the live
    // client's socket does: the server opens the editor pane for a client
    // it knows, and the re-observe in shoot composes it beside the page.
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut sock = runtime.block_on(hold_peer(socket, cwd, (0, 0)))?;
    runtime.block_on(super::persona_overlay::spawn_edit(view, path, &mut sock));
    Ok(())
}

/// The e/n round-trip frames: the detail page staged, then the edit's
/// reload (a job lands in the file, the poll re-reads it), the note
/// composer with text typed, or the page after the note saved (status
/// draft, the note line in notes.md).
fn persona_edit_note_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: &ViewKind,
) -> Result<(), ShotError> {
    persona_detail_shot(runtime, view)?;
    let (path, _, _) = super::persona_overlay::persona_page_path(view)
        .ok_or("persona e/n frames: no persona page staged")?;
    match kind {
        // The user's edit, simulated at the file: one more job lands. The
        // arm predates the write (keys b"e" ran first), so the poll sees
        // the moved mtime and reloads through the live path.
        ViewKind::PersonaEditAfter => {
            super::persona_overlay::keys(view, b"e");
            let _ = super::persona_overlay::take_edit_spawn(view);
            let mut text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            text.push_str(
                "\n### see-it-edit\nThe mux editor round-trips a persona edit without leaving the pane.\n",
            );
            std::fs::write(&path, text).map_err(|e| e.to_string())?;
            super::persona_overlay::edit_poll(view);
        }
        ViewKind::PersonaNoteComposer => {
            super::persona_overlay::keys(view, b"n");
            super::persona_overlay::keys(view, b"lead with the demo");
        }
        ViewKind::PersonaNoteAfter => {
            super::persona_overlay::keys(view, b"n");
            super::persona_overlay::keys(view, b"lead with the demo");
            super::persona_overlay::keys(view, b"\r");
        }
        _ => {}
    }
    Ok(())
}

fn pulse_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: &ViewKind,
) -> Result<(), ShotError> {
    // The strip count reads the questions fold the live loop kicks;
    // the snapshot has no loop, so gather it or the pulse shot
    // undercounts the very surface it freezes.
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    if view.need_input_count() == 0 {
        return Err("nothing needs input, so the pulse would shoot empty".into());
    }
    let beat = if matches!(kind, ViewKind::NeedsInputPulseOn) {
        super::sideline::PULSE_ON
    } else {
        super::sideline::PULSE_OFF
    };
    super::sideline::PULSE_PIN.store(beat, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

fn needs_input_shot(runtime: &tokio::runtime::Runtime, view: &mut View) -> Result<(), ShotError> {
    // Same gather as the pulse views: without the fold the section
    // shot cannot show what it exists to judge.
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    // The panel groups by machine only when the other-machines layer
    // is on and the fleet map answers; a single-machine root keeps
    // the flat panel, so no refusal here. The layer lands before the
    // count, so a root whose only blocked row is remote still shoots.
    super::machine_tree::set_on(view, true);
    let read = runtime.block_on(super::org_seats::read_fleet());
    super::machine_tree::apply(view, read);
    if view.need_input_count() == 0 {
        return Err("nothing needs input, so the section would shoot empty".into());
    }
    view.needs_open = true;
    Ok(())
}

fn machines_shot(runtime: &tokio::runtime::Runtime, view: &mut View) -> Result<(), ShotError> {
    // A fleet map with fewer than two machines paints no layer, so
    // that shot is a plain sideline and refuses.
    super::machine_tree::set_on(view, true);
    let read = runtime.block_on(super::org_seats::read_fleet());
    super::machine_tree::apply(view, read);
    if !view.machine_tree.layered() {
        return Err(
            "machine layer rendered empty: fleet top answered fewer than two machines; \
shoot a root whose fleet map has a second machine (scripts/ops/mux-demo-snapshot.sh seeds one)"
                .into(),
        );
    }
    Ok(())
}

fn org_block_shot(runtime: &tokio::runtime::Runtime, view: &mut View) -> Result<(), ShotError> {
    // The prefix+C fold runs expanded-only in a reading: flip the
    // same state the key flips, then run the fold and the gate
    // inline where the live panel would spawn them. A fold with no
    // reading paints unknown lines - nothing to judge.
    view.org = crate::org_overlay::Panel::with_detail(true);
    view.org.expand();
    view.org.load_capacity();
    let seeded = std::env::var("FNO_SNAPSHOT_LANES").ok();
    let lanes = match seeded.and_then(|path| std::fs::read(path).ok()) {
        Some(raw) => crate::org_overlay::parse(&raw),
        None => runtime.block_on(crate::org_overlay::fold_now()),
    };
    let read = lanes.is_some();
    view.org.apply(crate::org_overlay::FoldPart::Lanes(lanes));
    // `FNO_SNAPSHOT_GATE` seeds the spawn-gate answer the way
    // `FNO_SNAPSHOT_LANES` seeds the fold: the live probe is a
    // child that can time out on a busy shooting host.
    let gate = match std::env::var("FNO_SNAPSHOT_GATE") {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("FNO_SNAPSHOT_GATE: {e}")),
        Err(_) => runtime.block_on(crate::org_overlay::gate_now()),
    };
    view.org.apply(crate::org_overlay::FoldPart::Gate(gate));
    view.open_org_modal();
    if !read {
        return Err(
            "org block rendered without a reading: fno doctor lanes returned none; \
shoot from a host where the lanes command answers"
                .into(),
        );
    }
    Ok(())
}

fn connections_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: &ViewKind,
) -> Result<(), ShotError> {
    view.open_connections();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    runtime.block_on(crate::connections_view::load_all(tx, view.conn_gen));
    let cv = view
        .connections
        .as_mut()
        .ok_or("connections overlay did not open")?;
    // A throwaway root reaches no real account, so the live probe and
    // identity legs only fail. Paint the cached reading the root seeds.
    let mut cached = true;
    while let Ok((_, outcome)) = rx.try_recv() {
        use crate::connections_view::ReadOutcome as R;
        match outcome {
            R::UsageFailed(_) | R::IdentityFailed(_) => {}
            R::Usage(_) if !cached => {}
            R::Usage(cells) => {
                cached = false;
                cv.apply_read(R::Usage(cells));
            }
            other => cv.apply_read(other),
        }
    }
    if cv.accounts.is_empty() {
        return Err(
            "connections view rendered empty: this root registers no accounts; \
shoot a root whose config lists accounts (scripts/ops/mux-demo-snapshot.sh seeds them)"
                .into(),
        );
    }
    if *kind == ViewKind::ConnectionsUse {
        cv.acct_sel = cv.accounts.iter().position(|a| !a.active).unwrap_or(0);
        cv.render_with(&view.layout.agents, usize::MAX);
        cv.on_key(b'u');
    }
    if matches!(
        kind,
        ViewKind::ConnectionsAdd | ViewKind::ConnectionsAddSearch | ViewKind::ConnectionsAddLocal
    ) {
        cv.on_key(b'a');
        // A throwaway root has no models.dev cache: seed a few
        // catalog providers so the Other section paints.
        if cv.catalog.is_none() {
            cv.catalog = crate::model_catalog::parse(DEMO_CATALOG).ok();
        }
        let keys: &[u8] = match kind {
            ViewKind::ConnectionsAddSearch => b"ai",
            ViewKind::ConnectionsAddLocal => b"ollama\r\r",
            _ => b"",
        };
        for k in keys {
            cv.on_key(*k);
        }
    }
    Ok(())
}

fn org_view_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    // The org view at one tab or page: open it the way the key
    // does, run one synchronous fleet read, then select what the
    // shot names. A fleet map with no seat refuses the shot.
    super::backlog_board::toggle_org_view(view);
    view.board_full = true;
    let inputs = runtime.block_on(super::org_seats::gather(
        &crate::backlog_view::graph_path(),
        view.layout.agents.clone(),
    ));
    let gen = view.org_generation;
    super::org_board::apply_fold(view, gen, inputs);
    let questions = runtime.block_on(crate::needs_overlay::questions_now());
    view.apply_questions_fold(questions);
    // The feed page folds its Requests rows in the same synchronous shape,
    // so the shot paints populated rows instead of "not read yet".
    if matches!(kind, Some(ViewKind::OrgFeed)) {
        let requests = runtime.block_on(crate::requests_model::gather());
        if let Some(b) = view.org_board.as_mut() {
            match requests {
                crate::requests_model::RequestsRead::Rows(rows) => {
                    b.requests = Some(rows);
                    b.requests_error = None;
                }
                crate::requests_model::RequestsRead::Failed(reason) => {
                    b.requests_error = Some(reason)
                }
            }
        }
    }
    let Some(b) = view.org_board.as_mut() else {
        return Err("org view did not open".into());
    };
    let first = b.fleet.as_ref().and_then(|f| {
        f.machines.iter().find_map(|m| {
            m.leads
                .iter()
                .find_map(|p| p.seats.first().map(|s| super::org_board::seat_key(m, p, s)))
        })
    });
    let Some(first) = first else {
        return Err("org view rendered empty: fleet top answered no seat; \
shoot a root whose fleet map has seats \
(scripts/ops/mux-demo-snapshot.sh seeds one)"
            .into());
    };
    b.tab = match kind {
        Some(ViewKind::OrgRecent) => crate::view_store::OrgTab::Recent,
        Some(ViewKind::OrgOverview) => crate::view_store::OrgTab::Overview,
        Some(ViewKind::OrgGraph) => crate::view_store::OrgTab::Graph,
        Some(ViewKind::OrgHealth) => crate::view_store::OrgTab::Health,
        _ => crate::view_store::OrgTab::Table,
    };
    match kind {
        Some(ViewKind::OrgSeat) => b.select(&first),
        Some(ViewKind::OrgFeed) => b.select("feed"),
        Some(ViewKind::OrgSystem) => b.select("system"),
        Some(ViewKind::OrgRemote | ViewKind::OrgPeek | ViewKind::OrgSpawn | ViewKind::OrgKeys) => {
            remote_shot(runtime, view, kind)?
        }
        _ => {}
    }
    Ok(())
}

fn backlog_shot(
    runtime: &tokio::runtime::Runtime,
    view: &mut View,
    kind: Option<&ViewKind>,
) -> Result<(), ShotError> {
    // The V-cycle backlog board, full screen so the board and
    // details panes lay out side by side. Set the sideline field
    // directly: the shot never writes the view prefs.
    view.sideline_view = crate::view_store::SidelineView::Backlog;
    runtime.block_on(super::backlog_board::fold_once(view));
    view.board_full = true;
    let b = view
        .backlog_board
        .as_mut()
        .ok_or("the backlog board did not open")?;
    let focus = super::backlog_board::cursor_card_id(b);
    b.query.view = match kind {
        Some(ViewKind::BacklogList) => crate::backlog_model::View::List,
        Some(ViewKind::BacklogGraph) => crate::backlog_model::View::Graph,
        _ => crate::backlog_model::View::Kanban,
    };
    if matches!(kind, Some(ViewKind::BacklogSearch)) {
        // `/` mid-typing: the input open on a half-typed qualifier.
        super::backlog_board::seed_find(b, "-p:p3");
    }
    if b.query.view == crate::backlog_model::View::Graph {
        b.query.focus = focus.clone();
        super::backlog_board::rederive(b);
        super::backlog_board::focus_card(b, focus.as_deref());
    }
    if !b.body.as_ref().is_some_and(|body| !body.lanes.is_empty()) {
        let read = b.inputs.as_ref().map_or_else(
            || "the gather never landed".to_string(),
            |i| {
                format!(
                    "gather read {} rows, rows_error {:?}, errors {:?}",
                    i.rows.len(),
                    i.rows_error,
                    b.errors
                )
            },
        );
        return Err(format!(
            "backlog view rendered empty ({read}); \
shoot a root seeded with nodes (scripts/ops/mux-demo-snapshot.sh seeds one)"
        )
        .into());
    }
    Ok(())
}

/// The row menus, hovers and section menus the sideline opens. False when
/// the view is not one of them.
fn stage_menu(view: &mut View, kind: Option<&ViewKind>) -> Result<bool, ShotError> {
    match kind {
        Some(ViewKind::Dock) => dock_shot(view)?,
        Some(ViewKind::RowMenu) => {
            let i = view
                .display_rows()
                .iter()
                .position(|r| matches!(r, super::DisplayRow::Agent(_)))
                .ok_or("no agent rows to open a row menu on")?;
            view.open_row_menu(i, Anchor::Center);
            // The shot must pair the menu with the row it acts on: the
            // selector moves to the menu's row, so the highlight names the
            // target the header names.
            view.selector = Some(i);
        }
        Some(kind @ (ViewKind::Hover | ViewKind::Cursor)) => {
            let agents = view.display_rows().into_iter().enumerate();
            let mut agents = agents.filter(|(_, r)| matches!(r, super::DisplayRow::Agent(_)));
            let hover = matches!(kind, ViewKind::Hover);
            let i = agents.nth(if hover { 2 } else { 3 }).map(|(i, _)| i);
            if hover {
                view.hover_row = i;
            } else {
                view.selector = i;
            }
        }
        Some(ViewKind::SplitMenu) => {
            let i = view
                .display_rows()
                .iter()
                .position(|r| {
                    matches!(r, super::DisplayRow::Agent(a)
                        if a.pane_id.is_none() && a.attach_id.is_some() && !a.exited)
                })
                .ok_or("no paneless bg rows to open a split menu on")?;
            view.open_row_menu(i, Anchor::Center);
        }
        Some(ViewKind::TabMenu) => {
            let tid = view
                .layout
                .squads
                .iter()
                .flat_map(|s| s.tabs.iter())
                .next()
                .map(|t| t.id)
                .ok_or("no tabs to open a tab menu on")?;
            view.open_tab_menu_by_id(tid, Anchor::Center);
        }
        Some(ViewKind::SidelineMenu) => view.open_sideline_menu(Anchor::Center),
        Some(ViewKind::NewWorkspace) => {
            view.open_create();
            view.create = Some("fno-web".to_string());
        }
        Some(ViewKind::WorkspaceMenu) => {
            let i = view
                .display_rows()
                .iter()
                .position(|r| matches!(r, super::DisplayRow::Sel(row) if row.tab.is_none()))
                .ok_or("no workspace row to open a section menu on")?;
            view.open_row_menu(i, Anchor::Center);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn dock_shot(view: &mut View) -> Result<(), ShotError> {
    let (i, name) = view
        .display_rows()
        .iter()
        .enumerate()
        .find_map(|(i, r)| match r {
            super::DisplayRow::Agent(a) => Some((i, a.name.clone())),
            _ => None,
        })
        .ok_or("no agent rows to dock a reply under")?;
    let seq = view.open_peek(i, name);
    if let Some(p) = view.peek.as_mut().filter(|p| p.seq == seq) {
        p.body = Some(
            [
                "  you: can you take the checkout retry next?",
                "  agent: yes, reading the queue worker first.",
                "  agent: the retry loop swallows the 429, fixing that now.",
                "  you: thanks, ping me when the PR is up.",
            ]
            .map(String::from)
            .to_vec(),
        );
    }
    // The dock needs a sideline at least MIN_DOCK_W wide; the demo
    // root's default is narrower, so the peek would center instead.
    view.sideline_width = view.sideline_width.max(48);
    view.peek_docked = true;
    view.peek_input = Some((String::new(), String::from("on it")));
    Ok(())
}

/// The settings, keys, update and decisions modals. False when the view is
/// not one of them.
/// The snapshot seam word each Config-tab view maps to.
fn config_seam_word(kind: &ViewKind) -> &'static str {
    match kind {
        ViewKind::SettingsConfig => "list",
        ViewKind::SettingsConfigFilter => "filter",
        ViewKind::SettingsConfigToggle => "toggle",
        ViewKind::SettingsConfigCycle => "cycle",
        ViewKind::SettingsConfigPage => "page",
        ViewKind::SettingsConfigDetail => "detail",
        ViewKind::SettingsConfigError => "error",
        ViewKind::SettingsConfigPaths => "paths",
        other => unreachable!("not a Config-tab snapshot view: {other:?}"),
    }
}

fn stage_modal(view: &mut View, kind: Option<&ViewKind>) -> Result<bool, ShotError> {
    match kind {
        Some(ViewKind::Settings) => {
            view.lane.reset();
            theme_import_ui::reset(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(
            kind @ (ViewKind::SettingsConfig
            | ViewKind::SettingsConfigFilter
            | ViewKind::SettingsConfigToggle
            | ViewKind::SettingsConfigCycle
            | ViewKind::SettingsConfigPage
            | ViewKind::SettingsConfigDetail
            | ViewKind::SettingsConfigError
            | ViewKind::SettingsConfigPaths),
        ) => {
            view.settings_tab = super::settings_modal::SettingsTab::Config;
            let seam_sel = super::config_page::snapshot_seam(view, config_seam_word(kind));
            view.aux = Some(view.build_settings_modal());
            if let (Some(sel), Some(modal)) = (seam_sel, view.aux.as_mut()) {
                // The staged row sits at the list's end: scroll to the
                // bottom so what it opened shows under it.
                modal.popup.sel = sel;
                modal.popup.scroll = usize::MAX;
            }
            // The pinned doc line reads the selection through the OPEN modal
            // (the live feed rebuilds it the same way); this first build
            // saw no aux, so one rebuild with the modal attached pins it.
            view.reopen_settings_keeping_sel();
            view.aux_esc.clear();
        }
        Some(ViewKind::RemoteHosts) => {
            super::remote_hosts::snapshot_seam(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
            // The page paints rows only when settings is open on the tab;
            // the live page re-opens itself on every probe, so nothing else
            // to gather here.
        }
        Some(ViewKind::RemoteHostsAdd) => {
            super::remote_hosts::snapshot_seam(view);
            super::remote_hosts::open_add(view);
        }
        Some(ViewKind::RemoteHostsRemove) => {
            super::remote_hosts::snapshot_seam_armed(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(ViewKind::RemoteHostsEdit) => {
            super::remote_hosts::snapshot_seam_edit(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(ViewKind::RemoteHostsDup) => {
            super::remote_hosts::snapshot_seam_dup(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(kind @ (ViewKind::Mods | ViewKind::ModsGallery | ViewKind::ModsConfirm)) => {
            super::mods::snapshot_seam(
                view,
                matches!(kind, ViewKind::ModsGallery),
                matches!(kind, ViewKind::ModsConfirm),
            );
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(ViewKind::Keys) => {
            reinstall_keymap();
            view.open_keys_modal();
        }
        Some(ViewKind::KeysTags) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            crate::keys::reinstall(crate::digest_overlay::keymap(&cwd).0);
            view.keys_modal = Some(super::keys_modal::keys_modal_with_filter(Some("split")));
        }
        Some(ViewKind::Sweep) => {
            view.aux = Some(super::sweep_scope::build_sweep_modal(
                &super::sweep_scope::SweepCounts {
                    tabs: 6,
                    used: 4,
                    dead: 3,
                    squads: 2,
                    stopped: 41,
                    ..Default::default()
                },
            ));
            view.aux_esc.clear();
        }
        Some(kind @ (ViewKind::UpdateWheel | ViewKind::UpdateBinary)) => update_shot(view, kind),
        Some(ViewKind::UpdateUnsupported) => update_unsupported_shot(view),
        Some(ViewKind::ServerSkew) => {
            // The pair the 2026-11-03 repro named: the client spoke v121 then,
            // so the gate refused and the operator had no in-mux restart.
            view.aux = Some(super::server_skew::build_skew_popup(
                Some(120),
                "open a row from another machine",
            ));
            view.aux_esc.clear();
        }
        Some(ViewKind::Decisions) => decisions_shot(view)?,
        Some(ViewKind::LawBook) => law_book_shot(view)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn update_shot(view: &mut View, kind: &ViewKind) {
    use super::release_check::{Channel, ReleaseNotesSection, ReleaseOutcome};
    use super::update_menu::{UpdateOutcome, UpdateProbe, UpdateReadiness};
    let (channel, installed) = match kind {
        ViewKind::UpdateWheel => (Channel::Uv, "0.4.5"),
        _ => (Channel::Binary, "0.4.6"),
    };
    let probe = UpdateProbe {
                readiness: UpdateOutcome::Ok(UpdateReadiness {
                    update_ready: false,
                    installed_rev: None,
                    source_rev: None,
                    installed_version: Some(installed.into()),
                    source_prs_ahead: None,
                    changelog: vec![],
                    release_notes: None,
                    guidance: "no source checkout and no installed-rev marker; a release install refreshes with `fno update`".into(),
                    degraded: None,
                    running: vec![],
                    running_stale: 0,
                    source_pin: None,
                    revisions: None,
                }),
                release: ReleaseOutcome::Newer {
                    channel,
                    installed: installed.into(),
                    latest: "0.4.7".into(),
                    notes: vec![ReleaseNotesSection {
                        area: "update".into(),
                        bullets: vec![
                            "fno update moves a wheel install to ~/.fno/install/bin".into(),
                            "release tarballs ship a .sha256".into(),
                        ],
                    }],
                },
            };
    view.aux = Some(super::update_menu::build_update_modal(Some(&probe)));
    view.aux_esc.clear();
}

fn update_unsupported_shot(view: &mut View) {
    use super::release_check::{Channel, ReleaseOutcome};
    use super::update_menu::{UpdateOutcome, UpdateProbe, UpdateReadiness};
    let probe = UpdateProbe {
                readiness: UpdateOutcome::Ok(UpdateReadiness {
                    update_ready: false,
                    installed_rev: None,
                    source_rev: None,
                    installed_version: Some("0.4.4".into()),
                    source_prs_ahead: None,
                    changelog: vec![],
                    release_notes: None,
                    guidance: "no source checkout and no installed-rev marker; a release install refreshes with `fno update`".into(),
                    degraded: None,
                    running: vec![],
                    running_stale: 0,
                    source_pin: None,
                    revisions: None,
                }),
                release: ReleaseOutcome::Unsupported {
                    channel: Channel::Uv,
                    installed: "0.4.4".into(),
                    floor: "0.4.5".into(),
                },
            };
    view.update_outcome = Some(probe.clone());
    view.aux = Some(super::update_menu::build_update_modal(Some(&probe)));
    view.aux_esc.clear();
}

fn decisions_shot(view: &mut View) -> Result<(), ShotError> {
    let node = "x-aaaa";
    let rows = serde_json::from_value(serde_json::json!([
                {"decision_id":"d-1","decision":"Meter usage in whole units","subject":node,"subject_node":node,"subject_pr":"","subject_text":"","decided_by":"ada","authority_source":"operator","quote":"count whole units only","ts":"2026-11-03T09:00:00Z","lifecycle":"live","lane":"user","audience":"customer:solo-founder,customer:vibe-coder","job":["solo-founder/ship-first-feature"],"question":"Round partial units up?"},
                {"decision_id":"d-2","decision":"Hold the build until the queue drains","subject":"example/app#118","subject_node":"","subject_pr":"#118","subject_text":"","decided_by":"7c1e9a02-1111-4222-8333-444455556666","decided_by_name":"bo","authority_source":"role","ts":"2026-11-02T15:30:00Z","lifecycle":"live","lane":"agent"},
                {"decision_id":"d-3","decision":"Retry the sync after the rate limit","subject":"sync","subject_node":"","subject_pr":"","subject_text":"sync","ts":"2026-10-30T11:00:00Z","lifecycle":"expired","lane":"unattributed"},
                {"decision_id":"d-4","decision":"Read tide data from one source","subject":"tide-source","subject_node":"","subject_pr":"","subject_text":"tide-source","ts":"2026-10-28T08:15:00Z","lifecycle":"live","lane":"unattributed"}
            ]))
            .map_err(|e| e.to_string())?;
    crate::client::decisions_view::open_rows(view, rows);
    Ok(())
}

fn law_book_shot(view: &mut View) -> Result<(), ShotError> {
    // Fixed rows, the Decisions arm's convention: a snapshot is a
    // demo frame, and a throwaway server seeds no decisions. The
    // live tab reads the real listing through open()/adopt.
    let rows = serde_json::from_value(serde_json::json!([
                {"decision_id":"d-10","decision":"A merge guard stops a PR only when it does not solve the problems its plan set out, or when the plan's own gates cannot run at the head the PR pins.","subject":"merge-guard-purpose","subject_node":"","subject_pr":"","subject_text":"merge-guard-purpose","decided_by":"ada","authority_source":"operator","ts":"2026-11-04T09:00:00Z","lifecycle":"live","lane":"user"},
                {"decision_id":"d-11","decision":"A look approval rolls forward to every later head of the PR until merge. Only a substantive paint change re-arms the look.","subject":"look-rolls-forward","subject_node":"","subject_pr":"","subject_text":"look-rolls-forward","decided_by":"ada","authority_source":"operator","ts":"2026-11-06T10:00:00Z","lifecycle":"unscoped","lane":"agent"},
                {"decision_id":"d-12","decision":"The public launch is v0.5.0. v0.4.7 seeds the fresh public repo and nothing else.","subject":"public-launch-version","subject_node":"","subject_pr":"","subject_text":"public-launch-version","decided_by":"ada","authority_source":"operator","ts":"2026-11-05T12:00:00Z","lifecycle":"live","lane":"user"},
                {"decision_id":"d-13","decision":"Codex workers start from the repo root, not a worktree. A finished target worker is removed, and new build work gets a fresh spawn through the gate.","subject":"codex-worker-start","subject_node":"","subject_pr":"","subject_text":"codex-worker-start","decided_by":"ada","authority_source":"operator","ts":"2026-11-07T08:00:00Z","lifecycle":"live","lane":"user"},
                {"decision_id":"d-14","decision":"Keep the ask short. One question, one decision word.","subject":"ask-length","subject_node":"","subject_pr":"","subject_text":"ask-length","decided_by":"ada","authority_source":"operator","ts":"2026-11-03T08:00:00Z","lifecycle":"live","lane":"user"},
                {"decision_id":"d-15","decision":"The zai lanes hold sixteen workers; the machine caps live sessions at twenty-three.","subject":"zai-lanes-cap","subject_node":"","subject_pr":"","subject_text":"zai-lanes-cap","decided_by":"ada","authority_source":"operator","ts":"2026-11-02T08:00:00Z","lifecycle":"live","lane":"user"},
                {"decision_id":"d-16","decision":"The older merge-guard wording, superseded by the purpose ruling.","subject":"merge-guard-purpose","subject_node":"","subject_pr":"","subject_text":"merge-guard-purpose","decided_by":"ada","authority_source":"operator","ts":"2026-11-01T09:00:00Z","lifecycle":"superseded","lane":"user"}
            ]))
            .map_err(|e| e.to_string())?;
    crate::client::decisions_view::open_rows(view, rows);
    if let Some(b) = view.decisions_board.as_mut() {
        b.book = true;
    }
    Ok(())
}

/// The review, navigator, org-HQ and composer views: everything the earlier
/// groups did not stage.
fn stage_board(view: &mut View, kind: Option<&ViewKind>) -> Result<(), ShotError> {
    match kind {
        Some(ViewKind::ReviewDiff) => {
            View::open(view);
            let review = review_fixture()?;
            view.backlog_board
                .as_mut()
                .ok_or("the backlog board did not open")?
                .review = Some(review);
        }
        Some(ViewKind::ReviewPanel | ViewKind::ReviewPanelDrag) => {
            review_fixture()?;
            split_fixture(view, kind);
        }
        Some(ViewKind::NavigatorPr) => {
            let pr = view.layout.agents.iter().find_map(|a| a.pr).ok_or(
                "no agent row carries a PR; shoot a root whose claimed node has a \
pr_number (scripts/ops/mux-demo-snapshot.sh seeds one)",
            )?;
            open_navigator(view, pr.to_string())?;
        }
        Some(ViewKind::NavigatorModel) => open_navigator(view, "opus".into())?,
        Some(ViewKind::NavigatorMenu) => {
            open_navigator(view, String::new())?;
            let at = view
                .nav_filtered(view.nav.as_ref().ok_or("navigator did not open")?)
                .iter()
                .position(|r| matches!(r.menu, super::row_menu::NavMenu::Agent(_)))
                .ok_or("no agent rows to open a navigator menu on")?;
            if let Some(n) = view.nav.as_mut() {
                n.cursor = at;
            }
            view.nav_open_menu();
        }
        Some(ViewKind::HqEmpty | ViewKind::HqComposer) => hq_shot(view, kind)?,
        Some(
            ViewKind::Composer
            | ViewKind::ComposerGhost
            | ViewKind::ComposerRefused
            | ViewKind::ComposerChip(_)
            | ViewKind::ComposerPicker
            | ViewKind::ComposerComplete
            | ViewKind::ComposerMode
            | ViewKind::ComposerApproval,
        ) => composer_shot(view, kind),
        // The messages seam above already opened and positioned the board.
        Some(ViewKind::MessagesTree)
        | Some(ViewKind::MessagesChats)
        | Some(ViewKind::MessagesThread)
        | Some(ViewKind::MessagesSystem)
        | Some(ViewKind::MessagesArchive)
        | Some(ViewKind::MessagesYou)
        | Some(ViewKind::MessagesYouThread)
        | Some(ViewKind::MessagesAsks)
        | Some(ViewKind::MessagesAsksTopics)
        | None => {}
        // Every other view was staged by an earlier group.
        _ => {}
    }
    Ok(())
}

fn hq_shot(view: &mut View, kind: Option<&ViewKind>) -> Result<(), ShotError> {
    super::org_board::open(view);
    view.board_full = true;
    view.sideline_width = view.sideline_width.max(60);
    let b = view.org_board.as_mut().ok_or("org view did not open")?;
    b.fleet = Some(super::org_seats::Fleet {
        machines: Vec::new(),
        notice: None,
        seat_error: None,
        measured_at: chrono::Utc::now().timestamp().max(0) as u64,
    });
    if matches!(kind, Some(ViewKind::HqComposer)) {
        view.launcher_catalog = Some(super::agent_launcher::CatalogOutcome::Ok(
            vec![super::agent_launcher::HarnessChoice {
                name: "claude".into(),
                native: true,
                installed: true,
                models: Vec::new(),
                more: Vec::new(),
                catalog_error: None,
                models_error: None,
                efforts: Some(Vec::new()),
                permission_modes: Some(Vec::new()),
                launch_flags: None,
            }],
            None,
            Vec::new(),
        ));
        super::org_board::open_composer(view, "");
        if let Some(l) = view.launcher.as_mut() {
            super::agent_launcher::sync_harness_names(l, &view.launcher_catalog);
            l.draft.message = "write the retry test for the spawn door".to_string();
            l.draft.cursor_chars = l.draft.message.chars().count();
        }
    }
    Ok(())
}

fn composer_shot(view: &mut View, kind: Option<&ViewKind>) {
    // The card is judged at the operator's real sideline width.
    view.sideline_width = view.sideline_width.max(60);
    super::agent_launcher::open(view);
    // A sideline too narrow for the card refuses the way the live
    // key does: nothing opens and the bottom row says why.
    if view.panel_w() == 0 {
        view.launcher = None;
        view.set_notice("terminal too narrow for the composer".to_string());
    }
    if let Some(l) = view.launcher.as_mut() {
        l.draft.message = "fix the mux composer cursor and paste".to_string();
        l.draft.cursor_chars = if matches!(
            kind,
            Some(ViewKind::ComposerChip(_) | ViewKind::ComposerComplete)
        ) {
            l.draft.message.chars().count()
        } else {
            "fix the mux composer ".chars().count()
        };
        if matches!(kind, Some(ViewKind::ComposerRefused)) {
            l.phase = super::agent_launcher::Phase::Refused {
                request_id: 1,
                reason: "the spawn gate is closed for this harness".to_string(),
            };
        }
    }
    match kind {
        Some(ViewKind::ComposerChip(n)) => {
            super::agent_launcher::focus_chip_for_snapshot(view, *n, 0);
        }
        Some(ViewKind::ComposerPicker) => {
            super::agent_launcher::focus_chip_for_snapshot(view, 3, 1);
        }
        Some(ViewKind::ComposerComplete) => {
            super::agent_launcher::focus_chip_for_snapshot(view, 0, 4);
        }
        Some(ViewKind::ComposerMode) => {
            super::agent_launcher::focus_chip_for_snapshot(view, 0, 2);
        }
        Some(ViewKind::ComposerApproval) => {
            super::agent_launcher::focus_chip_for_snapshot(view, 0, 3);
        }
        _ => {}
    }
    if matches!(kind, Some(ViewKind::ComposerGhost)) {
        super::agent_launcher::close(view);
    }
}

/// The key layer a live client in this cwd would run, so a shot of the keys
/// list or the footer names the configured prefix.
fn reinstall_keymap() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::keys::reinstall(crate::digest_overlay::keymap(&cwd).0);
}

/// Attach a second sizing client at `dims` and keep it attached until the
/// returned half drops. Its stream is drained so the server never stalls on it.
async fn hold_peer(
    socket: &Path,
    cwd: String,
    dims: (u16, u16),
) -> Result<tokio::net::unix::OwnedWriteHalf, String> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("peer cannot connect to {}: {e}", socket.display()))?;
    let (mut reader, mut writer) = stream.into_split();
    let attach = ClientMsg::Attach {
        proto: PROTO_VERSION,
        build: BUILD_VERSION.to_string(),
        rows: dims.0,
        cols: dims.1,
        cwd,
    };
    proto::write_msg(&mut writer, &attach)
        .await
        .map_err(|e| format!("peer attach write failed: {e}"))?;
    loop {
        let read = proto::read_msg::<_, ServerMsg>(&mut reader);
        match tokio::time::timeout(Duration::from_secs(10), read).await {
            Ok(Ok(ServerMsg::Layout { .. })) => break,
            Ok(Ok(ServerMsg::Bye { reason })) => {
                return Err(format!("server refused the peer attach: {reason}"))
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("peer read failed: {e}")),
            Err(_) => return Err("peer saw no layout within 10s".into()),
        }
    }
    tokio::spawn(
        async move { while proto::read_msg::<_, ServerMsg>(&mut reader).await.is_ok() {} },
    );
    Ok(writer)
}

/// `dims` set attaches as a sizing client at that content area and reads
/// for a settle window, so panes can redraw at the new size.
async fn observe(
    socket: &Path,
    cwd: String,
    dims: Option<(u16, u16)>,
    cmds: &[proto::Command],
) -> Result<Observed, String> {
    let stream = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| format!("connect to {} timed out", socket.display()))?
    .map_err(|e| {
        format!(
            "cannot connect to {}: {e}; list servers with `fno mux ls`",
            socket.display()
        )
    })?;
    let (mut reader, mut writer) = stream.into_split();
    proto::write_msg(
        &mut writer,
        &ClientMsg::Attach {
            proto: PROTO_VERSION,
            build: BUILD_VERSION.to_string(),
            // (0, 0) is the observer sentinel: it never resizes a PTY.
            rows: dims.map_or(0, |d| d.0),
            cols: dims.map_or(0, |d| d.1),
            cwd,
        },
    )
    .await
    .map_err(|e| format!("attach write failed: {e}"))?;
    // Commands ride the same attach, before the read loop: the server
    // applies them in order and the read collects the layout they
    // produced.
    for cmd in cmds {
        proto::write_msg(&mut writer, &ClientMsg::Command(cmd.clone()))
            .await
            .map_err(|e| format!("command write failed: {e}"))?;
    }
    // Read-only from here: release the write half without the half-close
    // that would make the server detach us.
    writer.forget();

    let mut layout = None;
    let mut frames = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let settle = dims.map(|_| Instant::now() + Duration::from_millis(2500));
    loop {
        // Once the layout is in, wait only briefly for its panes' frames.
        let wait = match (&layout, settle) {
            (Some(_), Some(t)) => t.saturating_duration_since(Instant::now()),
            (None, _) => deadline.saturating_duration_since(Instant::now()),
            (Some(_), None) => Duration::from_millis(400),
        };
        let msg =
            match tokio::time::timeout(wait, proto::read_msg::<_, ServerMsg>(&mut reader)).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => return Err(format!("read failed: {e}")),
                Err(_) if layout.is_some() => break,
                Err(_) => return Err("server sent no layout within 10s".into()),
            };
        match msg {
            ServerMsg::Layout {
                squads,
                active_squad,
                panes,
                focus,
                area,
                agents,
                focus_node,
                ..
            } => {
                layout = Some(LayoutView {
                    squads,
                    active_squad,
                    panes,
                    focus,
                    area,
                    agents,
                    focus_node,
                })
            }
            ServerMsg::Frame { pane_id, frame } if frame.geometry_ok() => {
                frames.insert(pane_id, frame);
            }
            ServerMsg::Bye { reason } => {
                return Err(format!("server refused the attach: {reason}"))
            }
            _ => {}
        }
        if let (Some(l), None) = (&layout, settle) {
            if l.panes.iter().all(|(id, _)| frames.contains_key(id)) {
                break;
            }
        }
    }
    let layout = layout.expect("loop exits with a layout");
    frames.retain(|id, _| layout.panes.iter().any(|(p, _)| p == id));
    Ok(Observed { layout, frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_extra(extra: &[&str]) -> Result<SnapshotArgs, String> {
        let mut tail: Vec<OsString> = ["--snapshot", "--server", "demo", "--out", "/tmp/s.png"]
            .iter()
            .map(OsString::from)
            .collect();
        tail.extend(extra.iter().map(OsString::from));
        parse(&tail)
    }

    #[test]
    fn view_flag_names_every_kind_and_refuses_the_rest() {
        for (name, kind) in [
            ("bell", ViewKind::Bell),
            ("bell-wide", ViewKind::BellWide),
            ("bell-full", ViewKind::BellFull),
            ("feed", ViewKind::Feed),
            ("row-menu", ViewKind::RowMenu),
            ("hover", ViewKind::Hover),
            ("cursor", ViewKind::Cursor),
            ("needs-input", ViewKind::NeedsInput),
            ("needs-input-pulse-on", ViewKind::NeedsInputPulseOn),
            ("needs-input-pulse-off", ViewKind::NeedsInputPulseOff),
            ("tab-menu", ViewKind::TabMenu),
            ("sideline-menu", ViewKind::SidelineMenu),
            ("new-workspace", ViewKind::NewWorkspace),
            ("workspace-menu", ViewKind::WorkspaceMenu),
            ("composer", ViewKind::Composer),
            ("composer-ghost", ViewKind::ComposerGhost),
            ("composer-refused", ViewKind::ComposerRefused),
            ("split-menu", ViewKind::SplitMenu),
            ("settings", ViewKind::Settings),
            ("settings-config", ViewKind::SettingsConfig),
            ("settings-config-filter", ViewKind::SettingsConfigFilter),
            ("settings-config-toggle", ViewKind::SettingsConfigToggle),
            ("settings-config-cycle", ViewKind::SettingsConfigCycle),
            ("settings-config-page", ViewKind::SettingsConfigPage),
            ("settings-config-detail", ViewKind::SettingsConfigDetail),
            ("settings-config-error", ViewKind::SettingsConfigError),
            ("settings-config-paths", ViewKind::SettingsConfigPaths),
            ("remote-hosts", ViewKind::RemoteHosts),
            ("remote-hosts-add", ViewKind::RemoteHostsAdd),
            ("remote-hosts-remove", ViewKind::RemoteHostsRemove),
            ("remote-hosts-edit", ViewKind::RemoteHostsEdit),
            ("remote-hosts-dup", ViewKind::RemoteHostsDup),
            ("mods", ViewKind::Mods),
            ("mods-gallery", ViewKind::ModsGallery),
            ("mods-confirm", ViewKind::ModsConfirm),
            ("report", ViewKind::Report),
            ("report-preview", ViewKind::ReportPreview),
            ("keys", ViewKind::Keys),
            ("keys-tags", ViewKind::KeysTags),
            ("org-block", ViewKind::OrgBlock),
            ("org-table", ViewKind::OrgTable),
            ("org-recent", ViewKind::OrgRecent),
            ("org-overview", ViewKind::OrgOverview),
            ("org-graph", ViewKind::OrgGraph),
            ("org-health", ViewKind::OrgHealth),
            ("org-seat", ViewKind::OrgSeat),
            ("org-feed", ViewKind::OrgFeed),
            ("org-system", ViewKind::OrgSystem),
            ("org-remote", ViewKind::OrgRemote),
            ("org-peek", ViewKind::OrgPeek),
            ("org-spawn", ViewKind::OrgSpawn),
            ("org-keys", ViewKind::OrgKeys),
            ("messages-tree", ViewKind::MessagesTree),
            ("messages-chats", ViewKind::MessagesChats),
            ("messages-thread", ViewKind::MessagesThread),
            ("messages-system", ViewKind::MessagesSystem),
            ("messages-archive", ViewKind::MessagesArchive),
            ("messages-you", ViewKind::MessagesYou),
            ("messages-you-thread", ViewKind::MessagesYouThread),
            ("messages-asks", ViewKind::MessagesAsks),
            ("messages-asks-topics", ViewKind::MessagesAsksTopics),
            ("review-diff", ViewKind::ReviewDiff),
            ("review-panel", ViewKind::ReviewPanel),
            ("review-panel-drag", ViewKind::ReviewPanelDrag),
            ("backlog-list", ViewKind::BacklogList),
            ("backlog-search", ViewKind::BacklogSearch),
            ("backlog-kanban", ViewKind::BacklogKanban),
            ("backlog-graph", ViewKind::BacklogGraph),
            ("navigator-pr", ViewKind::NavigatorPr),
            ("navigator-model", ViewKind::NavigatorModel),
            ("navigator-menu", ViewKind::NavigatorMenu),
            ("feed-detail", ViewKind::FeedDetail),
            ("question-detail", ViewKind::QuestionDetail),
            ("persona-detail", ViewKind::PersonaDetail),
            ("persona-question", ViewKind::PersonaQuestion),
            ("persona-decision", ViewKind::PersonaDecision),
            ("bell-personas", ViewKind::BellPersonas),
            ("sweep", ViewKind::Sweep),
            ("update-wheel", ViewKind::UpdateWheel),
            ("update-binary", ViewKind::UpdateBinary),
            ("update-unsupported", ViewKind::UpdateUnsupported),
            ("server-skew", ViewKind::ServerSkew),
            ("connections", ViewKind::Connections),
            ("connections-use", ViewKind::ConnectionsUse),
            ("connections-add", ViewKind::ConnectionsAdd),
            ("connections-add-search", ViewKind::ConnectionsAddSearch),
            ("connections-add-local", ViewKind::ConnectionsAddLocal),
            ("needs", ViewKind::Needs),
            ("dock", ViewKind::Dock),
            ("decisions", ViewKind::Decisions),
            ("law-book", ViewKind::LawBook),
            ("bell-audit", ViewKind::BellAudit),
            ("bell-decisions", ViewKind::BellDecisions),
            ("bell-audit-tab", ViewKind::BellAuditTab),
            ("machines", ViewKind::Machines),
        ] {
            let args = parse_extra(&["--view", name]).expect(name);
            assert_eq!(args.view, Some(kind), "{name}");
        }
        // --peer holds a second window; a malformed size is a usage error.
        assert_eq!(
            parse_extra(&["--peer", "120x40"]).unwrap().peer,
            Some((40, 120))
        );
        let e = parse_extra(&["--peer", "wide"]).expect_err("malformed peer");
        assert!(e.contains("--peer wants <cols>x<rows>"), "{e}");
        let e = parse_extra(&["--view", "nonesuch"]).expect_err("unknown view names the options");
        assert!(
                    e.contains(
                        "bell, bell-wide, bell-full, feed, row-menu, tab-menu, sideline-menu, new-workspace, workspace-menu, composer, composer-ghost, composer-refused, composer-tab-N, composer-picker, composer-mode, composer-approval, split-menu, settings, settings-config, settings-config-filter, settings-config-toggle, settings-config-cycle, settings-config-page, settings-config-detail, settings-config-error, settings-config-paths, keys, org-block, org-table, org-recent, org-overview, org-graph, org-health, org-seat, org-feed, org-system, org-remote, org-peek, org-spawn, org-keys, messages-tree, messages-chats, messages-thread, messages-system, messages-archive, messages-you, messages-you-thread, messages-asks, messages-asks-topics, law-book, backlog-list, backlog-search, backlog-kanban, backlog-graph, review-diff, review-panel, review-panel-drag, navigator-pr, navigator-menu, navigator-model, feed-detail, question-detail, connections-use or connections"
                    ),
                    "{e}"
                );
    }
}
