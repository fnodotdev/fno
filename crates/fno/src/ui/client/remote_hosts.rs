//! Remote hosts: the ssh names the user told the mux about (`mux.remote_hosts`),
//! the one probe that says whether each answers, and the settings page that
//! lists them beside the machines the fleet map already knows. Sessions still
//! come only from the fleet map; a host here that never beats the shared store
//! reads "no sessions here" instead of vanishing.

use std::time::Duration;

use super::input_field::InputField;
use super::machine_tree::MachineTree;
use super::org_board::{OrgMsg, OrgTx};
use door::{beat, fold_host, host_identity, parse_host, probe, HostState};

#[path = "remote_hosts/door.rs"]
pub mod door;
use super::*;

const OWN_HOST_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_INPUT: usize = 256;
const CONFIG_KEY: &str = "mux.remote_hosts";

/// The add field's placeholder: the generic forms, marked as an example.
/// An earlier example read `name.tailnet.ts.net`, and a user typed
/// `tailnet` as their literal domain; the tailnet name now rides a hint
/// line, and only when this machine answers on one.
const HOST_PLACEHOLDER: &str = "e.g. host, 192.168.1.20 or user@host";

/// The one suggestion line under the inline input: this machine's real
/// tailnet suffix around a `<machine>` template, so the hint cannot be
/// mistyped as one literal whole name.
fn tailnet_hint(own: &str) -> String {
    let suffix = own.split_once('.').map_or(own, |(_, s)| s);
    format!("tailnet names work too, e.g. <machine>.{suffix}; https:// too")
}

#[derive(Debug, Clone)]
struct Host {
    name: String,
    state: HostState,
    started: bool,
}

/// A row action on the Remote hosts page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    /// Enter on a host row: a map-only band joins the list; a configured
    /// host opens its inline edit on the row.
    Pick(String),
    /// The confirm row under an armed host: remove it now.
    Remove(String),
}

/// One host's inline edit: its row renders as the input, prefilled with
/// the stored address; Enter saves, Esc cancels.
#[derive(Debug, Clone)]
pub(super) struct EditHost {
    pub(super) original: String,
    pub(super) field: InputField,
}

#[derive(Debug, Default)]
pub(super) struct RemoteHosts {
    hosts: Vec<Host>,
    /// The Add field, open while the user types a host.
    pub(super) add: Option<InputField>,
    /// The host being edited in place; its row renders as the input.
    pub(super) edit: Option<EditHost>,
    /// The configured host `d` named, awaiting a second press to remove.
    armed: Option<String>,
    /// This machine's own tailnet DNS name, when `tailscale status`
    /// answered: the add field's example becomes a name that resolves.
    own_host: Option<String>,
    /// The one own-host lookup; never respawned after it lands or fails.
    own_host_asked: bool,
    /// The refused submit's reason, rendered inline under the field until
    /// the next Enter or the field closes.
    refusal: Option<String>,
}

/// One line of the page: a configured host or a machine only the map knows.
struct Line {
    name: String,
    word: String,
    detail: String,
    configured: bool,
}

/// The door core (the address forms above are now [`door`]):
/// parse, fold, probe and the state words are one leg; the page below is
/// view only.

impl RemoteHosts {
    /// The hosts in the merged config, each waiting for its first probe.
    pub(super) fn load() -> Self {
        let config = crate::config_cli::merged_config(&crate::config_cli::repo_root());
        let names = crate::config_cli::raw_get(&config, CONFIG_KEY)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        Self::from_names(names)
    }

    fn from_names(names: Vec<String>) -> Self {
        let hosts = names
            .into_iter()
            .map(|name| Host {
                name,
                state: HostState::Checking,
                started: false,
            })
            .collect();
        Self {
            hosts,
            ..Self::default()
        }
    }

    /// Hosts whose probe already answered: the snapshot seam and the tests.
    fn with_states(states: Vec<(&str, HostState)>) -> Self {
        let hosts = states
            .into_iter()
            .map(|(name, state)| Host {
                name: name.into(),
                state,
                started: true,
            })
            .collect();
        Self {
            hosts,
            ..Self::default()
        }
    }

