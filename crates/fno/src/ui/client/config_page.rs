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
use crate::config::config_schema::{setting_specs, EditMode, SettingSpec};
use crate::trusted_config::{Fault, TrustedFile, TrustedKey};
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
/// the popup unpinned: the width-measure pass runs before any aux popup
/// carries a selection, and a doc-less row pins nothing.
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
/// only the painted state they leave behind matters to a frame. Returns the
/// selection the state leaves, when it is not the first row.
pub(crate) fn snapshot_seam(view: &mut View, kind: &str) -> Option<usize> {
    let rows = compute_rows();
    // The folded Paths row's selectable index, taken before `rows` moves
    // into the list: the "paths" frame expands the group with one Enter
    // and keeps the cursor on it.
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
            // Enter on the folded Paths row expands it in place.
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
    (kind == "paths").then_some(paths_idx)
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
    let trusted = TrustedFile::from_env();
    let rows: Vec<SettingRow> = setting_specs()
        .into_iter()
        .map(|spec| {
            if let Some(key) = trusted_key(&spec.key) {
                return trusted_row(spec, key, &trusted);
            }
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
                label: row_label(&spec.key, &spec.label),
                value: raw.clone(),
                display: row_display(&spec.key, &raw),
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

/// The row of a key that grants access. It shows the file a save replaces,
/// never the merged view: a save would turn that view's project rows into
/// grants. Rows that cannot be read lock it.
fn trusted_row(spec: SettingSpec, key: TrustedKey, file: &TrustedFile) -> SettingRow {
    let (mode, raw) = match granted_text(key, file) {
        Ok(raw) => (RowMode::Editor, raw),
        Err(fault) => (
            RowMode::Locked {
                reason: fault.to_string(),
            },
            String::new(),
        ),
    };
    SettingRow {
        label: row_label(&spec.key, &spec.label),
        display: row_display(&spec.key, &raw),
        id: spec.key,
        value: raw,
        detail: spec.doc,
        mode,
    }
}

/// The list a save of `key` replaces, as the row and its editor seed spell it.
fn granted_text(key: TrustedKey, file: &TrustedFile) -> Result<String, Fault> {
    crate::setup_messaging::rows_to_rewrite(file, key)
        .map(|rows| value_text(&serde_json::Value::Array(rows)))
}

/// True when `id` grants access and its list is no longer the one `seed` was
/// read from. A save would then undo what changed, such as a revoke.
fn changed_since(id: &str, seed: &str, file: &TrustedFile) -> bool {
    trusted_key(id).is_some_and(|key| granted_text(key, file).ok().as_deref() != Some(seed))
}

/// The painted value for one row: the generic unset paint, or the path
/// shapes. An unset `paths.*` leaf paints its resolved default sketch, dim,
/// relative to the state dir or its class home (`<state>/db/history/ledger.json`);
/// a set one paints the shortened path; State Dir paints the shortened raw
/// path (`~` for home, middle-truncated when long).
fn row_display(key: &str, raw: &str) -> String {
    if key == "state_dir" {
        return shorten_path(raw);
    }
    if key.starts_with("paths.") {
        if raw.is_empty() {
            return path_default_display(key)
                .map(|s| middle_truncate(&s, SKETCH_CAP))
                .unwrap_or_else(|| "not set".into());
        }
        return shorten_path(raw);
    }
    unset_default(raw)
}

/// Fold the `paths.*` leaves into one group row (the user's look on the
/// frames asked for one collapsed "Paths" row). The summary names the count
/// and the override state ("20 · all default", "20 · 2 overridden"). An
/// override is a child with a non-empty value: an unset child paints its
/// default sketch, never an empty cell.
///
/// A setting other rows refer to paints before them: the group opens with
/// the settings its default sketches name (`<state>` is State folder,
/// `<vault>` the Obsidian vault folder), then the roots that stand alone, then the
/// rest by label. The heads stay at the top level too: they are settings
/// in their own right, and the group only repeats them as its key.
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
    children.sort_by_key(|c| {
        let rank = PATH_ROOTS
            .iter()
            .position(|k| *k == c.id)
            .unwrap_or(PATH_ROOTS.len());
        (rank, c.label.to_lowercase())
    });
    let heads: Vec<SettingRow> = PATH_HEADS
        .iter()
        .filter_map(|k| rest.iter().find(|r| r.id == *k).cloned())
        .collect();
    let mut group = heads;
    group.extend(children);
    let mut rest = rest;
    // The group closes the list, folded: most readers never move a path,
    // and Enter expands it in place for the one who does.
    rest.push(SettingRow {
        id: "paths".into(),
        label: "Paths".into(),
        value: String::new(),
        display: format!("{count} · {summary}"),
        detail: "Where fno keeps each kind of file. Enter expands the list.".into(),
        mode: RowMode::Group { rows: group },
    });
    rest
}

/// The settings the path defaults name as placeholders, in the order the
/// group lists them first: `<state>` is State folder, and `<vault>` is the
/// Obsidian vault folder, live only while Obsidian is on. `<repo>` and
/// `{project}` name the checkout and the project id, which the tab does
/// not repeat here.
const PATH_HEADS: [&str; 3] = ["state_dir", "obsidian.enabled", "obsidian.vault"];

/// The path roots that stand outside `<state>`, listed right after the
/// heads; every `<state>/...` row follows by label.
const PATH_ROOTS: [&str; 3] = ["paths.state_dir_local", "paths.data_dir", "paths.cache_dir"];

/// A row's label: the schema's words for the setting, else the key's last
/// segment title-cased (a leaf added without a label still paints).
fn row_label(key: &str, label: &str) -> String {
    if label.is_empty() {
        human_label(key)
    } else {
        label.to_string()
    }
}

/// The dim default sketch an unset `paths.*` row paints, relative to the
/// state dir or its class home. Display knowledge only: the resolvers in
/// fno-agent's `paths_cli.rs` stay the authority (the mux cannot link that
/// crate), and `docs/path-config.md` is the prose source. A key missing
/// here paints "not set" instead.
fn path_default_display(key: &str) -> Option<String> {
    let sketch = |s: &str| format!("<state>/{s}");
    match key {
        "paths.ledger_json" => Some(sketch("db/history/ledger.json")),
        "paths.evals_history" => Some(sketch("db/history/evals-history.jsonl")),
        "paths.briefs_dir" => Some(sketch("db/briefs/")),
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
        "paths.bus_dir" => Some(sketch("db/mail/bus/")),
        "paths.chats" => Some(sketch("db/mail/chats/")),
        "paths.observer_reports_dir" => Some(sketch("db/observer-reports/{owner}/")),
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
    middle_truncate(&tilde, 28)
}

/// The widest default sketch the right column holds whole at the modal's
/// width; a longer one (the handoffs fallback chain) gives up its middle
/// instead of its tail.
const SKETCH_CAP: usize = 52;

/// `s` cut to `cap` chars by dropping its middle: about two thirds head,
/// one third tail, joined by an ellipsis.
fn middle_truncate(s: &str, cap: usize) -> String {
    let n = s.chars().count();
    if n <= cap {
        return s.to_string();
    }
    let keep = cap - 1;
    let tail_len = keep / 3;
    let head: String = s.chars().take(keep - tail_len).collect();
    let tail: String = s.chars().skip(n - tail_len).collect();
    format!("{head}\u{2026}{tail}")
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
                label: row_label(&spec.key, &spec.label),
                value: raw.clone(),
                display: unset_default(&raw),
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

/// The painted value for an override row: an empty value paints `not set`
/// instead of a blank cell, whether the default is null or an empty string
/// (`user.name`): neither has a default worth showing. The true text stays
/// in `value`, so an editor row seeds empty.
fn unset_default(raw: &str) -> String {
    if raw.is_empty() {
        "not set".to_string()
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

/// Clear one key through the same door (`fno config unset`), in the file
/// [`write_value`] writes: a revoke must remove the row that grants.
async fn unset_key(id: &str) {
    let local = !machine_only(id);
    record_write(id, config_set::spawn_scoped(id, None, local).await.err());
    reload_rows();
}

/// A key that belongs to the user file: the host list, and the keys that
/// grant access, which a project file cannot set.
fn machine_only(id: &str) -> bool {
    id == "mux.remote_hosts" || trusted_key(id).is_some()
}

fn trusted_key(id: &str) -> Option<TrustedKey> {
    TrustedKey::ALL.into_iter().find(|key| key.dotted() == id)
}

/// The one write door: `fno config set`, project scope, except a
/// [`machine_only`] key.
async fn write_value(id: &str, value: &str) -> Option<String> {
    let result = if machine_only(id) {
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
    if changed_since(id, seed, &TrustedFile::from_env()) {
        let message = "the list changed while you edited, so nothing was saved; edit it again";
        record_write(id, Some(message.to_string()));
        reload_rows();
        return;
    }
    set_key(view, id, &text, sock_w).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The editor saves a trusted key to the global file, so it opens on that
    /// file's rows and refuses to save over a list that changed since.
    #[test]
    fn a_trusted_row_edits_the_global_list_only() {
        let _env = crate::digest_overlay::ENVIRONMENT_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(checkout.join(".fno")).expect("checkout");
        let global = dir.path().join("config.toml");
        let row = |who: &str| format!("[[web.invites]]\nemail = \"{who}\"\nrole = \"owner\"\n");
        std::fs::write(&global, row("kept@example.com")).expect("global seeded");
        std::fs::write(checkout.join(".fno/config.toml"), row("clone@example.com"))
            .expect("project seeded");
        let names = ["FNO_CONFIG", "FNO_GLOBAL_SETTINGS_PATH", "FNO_REPO_ROOT"];
        let saved = names.map(std::env::var_os);
        std::env::remove_var("FNO_CONFIG");
        std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", &global);
        std::env::set_var("FNO_REPO_ROOT", &checkout);
        let rows = compute_rows();
        let file = TrustedFile::from_env();
        for (name, value) in names.into_iter().zip(saved) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let seed = rows
            .iter()
            .find(|r| r.id == "web.invites")
            .map(|r| r.value.clone())
            .expect("the invites row");
        assert!(seed.contains("kept@example.com"), "{seed}");
        assert!(!seed.contains("clone@example.com"), "{seed}");
        assert!(!changed_since("web.invites", &seed, &file));
        std::fs::write(&global, "").expect("revoked");
        assert!(
            changed_since("web.invites", &seed, &file),
            "a revoke stands"
        );
    }

    /// A long default sketch keeps both ends inside the column, and a key
    /// with no default says so instead of the empty word "default".
    #[test]
    fn long_sketches_lose_their_middle_and_null_defaults_read_not_set() {
        let handoffs = row_display("paths.handoffs_dir", "");
        assert_eq!(handoffs.chars().count(), SKETCH_CAP);
        assert!(handoffs.starts_with("<vault>/internal/{project}/"));
        assert!(handoffs.ends_with("{project}/"));
        assert!(handoffs.contains('\u{2026}'));
        let ledger = row_display("paths.ledger_json", "");
        assert_eq!(
            ledger, "<state>/db/history/ledger.json",
            "a short sketch stays whole"
        );
        assert_eq!(row_display("obsidian.vault", ""), "not set");
        assert_eq!(row_display("branch.prefix", "fno"), "fno");
    }

    /// The Paths group opens with the settings its defaults name (State
    /// folder for `<state>`, the Obsidian pair for `<vault>`), then the roots
    /// outside `<state>`, then the rest by label; the group row closes the
    /// top level.
    #[test]
    fn paths_group_lists_the_settings_it_refers_to_first() {
        let row = |id: &str, label: &str| SettingRow {
            id: id.into(),
            label: label.into(),
            value: String::new(),
            display: String::new(),
            detail: String::new(),
            mode: RowMode::Editor,
        };
        let rows = fold_paths(vec![
            row("state_dir", "State folder"),
            row("paths.ledger_json", "Run history and cost file"),
            row("paths.briefs_dir", "Node briefs folder"),
            row("paths.cache_dir", "Cache folder"),
            row("obsidian.vault", "Obsidian vault folder"),
            row("obsidian.enabled", "Use an Obsidian vault"),
        ]);
        let top: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            top,
            ["state_dir", "obsidian.vault", "obsidian.enabled", "paths"]
        );
        let Some(RowMode::Group { rows: group }) = rows.last().map(|r| &r.mode) else {
            panic!("the Paths group row closes the list");
        };
        let order: Vec<&str> = group.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "state_dir",
                "obsidian.enabled",
                "obsidian.vault",
                "paths.cache_dir",
                "paths.briefs_dir",
                "paths.ledger_json",
            ]
        );
    }
}
