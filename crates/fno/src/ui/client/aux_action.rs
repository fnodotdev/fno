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

/// Run one aux-popup action (US4/US5). Menu entries open a surface or
/// detach; settings toggles flip a session-local view flag and rebuild the modal
/// so its glyph reflects the new state (the popup stays open for another toggle).
pub(super) async fn execute_aux_action(
    view: &mut View,
    action: AuxAction,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<DispatchFlow, String> {
    match action {
        AuxAction::OpenKeybinds => {
            view.aux = None;
            view.open_keys_modal();
        }
        // A FRESH settings open resets the colors drill, so a stale lane
        // text-entry (left armed by a mouse dismiss mid-typing) can never
        // capture the keys of a reopened modal. `reopen_settings_keeping_sel`
        // deliberately keeps it: it rebuilds the SAME view after an action.
        AuxAction::OpenSettings => {
            view.lane.reset();
            theme_import_ui::reset(view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        AuxAction::OpenUpdate => {
            view.aux = Some(build_update_modal(view.update_outcome.as_ref()));
            view.aux_esc.clear();
        }
        AuxAction::OpenPr(url) => update_menu::open_pr(view, url).await,
        AuxAction::OpenSweep => {
            view.aux = None;
            if view.sweep_inflight {
                view.set_notice("a sweep is already running".into());
            } else {
                view.sweep_action = Some(SweepAction::Counts);
            }
        }
        AuxAction::SweepTabs => begin_sweep_apply(view, SweepScope::Tabs),
        AuxAction::SweepUsedShells => begin_sweep_apply(view, SweepScope::UsedShells),
        AuxAction::SweepDeadAgents => begin_sweep_apply(view, SweepScope::Dead),
        AuxAction::SweepBoth => begin_sweep_apply(view, SweepScope::Both),
        AuxAction::UpgradeRelease(c) => {
            if update_menu::update_tap(view) {
                view.update_command_want = Some(c);
            }
        }
        AuxAction::RestartAgents => {
            // Never queues: the restart kills this server; the tap detaches.
            if update_menu::restart_tap(view) {
                return Ok(DispatchFlow::Detach);
            }
        }
        AuxAction::SweepNamed => begin_sweep_apply(view, SweepScope::Named),
        AuxAction::SweepSquads => begin_sweep_apply(view, SweepScope::Squads),
        AuxAction::SweepStopped => begin_sweep_apply(view, SweepScope::Stopped),
        AuxAction::OpenConnections => {
            // close the MENU and open the Connections modal in its
            // loading state; arm the first read (the run loop spawns it).
            view.aux = None;
            view.open_connections();
        }
        AuxAction::OpenAgentLauncher => {
            // close the MENU and open the new-agent composer;
            // the catalog probe is kicked on first open (run loop spawns it).
            view.aux = None;
            agent_launcher::open(view);
        }
        AuxAction::Detach => {
            view.aux = None;
            return Ok(DispatchFlow::Detach);
        }
        AuxAction::ToggleGroupByLead => {
            view.aux = None;
            view.toggle_group_by_lead();
        }
        AuxAction::LiftHold => hold::lift(view).await,
        AuxAction::ToggleExperimental(flag) => experimental::toggle(view, flag),
        AuxAction::Share(act) => share_flow::act(view, act).await,
        AuxAction::OpenBacklogView => {
            view.aux = None;
            View::open(view);
        }
        AuxAction::ToggleHoverFocus
        | AuxAction::ToggleStatus
        | AuxAction::ToggleConfirmLifecycle
        | AuxAction::ToggleResourceMeter
        | AuxAction::ToggleReserveAuto
        | AuxAction::ToggleLaunchScreen
        | AuxAction::EditReserveApps
        | AuxAction::ToggleSidelineLayout
        | AuxAction::ApplyTheme(_)
        | AuxAction::RemoteHost(_)
        | AuxAction::Mod(_)
        | AuxAction::ThemeImportOpen
        | AuxAction::ThemeImportSave
        | AuxAction::ThemeImportCancel
        | AuxAction::ThemePick
        | AuxAction::SettingsBack
        | AuxAction::LaneColorEdit(..)
        | AuxAction::LaneColorAdd(_)
        | AuxAction::LaneColorCustom(..)
        | AuxAction::LaneColorSet(..) => settings_modal::run_action(view, action, sock_w).await?,
    }
    Ok(DispatchFlow::Continue)
}

/// Aux-popup keys (US4/US5): arrows select (scrolling to keep the selection
/// visible), pgup/pgdn scroll, Enter runs, Esc/`q`/any unbound key dismiss (the
/// shared popup contract, codex P2); a detach entry propagates StdinFlow::Detach.
/// Esc is carried across reads so a split arrow never leaks into a pane.
pub(super) async fn aux_keys(
    view: &mut View,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    // An open settings text field or key capture consumes the chunk.
    if let Some(flow) = settings_modal::field_keys(view, bytes, sock_w).await? {
        return Ok(flow);
    }
    let trows = view.term.0 as usize;
    let mut esc = std::mem::take(&mut view.aux_esc);
    let toks = fold_modal_keys(&mut esc, bytes);
    view.aux_esc = esc;
    for tok in toks {
        if view.aux.is_none() {
            break;
        }
        match tok {
            ModalKey::Esc => {
                if !settings_modal::back(view) {
                    theme_import_ui::reset(view);
                    view.aux = None;
                }
            }
            ModalKey::Up => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Up);
                    m.popup.follow_sel(view.term);
                }
            }
            ModalKey::Down => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Down);
                    m.popup.follow_sel(view.term);
                }
            }
            ModalKey::Left => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Left);
                }
            }
            ModalKey::Right => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Right);
                }
            }
            // No split cells on a settings page either: the folded
            // Shift+arrow is inert, never a dismissal.
            ModalKey::ShiftArrow(_) => {}
            ModalKey::PageUp => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.scroll_by(-(trows as isize - 2).max(1));
                    m.popup.clamp_sel_to_view(view.term);
                }
            }
            ModalKey::PageDown => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.scroll_by((trows as isize - 2).max(1));
                    m.popup.clamp_sel_to_view(view.term);
                }
            }
            ModalKey::Enter => {
                if matches!(
                    aux_execute_selected(view, sock_w).await?,
                    DispatchFlow::Detach
                ) {
                    return Ok(StdinFlow::Detach);
                }
            }
            // Tab switches the settings modal's section. Other aux
            // popups have no tab strip, so Tab dismisses as every unbound key does.
            ModalKey::Byte(b'\t') => {
                let has_tabs = view
                    .aux
                    .as_ref()
                    .map(|m| !m.popup.chrome.tabs.is_empty())
                    .unwrap_or(false);
                if has_tabs {
                    settings_modal::switch_tab(view, view.settings_tab.next()).await;
                } else {
                    theme_import_ui::reset(view);
                    view.aux = None;
                }
            }
            // Space flips a settings checkbox in place, as Enter does.
            ModalKey::Byte(b' ') if settings_modal::space_toggle(view).is_some() => {
                aux_execute_selected(view, sock_w).await?;
            }
            // Any other (unbound) key dismisses, per the shared popup contract.
            ModalKey::Byte(_) => {
                theme_import_ui::reset(view);
                view.aux = None;
            }
        }
    }
    Ok(StdinFlow::Continue)
}