    fn names(&self) -> Vec<String> {
        self.hosts.iter().map(|h| h.name.clone()).collect()
    }

    /// The fleet map's destinations plus every configured host whose probe
    /// answered, each machine once. A ported or bracketed name probes here
    /// but never becomes a launch target: the launch door reads a bare
    /// name, so forwarding it would refuse every launch on that machine.
    pub(super) fn destinations(&self, mut map: Vec<String>) -> Vec<String> {
        for host in &self.hosts {
            if host.name.contains([':', '[']) {
                continue;
            }
            let known = {
                let names: Vec<&str> = map.iter().map(String::as_str).collect();
                fold_host(&host.name, &names).is_some()
            };
            if !known && matches!(host.state, HostState::Reachable(_)) {
                map.push(host.name.clone());
            }
        }
        map
    }

    fn lines(&self, tree: &MachineTree) -> Vec<Line> {
        let far: Vec<_> = tree.far_bands().collect();
        let far_names: Vec<&str> = far.iter().map(|(h, ..)| *h).collect();
        let mut lines: Vec<Line> = self
            .hosts
            .iter()
            .map(|host| {
                let band = fold_host(&host.name, &far_names).and_then(|name| {
                    far.iter()
                        .find(|(h, ..)| *h == name)
                        .map(|&(_, fresh, age_s)| (fresh, age_s))
                });
                let (word, detail) = door::state_word(band, &host.state);
                Line {
                    name: host.name.clone(),
                    word,
                    detail,
                    configured: true,
                }
            })
            .collect();
        for (host, fresh, age_s) in far {
            if !self
                .hosts
                .iter()
                .any(|h| fold_host(&h.name, &[host]).is_some())
            {
                let (word, _) = beat(fresh, age_s);
                lines.push(Line {
                    name: host.to_string(),
                    word,
                    detail: "on the shared store; not in mux.remote_hosts".into(),
                    configured: false,
                });
            }
        }
        lines
    }
}

/// The settings tab's rows: the add field when it is open, else one row per
/// host and the add row.
pub(super) fn rows(view: &View) -> (Vec<PopupRow>, Vec<AuxAction>) {
    page_rows(&view.remote_hosts, &view.machine_tree)
}

/// The page's rows from its parts: the seam the module's tests read.
fn page_rows(rh: &RemoteHosts, tree: &MachineTree) -> (Vec<PopupRow>, Vec<AuxAction>) {
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let inert = |label: &str| PopupRow::Entry {
        glyph: " ".into(),
        label: label.into(),
        hint: String::new(),
        enabled: false,
    };
    // One page, no drill-in: the list, then the inline input at the bottom.
    // Armed, it carries the cursor and the typing; unarmed it rests on the
    // dimmed placeholder. A refused submit and the tailnet suggestion sit
    // inline under it.
    rows.push(PopupRow::Header("remote hosts".into()));
    let lines = rh.lines(tree);
    if lines.is_empty() {
        rows.push(inert("no other machine yet; type a name below"));
    }
    for line in lines {
        // The host being edited renders its row as the input, prefilled
        // with the stored address; its refusal sits inline beneath. The
        // label reads `host`, the add row's word, so the row never repeats
        // the address it is editing.
        if let Some(edit) = rh.edit.as_ref().filter(|e| e.original == line.name) {
            rows.push(edit.field.row("host"));
            if let Some(reason) = &rh.refusal {
                rows.push(PopupRow::Error(reason.clone()));
            }
            continue;
        }
        // A band only the map knows is a pick: `+` says Enter adds it.
        let glyph = if !line.configured {
            "+"
        } else if line.word == "live" {
            "●"
        } else {
            "○"
        };
        // The status rides the hint slot, the row's dim right column, so
        // it says why it reads as it does before any key touches it.
        rows.push(PopupRow::Entry {
            glyph: glyph.into(),
            label: format!("{} · {}", line.name, line.word),
            hint: line.detail.clone(),
            enabled: true,
        });
        actions.push(AuxAction::RemoteHost(Act::Pick(line.name.clone())));
        // The armed host's confirm row: the removal state sits ON the page,
        // where the d-d-esc contract is readable, not only in a notice.
        if rh.armed.as_deref() == Some(line.name.as_str()) {
            rows.push(PopupRow::Entry {
                glyph: "✕".into(),
                label: format!("d again removes {} · esc cancels", line.name),
                hint: String::new(),
                enabled: true,
            });
            actions.push(AuxAction::RemoteHost(Act::Remove(line.name.clone())));
        }
    }
    // One input on the page at a time: the edit above suppresses the add
    // field's row until it closes.
    if rh.edit.is_none() {
        let input = match &rh.add {
            Some(field) => field.row("host"),
            None => PopupRow::Input {
                label: "host".into(),
                text: String::new(),
                cursor: 0,
                placeholder: HOST_PLACEHOLDER.into(),
            },
        };
        rows.push(input);
        if let Some(reason) = &rh.refusal {
            rows.push(PopupRow::Error(reason.clone()));
        }
    }
    if let Some(own) = rh.own_host.as_deref() {
        rows.push(PopupRow::Text(tailnet_hint(own)));
    }
    (rows, actions)
}

