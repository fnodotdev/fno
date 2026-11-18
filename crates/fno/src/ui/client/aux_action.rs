//! The aux-popup shell and its row actions: the sideline MENU popup and the
//! settings modal share one popup-plus-actions type whose selectable rows run
//! one enum of actions. Moved out of `client.rs` for the file budget; the
//! type's behavior is unchanged.

use super::*;

/// (US4/US5) The sideline MENU popup and the minimal settings modal share
/// this one aux-popup type: a [`Popup`] plus the [`AuxAction`] each selectable
/// row runs. The two chain (MENU -> settings) by swapping the `aux` slot.
pub(crate) struct AuxPopup {
    pub(crate) popup: Popup,
    pub(crate) actions: Vec<AuxAction>,
}

/// What a MENU / settings-modal / mini-kanban row does. Menu entries open a
/// surface or detach; settings entries change a live setting; a kanban entry
/// names a card. Not `Copy` since - a card action carries its node id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuxAction {
    OpenKeybinds,
    OpenSettings,
    OpenConnections,
    /// Open the sideline new-agent popup: one composer that
    /// launches a new harness session through the canonical spawn door.
    OpenAgentLauncher,
    /// Open the update-readiness overlay: version pair, changelog,
    /// and the one computed guidance line. Only offered by the menu when the
    /// last probe reported ready (or degraded) - see `build_sideline_menu`.
    OpenUpdate,
    /// Open one release-notes PR in the browser. The URL is Python's
    /// (Locked Decision 6); the modal rendered it as a tappable Entry only
    /// because the URL existed.
    OpenPr(String),
    /// (change 7) Queue `fno agent restart --mux` off the UI loop. Never
    /// `--force`: the modal named what survives, and the tap is the confirmation.
    RestartAgents,
    /// Queue the channel's release upgrade (`fno update`, `brew upgrade fno`).
    UpgradeRelease(release_check::Channel),
    /// Probe `mux workspace prune --dry-run` once and open the centered
    /// sweep-threads choice modal from its counts. Every scope of the prune
    /// lives behind this one entry.
    OpenSweep,
    /// Apply the prune with one scope. Each choice is the confirmation: the
    /// modal named the counts, the tap picked the half.
    SweepTabs,
    /// The opt-in used-shell half: `--tabs-only
    /// --include-used-shells`. Its own row so a tap can only ever pick the
    /// posture the modal showed.
    SweepUsedShells,
    SweepDeadAgents,
    SweepBoth,
    SweepNamed,
    SweepSquads,
    SweepStopped,
    Detach,
    ToggleHoverFocus,
    /// Flip one experimental toggle, then rebuild the open sidebar menu.
    ToggleExperimental(experimental::Flag),
    Share(share_flow::Act),
    OpenBacklogView,
    /// Nest workers under their lead in the card layout (the prefix+t toggle).
    ToggleGroupByLead,
    /// Flip the sideline's row shape (list <-> card) live: swap the view's
    /// in-memory layout, persist `sideline.layout` through the CLI, then
    /// invalidate the palette cache. A failed save keeps the in-memory shape
    /// and says so honestly, the same posture as the theme toggle.
    ToggleSidelineLayout,
    ToggleStatus,
    /// The whole-machine resource meter: flip the status-row meter, persist
    /// `resource_meter.enabled`, start or stop the sampler.
    ToggleResourceMeter,
    /// Flip `agents.reserve_auto` (the daemon's OBS / editor reserve).
    ToggleReserveAuto,
    /// Open the field that edits `agents.reserve_apps`.
    EditReserveApps,
    /// Flip `mux.launch_screen` between `agents` and `hq`.
    ToggleLaunchScreen,
    /// Clear the machine-wide fleet hold the status row names (prefix+U).
    LiftHold,
    ToggleConfirmLifecycle,
    /// Apply the named mux theme now: swap the in-memory theme, then
    /// persist via `fno config set mux.theme`. The picker lists the shipped
    /// names, so this carries one of them.
    ApplyTheme(String),
    RemoteHost(remote_hosts::Act),
    Mod(mods::Act),
    ThemeImportOpen,
    ThemeImportSave,
    ThemeImportCancel,
    /// Step the settings modal back one drill level.
    SettingsBack,
    /// Open the macOS file picker for a theme file.
    ThemePick,
    /// Open the color picker for one `[sideline.colors]` axis key
    /// (existing or just typed). The axis names its table
    /// (`harness` / `route` / `model` / `row`).
    LaneColorEdit(String, String),
    /// Open the text entry for naming a NEW key on an axis.
    LaneColorAdd(String),
    /// From the picker, open the free-form color entry (indexed(n) / #rrggbb).
    LaneColorCustom(String, String),
    /// Persist one lane color through `fno config set` block-replace, then
    /// restart-free via `reload_palette`.
    LaneColorSet(String, String, String),
}
