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

/// The tab's rows. `sel` is the popup's selected target index (the doc line
/// of the selected row paints under the list); `None` is the width-measure
/// pass, which paints no doc line.
pub(crate) fn rows(sel: Option<usize>) -> Vec<PopupRow> {
    with_ui(|ui| {
        if !ui.loaded {
            return vec![PopupRow::Text("reading config...".into())];
        }
        ui.list.popup_rows(sel)
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
    let mut list = SettingList::new(rows);
    let query = match kind {
        "filter" => "notify_on_done",
        "toggle" => "hover_focus",
        "cycle" => "sideline.layout",
        "page" => "autonomy.mode",
        "detail" => "mux.restore.policy",
        _ => "",
    };
    if !query.is_empty() {
        for c in query.chars() {
            list.feed(c.to_string().as_bytes(), None);
        }
    }
    match kind {
        "page" | "detail" => list.feed(b"\r", Some(0)),
        "toggle" => {
            let mut rows = compute_rows();
            for row in rows.iter_mut() {
                if row.id == "mux.hover_focus" {
                    row.value = "off".into();
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
        *ui = Some(ConfigUi { list, loaded: true });
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
        let ui = ui.get_or_insert_with(ConfigUi::default);
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
    setting_specs()
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
            if let RowMode::Cycle { options, .. } | RowMode::Page { options, .. } = &mut mode {
                let word = effective.as_str().unwrap_or_default().to_string();
                if let Some(pos) = options.iter().position(|o| *o == word) {
                    if let RowMode::Cycle { current, .. } | RowMode::Page { current, .. } =
                        &mut mode
                    {
                        *current = pos;
                    }
                }
            }
            SettingRow {
                id: spec.key.clone(),
                label: spec.key,
                value: value_text(&effective),
                detail: spec.doc,
                mode,
            }
        })
        .collect()
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
) -> Result<Option<StdinFlow>, String> {
    ensure_loaded().await;
    let sel = view.aux.as_ref().map(|m| m.popup.sel);
    let events = with_ui(|ui| ui.list.feed(bytes, sel));
    for event in events {
        match event {
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
                return Ok(Some(StdinFlow::Continue));
            }
            ListEvent::Close => {
                theme_import_ui::reset(view);
                view.aux = None;
                return Ok(Some(StdinFlow::Continue));
            }
            ListEvent::Select(idx) => {
                if let Some(m) = view.aux.as_mut() {
                    m.popup.sel = idx;
                    m.popup.follow_sel(view.term);
                }
            }
            ListEvent::Set { id, value } => {
                set_key(view, &id, &value, sock_w).await?;
            }
            ListEvent::Unset { id } => {
                unset_key(view, &id).await?;
            }
            ListEvent::Edit { id, seed } => {
                edit_value(view, &id, &seed, sock_w).await?;
            }
        }
    }
    if view.aux.is_some() {
        view.reopen_settings_keeping_sel();
    }
    Ok(Some(StdinFlow::Continue))
}

/// Write one key through the one door, routing a latched key through its
/// live handler first so General and Config never disagree.
async fn set_key(
    view: &mut View,
    id: &str,
    value: &str,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if route_live(view, id, value, sock_w).await? {
        reload_rows();
        return Ok(());
    }
    let result = write_value(id, value).await;
    record_write(id, result);
    reload_rows();
    Ok(())
}

/// Clear one key through the same door (`fno config unset`).
async fn unset_key(view: &mut View, id: &str) -> Result<(), String> {
    let _ = view;
    let result = config_set::spawn_config_unset(id).await;
    record_write(id, result);
    reload_rows();
    Ok(())
}

/// The one write door: `fno config set`, project scope, except the global
/// host list, which belongs to the user file.
async fn write_value(id: &str, value: &str) -> Result<(), String> {
    if id == "mux.remote_hosts" {
        return config_set::spawn_config_set_global(id, value).await;
    }
    config_set::spawn_config_set(id, value).await
}

/// Land a write's outcome on its row: a refusal paints under the row and the
/// row keeps its old value (the reload below re-reads it), a success clears
/// any stale refusal.
fn record_write(id: &str, result: Result<(), String>) {
    with_ui(|ui| match result {
        Ok(()) => ui.list.clear_note(),
        Err(message) => ui.list.set_note(id, message),
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
) -> Result<bool, String> {
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
            view.split_opens = if *v == "portal" {
                row_menu::SplitOpens::Portal
            } else {
                row_menu::SplitOpens::Pane
            };
            let shape = view.split_opens.word();
            config_set::spawn_config_set("split.opens", shape).await?;
            return Ok(true);
        }
        _ => None,
    };
    match action {
        Some(action) => {
            settings_modal::run_action(view, action, sock_w).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Open `$EDITOR` on a seeded temp file, then write the trimmed text through
/// the same door. An empty or unchanged save writes nothing.
async fn edit_value(
    view: &mut View,
    id: &str,
    seed: &str,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let dir = std::env::temp_dir().join("fno-mux-config-edit");
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let path = dir.join(format!("{}.toml", id.replace('.', "_")));
    std::fs::write(&path, seed).map_err(|e| format!("seed write: {e}"))?;
    let edited = path.clone();
    let ok = tokio::task::spawn_blocking(move || editor::edit_file_suspended(&edited))
        .await
        .unwrap_or(false);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !ok {
        return Ok(());
    }
    let text = text.trim().to_string();
    if text.is_empty() || text == seed {
        return Ok(());
    }
    set_key(view, id, &text, sock_w).await
}