/// Deep-link into settings on the Remote hosts tab with the inline input
/// focused: the sideline `+ host` button and its key.
pub(super) fn open_add(view: &mut View) {
    view.lane.reset();
    theme_import_ui::reset(view);
    disarm(view);
    view.settings_tab = SettingsTab::RemoteHosts;
    view.remote_hosts.refusal = None;
    view.remote_hosts.edit = None;
    view.remote_hosts.add = Some(InputField::new(HOST_PLACEHOLDER, MAX_INPUT));
    view.aux = Some(view.build_settings_modal());
    view.aux_esc.clear();
}

/// Type-to-focus: on the Remote hosts tab an unarmed inline input arms
/// itself on the first printable chunk, so typing starts the form without
/// a drill-in. An escape sequence or an empty flush never arms it.
pub(super) fn arm_on_typing(view: &mut View, bytes: &[u8]) -> bool {
    if view.remote_hosts.add.is_some()
        || view.remote_hosts.edit.is_some()
        || view.settings_tab != SettingsTab::RemoteHosts
        || bytes.first() == Some(&0x1b)
        || (bytes == b"d" && remove_target(view).is_some())
        || !bytes.iter().any(|b| b.is_ascii_graphic() || *b == b' ')
    {
        return false;
    }
    view.remote_hosts.add = Some(InputField::new(HOST_PLACEHOLDER, MAX_INPUT));
    true
}

/// Close the add field. True when one was open.
pub(super) fn cancel_add(view: &mut View) -> bool {
    view.remote_hosts.add.take().is_some()
}

/// Close the inline edit. True when one was open.
pub(super) fn cancel_edit(view: &mut View) -> bool {
    let closed = view.remote_hosts.edit.take().is_some();
    if closed {
        view.remote_hosts.refusal = None;
    }
    closed
}

/// Clear the armed removal. True when one was armed. Esc, a tab change,
/// or a fresh add cancels the confirm.
pub(super) fn disarm(view: &mut View) -> bool {
    view.remote_hosts.armed.take().is_some()
}

/// Enter on a configured host row: the row becomes the inline input,
/// prefilled with the stored address. No drill-in view; the edit lives on
/// the list, where the row reads now.
pub(super) fn open_edit(view: &mut View, name: &str) {
    view.remote_hosts.add = None;
    view.remote_hosts.refusal = None;
    view.remote_hosts.armed = None;
    view.remote_hosts.edit = Some(EditHost {
        original: name.to_string(),
        field: InputField::new(HOST_PLACEHOLDER, MAX_INPUT).with_text(name),
    });
}

