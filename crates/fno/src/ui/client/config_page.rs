//! The settings modal's Config tab: every schema leaf as a row of the
//! reusable settings list, values read from the effective config in process,
//! writes through the one bounded shell-out. The tab owns its state here and
//! not on `View`: `client.rs` is shrink-only past the file budget, so new
//! state lands in the module that answers the question. One mux client per
//! process reads it; the UI loop is the only writer, and no lock is held
//! across an await.
//!
//! Reads are pure file walks (`repo_root`, `merged_config`, `resolve_source`),
//! the same ladder a spawned `fno config set` resolves, so the tab never
//! calls `load_validated`: its validator notes write to stderr and its
//! retired-key sweep writes to disk, neither of which belongs to a tab open.

use super::*;
use crate::config::config_cli;
use crate::config::config_schema::{setting_specs, EditMode};
use setting_list::{Choice, ListEvent, RowMode, SettingList, SettingRow};

/// The tab's state: the list (query, page, note) over the loaded rows.
#[derive(Default)]
struct ConfigUi {
    list: SettingList,
    loaded: bool,
}

static UI: std::sync::Mutex<Option<ConfigUi>> = std::sync::Mutex::new(None);

/// Serializes the Config-tab tests: the state above is process-global, and
/// cargo runs tests in parallel threads.
#[cfg(test)]
static TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn test_gate() -> std::sync::MutexGuard<'static, ()> {
    match TEST_GATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Run `f` on the state, treating a poisoned lock as recoverable: the state
/// is repaint-only, so a panic mid-rebuild leaves no invariant worth dying
/// for.
fn with_ui<T>(f: impl FnOnce(&mut ConfigUi) -> T) -> T {
    let mut guard = match UI.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(guard.get_or_insert_with(ConfigUi::default))
}

/// The tab's rows. The selected row's doc no longer paints in the list:
/// it answers through [`pinned`], which the modal pins above its footer.
pub(crate) fn rows() -> Vec<PopupRow> {
    with_ui(|ui| {
        if !ui.loaded {
            return vec![PopupRow::Text("reading config...".into())];
        }
        ui.list.popup_rows()
    })
}

/// The doc line the modal pins above its footer: the selected row's help
/// text (or the selected folded child's, inside the group). `None` leaves
/// the popup unpinned; the width-measure pass passes `sel: None` and its
/// selected row has no doc, so it pins nothing either.
pub(crate) fn pinned(sel: Option<usize>) -> Option<String> {
    with_ui(|ui| {
        if !ui.loaded {
            return None;
        }
        ui.list.selected_detail(sel)
    })
}

/// The tab footer while a page or query is live; `None` keeps the modal's
/// standard footer.
pub(crate) fn footer() -> Option<&'static str> {
    with_ui(|ui| {
        if ui.loaded {
            Some(ui.list.footer())
        } else {
            None
        }
    })
}

/// Leave the tab: drop the query, page and note. The next open reloads.
pub(crate) fn reset() {
    *UI.lock().expect("config ui lock") = None;
}