/// Enter in a host's inline edit: parse, refuse a name another row or
/// band already lists, then rename in place. A re-spelling of the same
/// machine (user@, case, brackets, port aside) keeps its probe state; a
/// different address re-probes. A refusal keeps the field open.
pub(super) async fn submit_edit(view: &mut View, original: &str, input: String) -> bool {
    let name = match parse_host(&input) {
        Ok(name) => name,
        Err(reason) => {
            view.remote_hosts.refusal = Some(reason);
            return true;
        }
    };
    // The duplicate check runs on the resolved identity and skips the row
    // being edited: its own name, and any band that folds to it, are this
    // host, not a collision.
    let others: Vec<&str> = view
        .remote_hosts
        .hosts
        .iter()
        .map(|h| h.name.as_str())
        .filter(|n| *n != original)
        .collect();
    if let Some(row) = fold_host(&name, &others) {
        view.remote_hosts.refusal = Some(format!("{name} is already listed here as {row}"));
        return true;
    }
    let bands: Vec<String> = view
        .machine_tree
        .far_bands()
        .map(|(h, ..)| h.to_string())
        .collect();
    let band_refs: Vec<&str> = bands.iter().map(String::as_str).collect();
    let own_band = fold_host(original, &band_refs);
    let foreign_bands: Vec<&str> = band_refs
        .into_iter()
        .filter(|b| Some(*b) != own_band)
        .collect();
    if let Some(row) = fold_host(&name, &foreign_bands) {
        view.remote_hosts.refusal = Some(format!("{name} is already on the shared store as {row}"));
        return true;
    }
    rename_host(view, original, name).await;
    true
}

/// Save an edit: swap the stored name, keep the probe state when the
/// identity did not move, and re-point an armed confirm at the new name.
async fn rename_host(view: &mut View, original: &str, name: String) {
    let same_machine = host_identity(&name) == host_identity(original);
    if let Some(h) = view
        .remote_hosts
        .hosts
        .iter_mut()
        .find(|h| h.name == original)
    {
        h.name = name.clone();
        if !same_machine {
            h.state = HostState::Checking;
            h.started = false;
        }
    }
    view.remote_hosts.edit = None;
    view.remote_hosts.refusal = None;
    if view.remote_hosts.armed.as_deref() == Some(original) {
        view.remote_hosts.armed = Some(name.clone());
    }
    let saved = save(view).await;
    view.set_notice(match saved {
        Ok(()) => format!("{name}: saved"),
        Err(_) => format!("{name}: saved this session; save failed"),
    });
}

/// The configured host the selected row names, when `d` removes it: no
/// field open, and the selection on a host row or its confirm row. A
/// map-only band is not removable here (it is not configured).
pub(super) fn remove_target(view: &View) -> Option<String> {
    if view.settings_tab != SettingsTab::RemoteHosts
        || view.remote_hosts.add.is_some()
        || view.remote_hosts.edit.is_some()
    {
        return None;
    }
    let m = view.aux.as_ref()?;
    remove_target_in(m.popup.sel, &m.actions, &view.remote_hosts)
}