/// The snapshot seam: load the rows and stage one named state, the same
/// states the live keys reach. The list events a preset folds are dropped;
/// only the painted state they leave behind matters to a frame.
pub(crate) fn snapshot_seam(view: &mut View, kind: &str) {
    let rows = compute_rows();
    // The folded Paths row's selectable index, taken before `rows` moves
    // into the list: the "paths" frame opens the group with one Enter.
    let paths_idx = rows
        .iter()
        .take_while(|r| r.id != "paths")
        .filter(|r| !matches!(r.mode, RowMode::Locked { .. }))
        .count();
    let mut list = SettingList::new(rows);
    let query = match kind {
        "filter" => "notify_on_done",
        "toggle" => "hover_focus",
        "cycle" => "sideline.layout",
        "page" => "autonomy.mode",
        "detail" => "mux.restore.policy",
        "error" => "hover_focus",
        _ => "",
    };
    if !query.is_empty() {
        for c in query.chars() {
            list.feed(c.to_string().as_bytes(), None);
        }
    }
    match kind {
        "page" | "detail" => {
            list.feed(b"\r", Some(0));
        }
        "paths" => {
            // Enter on the folded Paths row opens the group.
            list.feed(b"\r", Some(paths_idx));
        }
        "error" => {
            // The refusal paint a failed write leaves under its row, staged
            // without a real failed write.
            list.set_note(
                "mux.hover_focus",
                "fno config set mux.hover_focus banana failed".into(),
            );
        }
        "toggle" => {
            let mut rows = compute_rows();
            for row in rows.iter_mut() {
                if row.id == "mux.hover_focus" {
                    row.value = "off".into();
                    row.display = "off".into();
                    row.mode = RowMode::Toggle { on: false };
                }
            }
            list.reload(rows);
            view.set_notice("focus follows mouse: off".into());
        }
        "cycle" => {
            let mut rows = compute_rows();
            for row in rows.iter_mut() {
                if row.id == "sideline.layout" {
                    row.value = "list".into();
                    row.display = "list".into();
                    row.mode = RowMode::Cycle {
                        options: vec!["card".into(), "list".into()],
                        current: 1,
                    };
                }
            }
            list.reload(rows);
            view.set_notice("sideline layout: list".into());
        }
        _ => {}
    }
    with_ui(|ui| {
        ui.list = list;
        ui.loaded = true;
    });
}

/// Load the rows once per open. Inline (not spawn_blocking): the walk is a
/// few dozen small file reads over a warm page cache, and the modal paints a
/// `reading config...` line until it lands.
pub(crate) async fn ensure_loaded() {
    let need = with_ui(|ui| !ui.loaded);
    if !need {
        return;
    }
    let rows = compute_rows();
    with_ui(|ui| {
        if !ui.loaded {
            ui.list = SettingList::new(rows);
            ui.loaded = true;
        }
    });
}

/// Every leaf of the committed schema as a row: the effective value from the
/// merged config (falling back to the schema default), the mode from the
/// leaf's `edit`, and `Locked` when a decider outranks the mux's write
/// target.
fn compute_rows() -> Vec<SettingRow> {
    // A YAML pin splits reads and writes: the pin is the only read
    // candidate, and `config set` lands every write in a file the reads
    // never open (the pin is not a toml, so it is not the global target
    // either). Every row refuses, naming the pin.
    if let Some(pin) = std::env::var_os("FNO_CONFIG")
        .map(PathBuf::from)
        .filter(|p| !p.is_empty())
        .filter(|p| p.extension().and_then(|e| e.to_str()) != Some("toml"))
    {
        let reason = format!(
            "FNO_CONFIG pins reads to {}, which outranks the mux; edit there",
            pin.display()
        );
        return pinned_rows(reason);
    }
    let root = config_cli::repo_root();
    let merged = config_cli::merged_config(&root);
    let ladder: Vec<PathBuf> = config_cli::candidate_paths(&root);
    let resolved_ladder: Vec<PathBuf> =
        ladder.iter().map(|p| config_cli::resolve_path(p)).collect();
    let local_override = ladder
        .first()
        .and_then(|p| p.parent())
        .map(|dir| config_cli::resolve_path(&dir.join("config.local.toml")));
    let target = write_target(&root);
    let target_pos = resolved_ladder
        .iter()
        .position(|p| *p == config_cli::resolve_path(&target));
    let rows: Vec<SettingRow> = setting_specs()
        .into_iter()
        .map(|spec| {
            let live = config_cli::raw_get(&merged, &spec.key).cloned();
            let effective = live.unwrap_or_else(|| spec.default.clone());
            let mut mode = match spec.edit {
                EditMode::Toggle => RowMode::Toggle {
                    on: effective.as_bool().unwrap_or(false),
                },
                EditMode::Cycle => RowMode::Cycle {
                    current: 0,
                    options: spec.choices.iter().map(|c| c.value.clone()).collect(),
                },
                EditMode::Page => RowMode::Page {
                    current: 0,
                    options: spec
                        .choices
                        .iter()
                        .map(|c| Choice {
                            value: c.value.clone(),
                            doc: c.doc.clone(),
                        })
                        .collect(),
                },
                EditMode::Editor => RowMode::Editor,
            };
            let decider = config_cli::resolve_source(&spec.key, &root);
            if let Some((dec, _)) = &decider {
                let dec = config_cli::resolve_path(dec);
                let above = local_override.as_ref().map_or(false, |p| *p == dec)
                    || target_pos
                        .zip(resolved_ladder.iter().position(|p| *p == dec))
                        .is_some_and(|(t, d)| d < t);
                if above {
                    mode = RowMode::Locked {
                        reason: format!(
                            "set in {}, which outranks the mux; edit there",
                            dec.display()
                        ),
                    };
                }
            }
            align_current(&mut mode, &effective);
            let raw = value_text(&effective);
            SettingRow {
                id: spec.key.clone(),
                label: human_label(&spec.key),
                value: raw.clone(),
                display: row_display(&spec.key, &raw, &spec.default),
                detail: spec.doc,
                mode,
            }
        })
        .collect();
    // Fold the paths.* leaves into one group row: twenty path roots crowd
    // the top level (the user's look on the frames asked for one collapsed
    // "Paths" row; State Dir stays top level, it is the state_dir leaf).
    fold_paths(rows)
}

/// The painted value for one row: the generic unset paint, or the path
/// shapes. An unset `paths.*` leaf paints its resolved default sketch, dim,
/// relative to the state dir or its class home (`<state>/db/history/ledger.json`);
/// a set one paints the shortened path; State Dir paints the shortened raw
/// path (`~` for home, middle-truncated when long).
fn row_display(key: &str, raw: &str, default: &serde_json::Value) -> String {
    if key == "state_dir" {
        return shorten_path(raw);
    }
    if key.starts_with("paths.") {
        if raw.is_empty() {
            return path_default_display(key).unwrap_or_else(|| "default".into());
        }
        return shorten_path(raw);
    }
    unset_default(raw, default)
}

/// Fold the `paths.*` leaves into one group row (the user's look on the
/// frames asked for one collapsed "Paths" row). The summary names the count
/// and the override state ("20 · all default", "20 · 2 overridden"). An
/// override is a child with a non-empty value: an unset child paints its
/// default sketch, never an empty cell.
fn fold_paths(rows: Vec<SettingRow>) -> Vec<SettingRow> {
    let (mut children, rest): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|r| r.id.starts_with("paths."));
    if children.is_empty() {
        return rest;
    }
    let overridden = children.iter().filter(|c| !c.value.is_empty()).count();
    let summary = if overridden == 0 {
        "all default".to_string()
    } else {
        format!("{overridden} overridden")
    };
    let count = children.len();
    children.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rest = rest;
    rest.push(SettingRow {
        id: "paths".into(),
        label: "Paths".into(),
        value: String::new(),
        display: format!("{count} · {summary}"),
        detail: "One row per path root; Enter opens the group.".into(),
        mode: RowMode::Group { rows: children },
    });
    rest
}