/// [`remove_target`]'s parts, so the tests and the footer hint read it
/// without a `View`.
pub(super) fn remove_target_in(
    sel: usize,
    actions: &[AuxAction],
    rh: &RemoteHosts,
) -> Option<String> {
    match actions.get(sel)? {
        AuxAction::RemoteHost(act) => match act {
            Act::Pick(name) if rh.hosts.iter().any(|h| &h.name == name) => Some(name.clone()),
            Act::Remove(name) => Some(name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The `d` chunk on the Remote hosts page: the first press arms removal
/// on the selected host row (the confirm row states the contract), the
/// second removes. Arming re-probes an unreachable host, the way the old
/// Enter pick did. Consumed only for a bare `d` naming a removable row;
/// false lets the byte arm the add field or dismiss the modal, as before.
pub(super) async fn remove_key_press(view: &mut View, bytes: &[u8]) -> bool {
    if bytes != b"d" {
        return false;
    }
    let Some(name) = remove_target(view) else {
        return false;
    };
    if view.remote_hosts.armed.as_deref() == Some(name.as_str()) {
        remove_host(view, &name).await;
    } else {
        view.remote_hosts.refusal = None;
        if let Some(h) = view.remote_hosts.hosts.iter_mut().find(|h| h.name == name) {
            if matches!(h.state, HostState::Unreachable(_)) {
                h.state = HostState::Checking;
                h.started = false;
            }
        }
        view.remote_hosts.armed = Some(name);
    }
    view.reopen_settings_keeping_sel();
    true
}

/// Enter in the add field: parse, refuse a name the page already lists,
/// then save, probe, and turn the machine layer on. True ends the chunk.
/// A refusal keeps the field open on what the user typed.
pub(super) async fn submit(view: &mut View, input: String) -> bool {
    let name = match parse_host(&input) {
        Ok(name) => name,
        Err(reason) => {
            view.remote_hosts.refusal = Some(reason);
            return true;
        }
    };
    // The duplicate check runs on the resolved identity: user@ and case do
    // not count, and a short name answers to the one machine it names. The
    // refusal names the row the page already shows.
    let configured: Vec<&str> = view
        .remote_hosts
        .hosts
        .iter()
        .map(|h| h.name.as_str())
        .collect();
    if let Some(row) = fold_host(&name, &configured) {
        view.remote_hosts.refusal = Some(format!("{name} is already listed here as {row}"));
        return true;
    }
    let bands: Vec<String> = view
        .machine_tree
        .far_bands()
        .map(|(h, ..)| h.to_string())
        .collect();
    let band_refs: Vec<&str> = bands.iter().map(String::as_str).collect();
    if let Some(row) = fold_host(&name, &band_refs) {
        view.remote_hosts.refusal = Some(format!("{name} is already on the shared store as {row}"));
        return true;
    }
    add_name(view, name).await;
    true
}

/// Add one resolved name (typed into the field, or picked off the page's
/// live band rows), persist, probe, and turn the machine layer on. The
/// caller owns the duplicate posture.
async fn add_name(view: &mut View, name: String) {
    view.remote_hosts.add = None;
    view.remote_hosts.edit = None;
    view.remote_hosts.refusal = None;
    view.remote_hosts.armed = None;
    view.remote_hosts.hosts.push(Host {
        name: name.clone(),
        state: HostState::Checking,
        started: false,
    });
    let saved = save(view).await;
    if !view.machine_tree.on {
        super::machine_tree::set_on(view, true);
        crate::view_store::save_experimental(crate::view_store::EXPERIMENTAL_OTHER_MACHINES, true);
    }
    view.set_notice(match saved {
        Ok(()) => format!("{name}: checking"),
        Err(_) => format!("{name}: added this session; save failed"),
    });
}

async fn save(view: &View) -> Result<(), String> {
    let json = serde_json::to_string(&view.remote_hosts.names()).map_err(|e| e.to_string())?;
    config_set::spawn_config_set_global(CONFIG_KEY, &json).await
}

/// Run one row action.
pub(super) async fn act(view: &mut View, act: Act) {
    match act {
        Act::Pick(name) => {
            let lines = view.remote_hosts.lines(&view.machine_tree);
            let Some(line) = lines.iter().find(|l| l.name == name) else {
                return;
            };
            if !line.configured {
                // A pick, not a notice: the band is a machine fno already
                // knows and this page already lists; Enter adds it.
                add_name(view, name).await;
            } else {
                // Enter edits in place: the row becomes the inline input,
                // prefilled with the address. Removal rides `d`, never
                // Enter (user ruling 2026-11-16: Enter stays open/select).
                open_edit(view, &name);
            }
        }
        Act::Remove(name) => remove_host(view, &name).await,
    }
    view.reopen_settings_keeping_sel();
}

/// Remove one configured host and persist the list. A failed save keeps
/// the removal for this session only, and the notice says so.
async fn remove_host(view: &mut View, name: &str) {
    view.remote_hosts.armed = None;
    view.remote_hosts.hosts.retain(|h| h.name != name);
    let saved = save(view).await;
    view.set_notice(match saved {
        Ok(()) => format!("{name}: removed"),
        Err(_) => format!("{name}: removed this session; save failed"),
    });
}

/// The snapshot seam: two configured hosts in opposite states, so the shot
/// shows both words. The demo fixture hosts the suffix, never a real
/// machine name.
pub(super) fn snapshot_seam(view: &mut View) {
    view.remote_hosts = RemoteHosts::with_states(vec![
        ("linux1", HostState::Reachable("fno 0.4.7".into())),
        (
            "nohost",
            HostState::Unreachable("could not reach nohost: Connection refused".into()),
        ),
    ]);
    view.remote_hosts.own_host = Some("demo-host.example-net.ts.net".into());
    view.remote_hosts.own_host_asked = true;
    view.settings_tab = SettingsTab::RemoteHosts;
}

/// The duplicate-refusal seam: the add field holding the folded name that
/// the dup report described, with the refusal notice up.
pub(super) fn snapshot_seam_dup(view: &mut View) {
    view.remote_hosts =
        RemoteHosts::with_states(vec![("DemoBox", HostState::Reachable("fno 0.4.7".into()))]);
    view.remote_hosts.own_host = Some("demo-host.example-net.ts.net".into());
    view.remote_hosts.own_host_asked = true;
    view.remote_hosts.add =
        Some(InputField::new(HOST_PLACEHOLDER, MAX_INPUT).with_text("demobox.example-net.ts.net"));
    view.remote_hosts.refusal =
        Some("demobox.example-net.ts.net is already listed here as DemoBox".into());
    view.settings_tab = SettingsTab::RemoteHosts;
}

/// The remove-confirm seam: the list with one host armed, so the shot
/// shows the confirm row.
pub(super) fn snapshot_seam_armed(view: &mut View) {
    snapshot_seam(view);
    view.remote_hosts.armed = Some("linux1".into());
}

/// The edit seam: one host's row rendering as the inline input, prefilled
/// with the stored address.
pub(super) fn snapshot_seam_edit(view: &mut View) {
    snapshot_seam(view);
    view.remote_hosts.edit = Some(EditHost {
        original: "linux1".into(),
        field: InputField::new(HOST_PLACEHOLDER, MAX_INPUT).with_text("linux1"),
    });
}

/// Start one probe per host that has none in flight, off the UI loop, and
/// ask once per session for this machine's own tailnet name.
pub(super) fn kick(view: &mut View, tx: &OrgTx) {
    kick_own_host(view, tx);
    for host in view.remote_hosts.hosts.iter_mut().filter(|h| !h.started) {
        host.started = true;
        let (name, tx) = (host.name.clone(), tx.clone());
        tokio::spawn(async move {
            let state = probe(&name).await;
            if tx.send((0, OrgMsg::HostProbe(name, state))).is_err() {
                // The UI loop is gone; nothing left to report to.
            }
        });
    }
}

/// The one own-host lookup: `tailscale status --json`, bounded like a
/// probe. Its answer upgrades the add field's example to a real name.
fn kick_own_host(view: &mut View, tx: &OrgTx) {
    if view.remote_hosts.own_host_asked {
        return;
    }
    view.remote_hosts.own_host_asked = true;
    let tx = tx.clone();
    tokio::spawn(async move {
        let mut command = crate::process_admission::tokio_command("tailscale");
        command
            .args(["status", "--json"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let run = crate::process_admission::tokio_output(&mut command);
        let name = match tokio::time::timeout(OWN_HOST_TIMEOUT, run).await {
            Ok(Ok(out)) if out.status.success() => {
                crate::config::config_doctor::phone_sink::tailscale_dns_name_of(
                    &String::from_utf8_lossy(&out.stdout),
                )
            }
            _ => None,
        };
        if tx.send((0, OrgMsg::OwnHost(name))).is_err() {
            // The UI loop is gone; the answer has nowhere to land.
        }
    });
}

/// The own-host answer landed: the add view's suggestion line reads it, so
/// a page open on the tab rebuilds.
pub(super) fn set_own_host(view: &mut View, name: Option<String>) {
    view.remote_hosts.own_host = name;
    if view.aux.is_some() && view.settings_tab == SettingsTab::RemoteHosts {
        view.reopen_settings_keeping_sel();
    }
}

/// A probe landed.
pub(super) fn apply(view: &mut View, name: String, state: HostState) {
    if let Some(host) = view.remote_hosts.hosts.iter_mut().find(|h| h.name == name) {
        host.state = state;
    }
    if let Some(l) = view.launcher.as_mut() {
        l.machines = view
            .remote_hosts
            .destinations(view.machine_tree.destinations());
    }
    if view.aux.is_some() && view.settings_tab == SettingsTab::RemoteHosts {
        view.reopen_settings_keeping_sel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_hint_is_a_template_around_the_real_suffix() {
        assert!(
            tailnet_hint("demo-host.example-net.ts.net").contains("<machine>.example-net.ts.net")
        );
    }

    #[test]
    fn destinations_add_only_reachable_hosts_not_on_the_map() {
        let hosts = RemoteHosts::with_states(vec![
            ("linux1", HostState::Reachable("fno 1".into())),
            ("nohost", HostState::Unreachable("down".into())),
            ("mac", HostState::Reachable("fno 1".into())),
        ]);
        let got = hosts.destinations(vec!["mac.local".into()]);
        assert_eq!(got, vec!["mac.local".to_string(), "linux1".to_string()]);
    }

    #[test]
    fn page_lines_name_each_state() {
        let hosts = RemoteHosts::with_states(vec![
            ("linux1", HostState::Reachable("fno 1".into())),
            (
                "nohost",
                HostState::Unreachable("could not reach nohost: no route".into()),
            ),
        ]);
        let lines = hosts.lines(&MachineTree::default());
        let words: Vec<&str> = lines.iter().map(|l| l.word.as_str()).collect();
        assert_eq!(words, ["no sessions here", "couldn't connect"]);
        assert!(lines.iter().any(|l| l.detail.contains("no route")));
        // The beat words are the page's words: live, or the band's age.
        assert_eq!(beat(true, 0).0, "live");
        assert_eq!(beat(false, 360).0, "beat 6m ago");
    }

    #[test]
    fn inline_input_rows_read_their_state() {
        let mut rh =
            RemoteHosts::with_states(vec![("linux1", HostState::Reachable("fno 1".into()))]);
        rh.own_host = Some("demo-host.example-net.ts.net".into());

        // One page, no drill-in: the list, then the input at the bottom.
        // Unarmed it rests on the placeholder; armed it carries the field.
        let (rows, _) = page_rows(&rh, &MachineTree::default());
        assert!(rows.iter().any(|r| matches!(r, PopupRow::Input
            { text, placeholder, .. } if text.is_empty()
                && placeholder.contains("e.g."))));
        rh.add = Some(InputField::new(HOST_PLACEHOLDER, MAX_INPUT));
        let (rows, _) = page_rows(&rh, &MachineTree::default());
        assert!(rows.iter().any(|r| matches!(r, PopupRow::Input { .. })));
        assert!(rows.iter().any(|r| matches!(r, PopupRow::Text(t)
            if t.contains("<machine>.example-net.ts.net"))));

        // The refused submit rides inline under the input, in the error style.
        rh.refusal = Some("x is already listed here as y".into());
        let (rows, _) = page_rows(&rh, &MachineTree::default());
        assert!(rows
            .iter()
            .any(|r| matches!(r, PopupRow::Error(t) if t.contains("already listed"))));
        rh.refusal = None;

        // The armed host's confirm row sits beneath its row, wired to Remove.
        rh.armed = Some("linux1".into());
        let (rows, actions) = page_rows(&rh, &MachineTree::default());
        let confirm = rows.iter().position(|r| {
            matches!(r, PopupRow::Entry
            { glyph, label, .. } if glyph == "✕"
                && label.contains("d again removes linux1")
                && label.contains("esc cancels"))
        });
        assert!(confirm.is_some());
        assert!(actions.contains(&AuxAction::RemoteHost(Act::Remove("linux1".into()))));

        // Disarmed: no confirm row anywhere, and no back row exists at all.
        rh.armed = None;
        let (rows, actions) = page_rows(&rh, &MachineTree::default());
        assert!(rows
            .iter()
            .all(|r| !matches!(r, PopupRow::Entry { glyph, .. } if glyph == "✕")));
        assert!(rows
            .iter()
            .all(|r| !matches!(r, PopupRow::Entry { label, .. } if label == "back")));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, AuxAction::RemoteHost(Act::Remove(_)))));
    }

    #[test]
    fn host_rows_carry_their_status_in_the_hint_slot() {
        let hosts = RemoteHosts::with_states(vec![(
            "nohost",
            HostState::Unreachable("could not reach nohost: no route".into()),
        )]);
        let (rows, _) = page_rows(&hosts, &MachineTree::default());
        assert!(rows.iter().any(|r| matches!(r, PopupRow::Entry { hint, .. }
            if hint.contains("no route"))));
    }

    #[test]
    fn edit_rows_replace_their_host_row_and_block_the_add_row() {
        let mut rh = RemoteHosts::with_states(vec![
            ("linux1", HostState::Reachable("fno 1".into())),
            ("nohost", HostState::Unreachable("down".into())),
        ]);
        rh.edit = Some(EditHost {
            original: "linux1".into(),
            field: InputField::new(HOST_PLACEHOLDER, MAX_INPUT).with_text("linux1"),
        });
        let (rows, actions) = page_rows(&rh, &MachineTree::default());
        // The edited row renders as the input: no Entry, no Pick action.
        // Its label reads `host`, the add row's word, never the address.
        assert!(rows.iter().any(|r| matches!(r, PopupRow::Input
            { label, text, .. } if label == "host" && text == "linux1")));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, AuxAction::RemoteHost(Act::Pick(n)) if n == "linux1")));
        // The untouched host keeps its Entry, and the page holds one input:
        // the add row is suppressed beneath the edit.
        assert!(rows
            .iter()
            .any(|r| matches!(r, PopupRow::Entry { label, .. } if label.contains("nohost"))));
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, PopupRow::Input { .. }))
                .count(),
            1
        );
        // A refusal rides directly under the edit row, not under a bottom
        // add row that is not there.
        rh.refusal = Some("x is already listed here as y".into());
        let (rows, _) = page_rows(&rh, &MachineTree::default());
        let input_at = rows
            .iter()
            .position(|r| matches!(r, PopupRow::Input { .. }));
        let error_at = rows.iter().position(|r| matches!(r, PopupRow::Error(_)));
        assert_eq!(error_at, input_at.map(|i| i + 1));
    }

    #[test]
    fn remove_target_names_configured_rows_only() {
        let mut rh =
            RemoteHosts::with_states(vec![("linux1", HostState::Reachable("fno 1".into()))]);
        let tree = MachineTree::default();
        let acts_of = |rh: &RemoteHosts| page_rows(rh, &tree).1;

        // The host row: d removes it.
        let acts = acts_of(&rh);
        let at = acts
            .iter()
            .position(|a| matches!(a, AuxAction::RemoteHost(Act::Pick(n)) if n == "linux1"))
            .unwrap();
        assert_eq!(remove_target_in(at, &acts, &rh).as_deref(), Some("linux1"));

        // The armed host's confirm row also answers d: it removes.
        rh.armed = Some("linux1".into());
        let acts = acts_of(&rh);
        let confirm_at = acts
            .iter()
            .position(|a| matches!(a, AuxAction::RemoteHost(Act::Remove(_))))
            .unwrap();
        assert_eq!(
            remove_target_in(confirm_at, &acts, &rh).as_deref(),
            Some("linux1")
        );
    }
}