/// The dim default sketch an unset `paths.*` row paints, relative to the
/// state dir or its class home. Display knowledge only: the resolvers in
/// fno-agent's `paths_cli.rs` stay the authority (the mux cannot link that
/// crate), and `docs/path-config.md` is the prose source. A key missing
/// here paints the generic "default" word instead.
fn path_default_display(key: &str) -> Option<String> {
    let sketch = |s: &str| format!("<state>/{s}");
    match key {
        "paths.ledger_json" => Some(sketch("db/history/ledger.json")),
        "paths.evals_history" => Some(sketch("db/history/evals-history.jsonl")),
        "paths.briefs_dir" => Some(sketch("briefs/")),
        "paths.fleet_dir" => Some(sketch("cache/fleet/")),
        "paths.postmortems_dir" => Some(sketch("db/postmortems/")),
        "paths.worktrees_base" => Some(sketch("spaces/worktrees/")),
        "paths.cargo_targets_base" => Some(sketch("cache/cargo-build/")),
        "paths.cache_dir" => Some(if cfg!(target_os = "macos") {
            "~/Library/Caches/fno".to_string()
        } else {
            "~/.cache/fno".to_string()
        }),
        "paths.data_dir" => Some("~/.local/share/fno".to_string()),
        "paths.state_dir_local" => Some("~/.local/state/fno".to_string()),
        "paths.memory_dir" => Some(sketch("memory/")),
        "paths.hook_logs_dir" => Some(sketch("hook-logs/")),
        "paths.inbox_path" => Some("<repo>/internal/fno/backlog/parking-lot.md".to_string()),
        "paths.handoffs_dir" => {
            Some("<vault>/internal/{project}/handoffs/ else <state>/db/handoffs/{project}/".into())
        }
        "paths.retro_pending_dir" => Some(sketch("state/retro-pending/")),
        "paths.bus_dir" => Some(sketch("db/mail/bus/")),
        "paths.chats" => Some(sketch("db/mail/chats/")),
        "paths.observer_reports_dir" => Some(sketch("observer-reports/{owner}/")),
        "paths.operator_lane" => Some(sketch("my-priorities.md")),
        "paths.spaces_dir" => Some(sketch("spaces/")),
        _ => None,
    }
}

/// A path as the right column paints it: `$HOME` becomes `~`, and a value
/// longer than the cap gives up its middle (the head anchors it, the tail
/// names the leaf, the middle is the bulk a reader skips). The plain-body
/// painter clips the right column at the modal edge anyway, which keeps
/// only the head; this keeps both ends.
fn shorten_path(raw: &str) -> String {
    let tilde = match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            let home = home.to_string_lossy().to_string();
            raw.strip_prefix(home.as_str())
                .map(|rest| format!("~{rest}"))
                .unwrap_or_else(|| raw.to_string())
        }
        _ => raw.to_string(),
    };
    const CAP: usize = 28;
    const HEAD: usize = 16;
    const TAIL: usize = 8;
    let n = tilde.chars().count();
    if n <= CAP {
        return tilde;
    }
    let chars: Vec<char> = tilde.chars().collect();
    format!(
        "{}\u{2026}{}",
        chars[..HEAD].iter().collect::<String>(),
        chars[n - TAIL..].iter().collect::<String>()
    )
}

/// The rows a YAML pin leaves behind: every key refuses, naming the pin,
/// because the pin is the only read candidate and `config set` writes
/// somewhere the reads never open.
fn pinned_rows(reason: String) -> Vec<SettingRow> {
    setting_specs()
        .into_iter()
        .map(|spec| {
            let raw = value_text(&spec.default);
            SettingRow {
                id: spec.key.clone(),
                label: human_label(&spec.key),
                value: raw.clone(),
                display: unset_default(&raw, &spec.default),
                detail: spec.doc,
                mode: RowMode::Locked {
                    reason: reason.clone(),
                },
            }
        })
        .collect()
}

/// Point a cycle or page row's `current` at the effective value, so the
/// next Enter steps from where the config actually is.
fn align_current(mode: &mut RowMode, effective: &serde_json::Value) {
    let word = effective.as_str().unwrap_or_default().to_string();
    let pos = match mode {
        RowMode::Cycle { options, .. } => options.iter().position(|o| *o == word),
        RowMode::Page { options, .. } => options.iter().position(|o| o.value == word),
        _ => None,
    };
    if let (Some(pos), RowMode::Cycle { current, .. } | RowMode::Page { current, .. }) = (pos, mode)
    {
        *current = pos;
    }
}

/// The painted value for an override row: an empty value over a null
/// default paints `default`, the state the key is in, instead of a blank
/// cell. The true text stays in `value`, so an editor row seeds empty.
fn unset_default(raw: &str, default: &serde_json::Value) -> String {
    if raw.is_empty() && default.is_null() {
        "default".to_string()
    } else {
        raw.to_string()
    }
}

/// A key's last segment as title-cased words: `mux.hover_focus` paints
/// `Hover Focus`. The dotted key stays on the row as its dim line, and the
/// filter still matches it.
fn human_label(key: &str) -> String {
    let seg = key.rsplit('.').next().unwrap_or(key);
    let mut out = String::new();
    for word in seg.split('_').filter(|w| !w.is_empty()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            out.push(' ');
        }
    }
    if out.is_empty() {
        return key.to_string();
    }
    out.pop();
    out
}

/// The file `fno config set` (as the mux shells it) would write: the
/// `FNO_CONFIG` pin when it is a toml (the pin is then the only candidate),
/// otherwise the project file at the repo root.
fn write_target(root: &Path) -> PathBuf {
    match std::env::var_os("FNO_CONFIG") {
        Some(pin) if !pin.is_empty() => {
            let pin = PathBuf::from(pin);
            if pin.extension().and_then(|e| e.to_str()) == Some("toml") {
                return pin;
            }
        }
        _ => {}
    }
    root.join(".fno").join("config.toml")
}

/// One value as the row paints it: booleans as on/off, strings bare,
/// arrays compact JSON (valid TOML as-is), objects as TOML inline tables,
/// so the text `config set` parses straight back.
fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(true) => "on".into(),
        serde_json::Value::Bool(false) => "off".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) => toml_inline(value),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A value as one TOML inline-table-shaped line: `{ k = "v", n = 1 }`.
/// Anything TOML cannot spell (a bare null) stays as JSON's word, and the
/// write refuses through the row.
fn toml_inline(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k} = {}", toml_inline(v)))
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Fold one raw chunk into the list. Every chunk is consumed: the Config tab
/// owns the keyboard while it is open, so an unbound key never dismisses.
pub(crate) async fn feed(
    view: &mut View,
    bytes: &[u8],
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Option<StdinFlow> {
    ensure_loaded().await;
    let sel = view.aux.as_ref().map(|m| m.popup.sel);
    let events = with_ui(|ui| ui.list.feed(bytes, sel));
    for event in events {
        match event {
            ListEvent::None => {}
            ListEvent::Up => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Up);
                    m.popup.follow_sel(view.term);
                }
            }
            ListEvent::Down => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.nav(NavDir::Down);
                    m.popup.follow_sel(view.term);
                }
            }
            ListEvent::Tab => {
                settings_modal::switch_tab(view, view.settings_tab.next()).await;
                return Some(StdinFlow::Continue);
            }
            ListEvent::Close => {
                theme_import_ui::reset(view);
                view.aux = None;
                return Some(StdinFlow::Continue);
            }
            ListEvent::Select(idx) => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.sel = idx;
                    m.popup.follow_sel(view.term);
                }
            }
            ListEvent::Set { id, value } => {
                set_key(view, &id, &value, sock_w).await;
            }
            ListEvent::Unset { id } => {
                unset_key(&id).await;
            }
            ListEvent::Edit { id, seed } => {
                edit_value(view, &id, &seed, sock_w).await;
            }
        }
    }
    if view.aux.is_some() {
        view.reopen_settings_keeping_sel();
    }
    Some(StdinFlow::Continue)
}

/// Write one key through the one door, routing a latched key through its
/// live handler first so General and Config never disagree.
async fn set_key(
    view: &mut View,
    id: &str,
    value: &str,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) {
    if route_live(view, id, value, sock_w).await {
        reload_rows();
        return;
    }
    record_write(id, write_value(id, value).await);
    reload_rows();
}

/// Clear one key through the same door (`fno config unset`, project scope).
async fn unset_key(id: &str) {
    record_write(id, config_set::spawn_scoped(id, None, true).await.err());
    reload_rows();
}

/// The one write door: `fno config set`, project scope, except the global
/// host list, which belongs to the user file.
async fn write_value(id: &str, value: &str) -> Option<String> {
    let result = if id == "mux.remote_hosts" {
        config_set::spawn_config_set_global(id, value).await
    } else {
        config_set::spawn_config_set(id, value).await
    };
    result.err()
}

/// Land a write's outcome on its row: a refusal paints under the row and the
/// row keeps its old value (the reload below re-reads it), a success clears
/// any stale refusal.
fn record_write(id: &str, failure: Option<String>) {
    with_ui(|ui| match failure {
        Some(message) => ui.list.set_note(id, message),
        None => ui.list.clear_note(),
    });
}

/// Re-read every row after a write, so a coerced value repaints truthfully.
fn reload_rows() {
    let rows = compute_rows();
    with_ui(|ui| {
        if ui.loaded {
            ui.list.reload(rows);
        }
    });
}

/// Route a Config-tab write through the live handler the running mux already
/// owns, so General and Config never disagree. True when routed. A toggle
/// route only fires when the flip lands on the wanted value; a drifted view
/// falls through to the plain write.
async fn route_live(
    view: &mut View,
    id: &str,
    value: &str,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> bool {
    let wants = |current: bool, text: &str| current == (text == "false");
    let action = match (id, value) {
        ("mux.hover_focus", v) if wants(view.hover_focus, v) => Some(AuxAction::ToggleHoverFocus),
        ("mux.status_row", v) if wants(view.status_on, v) => Some(AuxAction::ToggleStatus),
        ("resource_meter.enabled", v) if wants(view.resource_meter_on, v) => {
            Some(AuxAction::ToggleResourceMeter)
        }
        ("sideline.layout", "card")
            if view.sideline_layout == crate::sideline_color::SidelineLayout::List =>
        {
            Some(AuxAction::ToggleSidelineLayout)
        }
        ("sideline.layout", "list")
            if view.sideline_layout == crate::sideline_color::SidelineLayout::Card =>
        {
            Some(AuxAction::ToggleSidelineLayout)
        }
        ("mux.launch_screen", "agents")
            if view.launch_screen == crate::view_store::LaunchScreen::Hq =>
        {
            Some(AuxAction::ToggleLaunchScreen)
        }
        ("mux.launch_screen", "hq")
            if view.launch_screen == crate::view_store::LaunchScreen::Agents =>
        {
            Some(AuxAction::ToggleLaunchScreen)
        }
        ("mux.theme", name) => Some(AuxAction::ApplyTheme((*name).to_string())),
        ("split.opens", v) => {
            view.split_opens = if v == "portal" {
                row_menu::SplitOpens::Portal
            } else {
                row_menu::SplitOpens::Pane
            };
            let shape = view.split_opens.word();
            record_write(
                id,
                config_set::spawn_config_set("split.opens", shape)
                    .await
                    .err(),
            );
            return true;
        }
        _ => None,
    };
    match action {
        Some(action) => match settings_modal::run_action(view, action, sock_w).await {
            Ok(()) => true,
            Err(message) => {
                with_ui(|ui| ui.list.set_note(id, message));
                true
            }
        },
        None => false,
    }
}

/// Open `$EDITOR` on a seeded temp file, then write the trimmed text through
/// the same door. An empty or unchanged save writes nothing; a failed seed
/// paints under the row like any refused write.
async fn edit_value(
    view: &mut View,
    id: &str,
    seed: &str,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) {
    let dir = std::env::temp_dir().join("fno-mux-config-edit");
    let path = dir.join(format!("{}.toml", id.replace('.', "_")));
    let seeded = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, seed));
    if let Err(e) = seeded {
        record_write(id, Some(format!("editor seed: {e}")));
        return;
    }
    let edited = path.clone();
    let ok = tokio::task::spawn_blocking(move || editor::edit_file_suspended(&edited))
        .await
        .unwrap_or(false);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    // A leftover seed is only overwritten by the next edit; removal is best effort.
    let _removed_seed = std::fs::remove_file(&path).is_ok();
    if !ok {
        return;
    }
    let text = text.trim().to_string();
    if text.is_empty() || text == seed {
        return;
    }
    set_key(view, id, &text, sock_w).await;
}
