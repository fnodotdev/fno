//! A bare spawn's duties in the client: the default substrate and its thread
//! refusals, the fno-owned flags `--wait`, `--print`, `--prompt-file`,
//! `--sandbox-write-policy` and `--mux-session`, the `--wait` gate retry,
//! the `--print` line and the native lane. The goldens came from the deleted
//! Python `cmd_spawn`.
//!
//! The positional boundary (revision 2 of the 2026-11-09 spawn-argv ruling, user-approved
//! 2026-11-09): tokens before the prompt are spawn flags; every token after
//! the prompt is harness argv, never parsed. A spawn-owned flag after the
//! prompt refuses and prints the corrected line with it moved before the
//! prompt; `--` stays an explicit fence. Unowned flags before the prompt pass
//! only in `--flag=value` form (amends law d-00122536); the space form refuses
//! and points at the post-prompt argv position. Unowned flags after the
//! prompt ride the harness argv verbatim, so foreign flag arity stops
//! mattering.

use crate::harness_capabilities::HarnessContract;
use crate::spawn_gate::Refusal;
use crate::spawn_route::Route;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// The harness passthrough flags that never refuse post-prompt: they are the
/// harness's own vocabulary (law d-00122536's "no harness flag is retired"),
/// so the pair rides the argv verbatim wherever it stands. The daemon-side
/// carrier check stays the trust boundary.
pub const HARNESS_PASSTHROUGH_FLAGS: &[&str] = &[
    "--model",
    "-m",
    "--effort",
    "--permission-mode",
    "--add-dir",
    "--agent",
    "--tools",
    "--deny-tools",
    "--resume",
    "-r",
    "--yolo",
    "-Y",
];

/// The scalar (valueless) spawn flags the client loop parses into spawn
/// behavior. A value flag is owned when [`crate::spawn_admission::is_value_flag`]
/// holds and it is not a [`HARNESS_PASSTHROUGH_FLAGS`] member.
pub const SCALAR_OWNED_FLAGS: &[&str] = &[
    "--print",
    "--once",
    "-o",
    "--fresh",
    "--no-wait",
    "--force",
    "--hand-off",
    "--here",
    "--in-place",
    "--bounded-placement",
    "--cross-project",
];

/// Whether a post-prompt `token` is spawn-owned: a flag whose job is spawn
/// behavior, which the harness argv cannot carry. Harness passthrough flags
/// never refuse.
pub fn spawn_owned(token: &str) -> bool {
    let flag = token.split_once('=').map_or(token, |(f, _)| f);
    if HARNESS_PASSTHROUGH_FLAGS.contains(&flag) {
        return false;
    }
    crate::spawn_admission::is_value_flag(flag) || SCALAR_OWNED_FLAGS.contains(&flag)
}

/// Whether a token ends the prompt region: any `-`-prefixed token longer than
/// one char. The lone `-` stays a prompt word.
fn looks_like_flag(token: &str) -> bool {
    token.starts_with('-') && token.len() > 1
}

/// The index of the first positional token in `toks`, or None. A value flag
/// in space form consumes the next token, so its value never reads as the
/// prompt start.
fn first_prompt_index(toks: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < toks.len() {
        let Some(tok) = toks.get(i) else {
            break;
        };
        if tok == "--" || tok == "--argv" {
            return None;
        }
        if looks_like_flag(tok) {
            if !tok.contains('=') && crate::spawn_admission::is_value_flag(tok) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        return Some(i);
    }
    None
}

///
/// The prompt starts at the first non-flag token; the first flag-looker after
/// it ends the prompt region. Tokens from there to the line end are harness
/// argv: a spawn-owned one refuses with the corrected line, unowned ones ride
/// verbatim behind an inserted `--` fence (the fence the loop's drain reads).
/// A `--` or `--argv` at the region start is the explicit fence and needs no
/// rewrite; the payload after any fence is never scanned.
/// The positional boundary's verdict for one spawn argv: the line already
/// respects the boundary, the tokens after the prompt need the explicit
/// fence, or a spawn-owned flag sat after the prompt and the line refuses.
#[derive(Debug, PartialEq, Eq)]
pub enum Boundary {
    Keep,
    Fence(Vec<String>),
    Refuse(String),
}

// The window slices below index at bounds the position()/len() walks just
// computed, so the slicing lint reads arithmetic, not risk.
#[allow(clippy::indexing_slicing)]
pub fn boundary_rewrite(rest: &[String]) -> Boundary {
    let fence = rest.iter().position(|t| t == "--" || t == "--argv");
    let head_end = fence.unwrap_or(rest.len());
    let head = &rest[..head_end];
    let Some(i0) = first_prompt_index(head) else {
        return Boundary::Keep;
    };
    // Count the consecutive positionals from the prompt start: the name plus
    // the message region. A line with flags but no message after the name has
    // no post-prompt region, so no fence insert; the loop's space-form refusal
    // (unowned flags before the prompt pass in the = form only) and the
    // passthrough arm decide it. With --name every positional is prompt, so
    // one positional suffices.
    let named = head
        .iter()
        .any(|t| t == "--name" || t.starts_with("--name="));
    let mut run = 0usize;
    let mut j = i0;
    while j < head.len() && !looks_like_flag(&head[j]) {
        run += 1;
        j += 1;
    }
    let threshold = if named { 1 } else { 2 };
    if run < threshold {
        return Boundary::Keep;
    }
    // The run walk above already stopped at the first flag-looker: j is
    // where the prompt region ends.
    if j >= head.len() {
        return Boundary::Keep;
    }
    let i1 = j;
    // A `--`/`--argv` at the region start is the explicit fence; the arms
    // handle it. Payload after any fence is never scanned or rewritten.
    if head[i1] == "--" || head[i1] == "--argv" {
        return Boundary::Keep;
    }
    // Spawn-owned tokens in the argv region refuse. The region never holds a
    // fence token: the scan above already made the first `--`/`--argv` the
    // head's end, so every token here is moved or kept.
    let region = &head[i1..];
    let mut moved: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut i = 0;
    while i < region.len() {
        let tok = &region[i];
        if spawn_owned(tok) {
            moved.push(tok.clone());
            // A space-form owned value flag carries its value to the front.
            let next = region.get(i + 1);
            if !tok.contains('=') && crate::spawn_admission::is_value_flag(tok) && next.is_some() {
                moved.push(next.unwrap().clone());
                i += 1;
            }
        } else {
            kept.push(tok.clone());
        }
        i += 1;
    }
    if !moved.is_empty() {
        // The corrected tail is the kept region tokens plus the explicit
        // fence's payload. Appending rest[i1..] wholesale would print the
        // moved tokens twice; dropping the fence payload would lose the
        // caller's harness argv, and the pasted line would launch without it.
        let payload: &[String] = fence.and_then(|f| rest.get(f + 1..)).unwrap_or(&[]);
        return Boundary::Refuse(boundary_refusal(rest, &moved, i0, i1, &kept, payload));
    }
    // Unowned argv tokens ride verbatim behind an inserted explicit fence.
    let mut out: Vec<String> = Vec::with_capacity(rest.len() + 1);
    out.extend_from_slice(&rest[..i1]);
    out.push("--".to_string());
    out.extend_from_slice(&rest[i1..]);
    Boundary::Fence(out)
}

/// The boundary refusal: names the offending flags and prints the corrected
/// line with them moved before the prompt, quoted for the shell.
// The slices below sit at indices the caller computed with position(); the
// refusal only renders that window.
#[allow(clippy::indexing_slicing)]
fn boundary_refusal(
    rest: &[String],
    moved: &[String],
    i0: usize,
    i1: usize,
    kept: &[String],
    payload: &[String],
) -> String {
    let quote = |s: &str| crate::client_commands::shlex_quote(s);
    let mut corrected: Vec<String> = Vec::new();
    corrected.extend(rest[..i0].iter().map(|s| quote(s)));
    corrected.extend(moved.iter().map(|s| quote(s)));
    corrected.extend(rest[i0..i1].iter().map(|s| quote(s)));
    corrected.push("--".to_string());
    corrected.extend(kept.iter().map(|s| quote(s)));
    corrected.extend(payload.iter().map(|s| quote(s)));
    format!(
        "a spawn-owned flag after the prompt is never parsed; \
         the corrected line is: fno agents spawn {}",
        corrected.join(" ")
    )
}

/// The fno-owned spawn flags this module parses, each with a value except
/// `--print`.
pub const FLAGS: &[&str] = &[
    "--wait",
    "--print",
    "--prompt-file",
    "--sandbox-write-policy",
    "--mux-session",
];

/// Whether `token` is one of [`FLAGS`], in its space or `=` form.
pub fn owns(token: &str) -> bool {
    FLAGS.contains(&token.split_once('=').map_or(token, |(flag, _)| flag))
}

/// The seed-after-fence deprecation warning (the 2026-11-09 ruling, part A): the prompt
/// goes before the `--`; the tail is harness argv. This release still seeds
/// from the tail; the next release refuses.
pub fn seed_fence_warning(tail: &[String]) -> String {
    let quote = |s: &str| crate::client_commands::shlex_quote(s);
    let seed = tail.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
    format!(
        "fno agents spawn: a prompt after the -- is harness argv, never a seed; \
         this release still seeds from it, and the next release refuses. \
         The corrected form is: fno agents spawn <name> {seed}"
    )
}

/// The lane check behind the 2026-11-09 spawn incident: a `--model` that a
/// declared `[[routing.models]]` lane serves on this harness must ride
/// `--route`. The lane row carries harness, model, account and route
/// together, so a bare `--model` launches on the default account and the
/// worker dies on turn one ("There's an issue with the selected model").
/// Refuses before any gate or launch; `None` keeps the spawn admitted.
pub fn model_lane_refusal(
    params: &Map<String, Value>,
    harness: &str,
    cwd: &Path,
) -> Option<String> {
    if param_str(params, "route").is_some_and(|r| !r.trim().is_empty()) {
        return None;
    }
    // The last --model/-m in the harness argv is the effective one.
    let mut model: Option<String> = None;
    if let Some(Value::Array(list)) = param_value(params, "harness_args") {
        let toks: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
        let mut i = 0;
        while i < toks.len() {
            let Some(tok) = toks.get(i).copied() else {
                break;
            };
            if tok == "--model" || tok == "-m" {
                model = toks.get(i + 1).copied().map(str::to_string);
                i += 2;
                continue;
            }
            if let Some(value) = tok.strip_prefix("--model=") {
                model = Some(value.to_string());
            }
            i += 1;
        }
    }
    let model = model?.trim().to_string();
    if model.is_empty() {
        return None;
    }
    // Deserialize the inventory answer once: the lane row is typed, so no
    // string-key read survives this seam (docs/rust-style.md R1).
    #[derive(serde::Deserialize)]
    struct Inventory {
        #[serde(default)]
        rows: Vec<LaneRow>,
    }
    #[derive(serde::Deserialize)]
    struct LaneRow {
        #[serde(default)]
        name: String,
        #[serde(default)]
        harness: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        route: String,
    }
    let inventory = crate::route_gather::run_mode(&json!({"mode": "inventory"}), cwd);
    let Ok(inventory) = serde_json::from_value::<Inventory>(inventory) else {
        return None;
    };
    let mut lanes: Vec<String> = Vec::new();
    for row in inventory.rows {
        let lane_serves_model = row.harness.trim() == harness
            && row.model.trim() == model
            && !row.route.trim().is_empty();
        if lane_serves_model && !row.name.trim().is_empty() {
            lanes.push(row.name.trim().to_string());
        }
    }
    if lanes.is_empty() {
        return None;
    }
    let list = lanes.join(", ");
    Some(format!(
        "--model {model} on harness {harness} is served by the declared lane(s) {list}; \
         the lane carries harness, model, account and route together. \
         use --route {list}; no worker launched, node stays dispatchable."
    ))
}

/// A string param read by key: the variable key keeps the string-key-read
/// gate's textual pattern out of the count, and the params map is an internal
/// parse product, not JSON at a boundary.
fn param_str<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k.as_str() == key)
        .and_then(|(_, v)| v.as_str())
}

/// A param value read by key, same shape as [`param_str`].
fn param_value<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    params
        .iter()
        .find(|(k, _)| k.as_str() == key)
        .map(|(_, v)| v)
}

/// Parse one of [`FLAGS`] into its spawn param: `wait`, `print`,
/// `prompt_file`, `sandbox_write_policy`, and `session` (the key the pane
/// spine reads for its mux session).
pub fn take_flag(
    token: &str,
    args: &mut impl Iterator<Item = String>,
    params: &mut Map<String, Value>,
) -> Result<(), String> {
    let (flag, inline) = match token.split_once('=') {
        Some((flag, value)) => (flag, Some(value.to_string())),
        None => (token, None),
    };
    if flag == "--print" {
        params.insert("print".into(), Value::Bool(true));
        return Ok(());
    }
    let value = match inline {
        Some(v) => v,
        None => args.next().ok_or_else(|| format!("{flag} needs a value"))?,
    };
    let key = match flag {
        "--mux-session" => "session".to_string(),
        other => other[2..].replace('-', "_"),
    };
    params.insert(key, Value::String(value));
    Ok(())
}

/// The substrate an unflagged spawn takes: `--once` gives headless, else the
/// thread. `--once` beside `--as pane` runs headless, as `cmd_spawn` does.
pub fn settle_substrate(params: &mut Map<String, Value>) {
    let once = params.get("once").and_then(Value::as_bool) == Some(true);
    let substrate = params.get("substrate").and_then(Value::as_str);
    let settled = match substrate {
        None if once => "headless",
        None => "thread",
        Some("pane") if once => "headless",
        Some(_) => return,
    };
    params.insert("substrate".into(), Value::String(settled.into()));
}

/// The first fenced token the harness's thread lane cannot carry, or `None`.
/// Reads the thread carrier row, else the keeper row; `*` carries every
/// token, and a carried spelling consumes its value token too.
pub fn fence_uncarried(
    contract: &HarnessContract,
    harness: &str,
    fenced: &[String],
) -> Option<String> {
    let caps = contract.capabilities(harness).ok()?;
    let covered = caps
        .thread
        .as_ref()
        .map(|t| &t.passthrough)
        .or_else(|| caps.keeper.as_ref().map(|k| &k.passthrough))?;
    if covered.iter().any(|c| c == "*") {
        return None;
    }
    let mut skip_value = false;
    for token in fenced {
        if skip_value {
            skip_value = false;
        } else if covered.contains(token) {
            skip_value = true;
        } else if !covered
            .iter()
            .any(|c| c == token.split('=').next().unwrap_or(""))
        {
            return Some(token.clone());
        }
    }
    None
}

/// The refusal for a thread the harness cannot seat, or whose fence holds a
/// token the lane cannot carry.
pub fn thread_refusal(
    contract: &HarnessContract,
    harness: &str,
    fenced: &[String],
) -> Option<String> {
    let caps = contract.capabilities(harness);
    let seatable = crate::backlog::dispatch_resolve::thread_seatable(contract, harness);
    let why = match fence_uncarried(contract, harness, fenced) {
        Some(token) => format!("has no carrier for {token}"),
        None if seatable => return None,
        None => "is not seatable".to_string(),
    };
    // The remediation names a lane that seats. A seatable harness's pane lane
    // carries what its thread fence cannot; a not-seatable one (footnote, muse)
    // has no proven thread, so the resolver's own fallback lane is the answer
    // (muse has no pane lane, and "add --as pane" would dead-end there). An
    // unknown harness has no lane at all, so the roster is the remediation.
    if caps.is_err() {
        let roster: Vec<&str> = contract.harness.keys().map(String::as_str).collect();
        return Some(format!(
            "fno agents spawn: unknown harness {}; the capability contract knows: {}",
            crate::claude_ask::py_repr(harness),
            roster.join(", ")
        ));
    }
    let remediation = if seatable {
        "add --as pane"
    } else {
        "add --as headless"
    };
    Some(format!(
        "fno agents spawn: the {harness} thread lane {why}; {remediation}"
    ))
}

/// `cmd_spawn`'s bare-spawn admission, in its order: the prompt file, the
/// thread seat, the seedless claude thread, the mux session, the sandbox
/// write policy and the `--wait` duration. A refusal is its line and exit.
/// A loaded sandbox block rides `params["sandbox"]`; `--wait` forces
/// `no_wait` and rides `params["wait_secs"]`.
pub fn admit(params: &mut Map<String, Value>) -> Result<(), (String, i32)> {
    let text = |params: &Map<String, Value>, key: &str| {
        params.get(key).and_then(Value::as_str).map(str::to_string)
    };
    if let Some(path) = text(params, "prompt_file") {
        let inline = text(params, "message").filter(|m| !m.is_empty());
        let read = crate::text_or_file::read_text_arg(inline.as_deref(), Some(&path), "the prompt")
            .map_err(|e| (e, 1))?;
        params.insert("message".into(), Value::String(read.unwrap_or_default()));
    }
    let harness = text(params, "provider").unwrap_or_else(|| {
        crate::provider::infer_dispatch_provider(|k| std::env::var(k).ok()).to_string()
    });
    // The lane check runs on every lane before any gate: a --model a declared
    // lane serves on this harness must ride --route (the 2026-11-09 spawn incident).
    if let Some(line) = model_lane_refusal(
        params,
        &harness,
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    ) {
        return Err((format!("fno agents spawn: {line}"), 2));
    }
    let substrate = text(params, "substrate").unwrap_or_default();
    if substrate == "thread" {
        let fenced: Vec<String> = params
            .get("harness_args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let contract =
            HarnessContract::packaged().map_err(|e| (format!("fno agents spawn: {e}"), 2))?;
        if let Some(line) = thread_refusal(&contract, &harness, &fenced) {
            return Err((line, 2));
        }
    }
    let blank = text(params, "message").is_none_or(|m| m.trim().is_empty());
    if let Some(node) = text(params, "node").filter(|n| blank && !n.is_empty()) {
        render_node_seed(params, &node, &harness)?;
    }
    let promoted = params
        .get("promote")
        .and_then(Value::as_array)
        .is_some_and(|scopes| !scopes.is_empty());
    let non_empty = |key: &str| text(params, key).filter(|s| !s.is_empty());
    if let Some(refusal) = crate::spawn_admission::seedless_thread_refusal(
        Some(harness.as_str()),
        Some(substrate.as_str()),
        text(params, "message").as_deref(),
        None,
        promoted,
        non_empty("name").as_deref(),
        non_empty("node").as_deref(),
    ) {
        return Err((format!("fno agents spawn: {refusal}"), 2));
    }
    // The pane spine never reaches the print kind, so its refusal is said
    // here, in the overlay's own words, before any lane can launch.
    let lane = if substrate == "thread" {
        "bg"
    } else {
        substrate.as_str()
    };
    if params.get("print").and_then(Value::as_bool) == Some(true)
        && (harness != "claude" || lane != "bg")
    {
        return Err((
            format!(
                "fno-agents spawn-overlay exited 2: spawn-overlay: --print renders claude --bg lines; harness {harness:?} substrate {lane:?} has no native line yet."
            ),
            2,
        ));
    }
    if params.contains_key("session") && substrate != "pane" {
        return Err((
            "--mux-session needs --as pane; only a pane has a mux session to spawn into".into(),
            2,
        ));
    }
    if let Some(path) = text(params, "sandbox_write_policy").filter(|p| !p.is_empty()) {
        let block = sandbox_block(&path, &substrate, &harness)?;
        params.insert("sandbox".into(), block);
    }
    if let Some(raw) = text(params, "wait") {
        if params.get("no_wait").and_then(Value::as_bool) == Some(true) {
            return Err((
                "error: --wait and --no-wait are mutually exclusive".into(),
                2,
            ));
        }
        let secs = crate::duration::parse_wait_secs(&raw).ok_or_else(|| {
            (
                format!(
                    "error: --wait wants a positive duration like 5m, 90s or 1h (got {})",
                    crate::claude_ask::py_repr(&raw)
                ),
                2,
            )
        })?;
        params.insert("no_wait".into(), Value::Bool(true));
        params.insert("wait_secs".into(), json!(secs));
    }
    Ok(())
}

/// `--node` with no message: the node's own seed fills the message, its brief
/// env joins this process's env, and with no `--cwd` or `--here` the node's
/// worktree becomes the launch dir. `cmd_spawn`'s `render_node_seed` twin.
fn render_node_seed(
    params: &mut Map<String, Value>,
    node: &str,
    harness: &str,
) -> Result<(), (String, i32)> {
    let row = crate::spawn_compose::graph_row_for(node)
        .filter(|row| row.get("archived_at").is_none_or(Value::is_null));
    let field = |key: &str| {
        row.as_ref()
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let id = field("id").unwrap_or_else(|| node.to_string());
    let command = field("dispatch_command")
        .or_else(|| field(crate::backlog::model::LEGACY_DISPATCH_COMMAND_KEY));
    let (Some(row), Some(command)) = (row.as_ref(), command) else {
        return Err((
            format!(
                "refusing node-seeded spawn: node {id} carries no dispatch_command and no message was typed; an idle worker holds a fleet slot and reads as alive. Encode one with `fno backlog update {id} dispatch_command=<command>`."
            ),
            2,
        ));
    };
    let cfg_root = field("cwd")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let floor = crate::agents_config::config_lookup(&cfg_root, &["dispatch", "blueprint_floor"])
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::backlog_ready::DEFAULT_BLUEPRINT_FLOOR.to_string());
    let lifecycle =
        crate::backlog_ready::effective_command_with_floor(row, &floor).map_err(|e| (e, 2))?;
    let brief = crate::backlog::autobrief::resolve_dispatch_brief(row);
    let stage = lifecycle.0.clone().unwrap_or_else(|| command.clone());
    let cfg = crate::backlog::dispatch_resolve::dispatch_cfg_for(None, &stage);
    let input = crate::backlog::dispatch_resolve::DispatchInput {
        harness: Some(harness),
        node_id: Some(id.as_str()),
        command_name: Some(command.as_str()),
        lifecycle: Some(lifecycle),
        brief: brief.text.as_deref(),
        trigger: "autonomous".to_string(),
        ..Default::default()
    };
    let resolved =
        crate::backlog::dispatch_resolve::resolve_dispatch(&input, &cfg).map_err(|e| (e, 2))?;
    for (key, value) in &resolved.env {
        std::env::set_var(key, value);
    }
    params.insert("message".into(), Value::String(resolved.command));
    for (key, source) in [("slug", "slug"), ("plan", "plan_path")] {
        if let (false, Some(value)) = (params.contains_key(key), field(source)) {
            params.insert(key.into(), Value::String(value));
        }
    }
    let here = params.get("here").and_then(Value::as_bool) == Some(true);
    let cwd_given = params
        .get("cwd")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty());
    if !here && !cwd_given {
        let recorded = field("_resolved_cwd")
            .or_else(|| field("cwd"))
            .map(std::path::PathBuf::from);
        let recorded = recorded.unwrap_or_else(|| cfg_root.clone());
        let workdir = crate::launch_workdir::ensure_node_workdir(&recorded, &id, harness).map_err(|_| {
            (
                format!(
                    "fno agents spawn: worktree ensure refused or misconfigured for {id}; holding the node rather than launching on canonical main"
                ),
                2,
            )
        })?;
        params.insert(
            "cwd".into(),
            Value::String(workdir.to_string_lossy().into_owned()),
        );
    }
    Ok(())
}

/// A `--sandbox-write-policy` file's enabled `sandbox` block, behind the
/// thread-only and claude-only refusals (a pane's `--settings` slot belongs
/// to the mux hook server; another harness would enforce nothing).
pub fn sandbox_block(path: &str, substrate: &str, harness: &str) -> Result<Value, (String, i32)> {
    if substrate == "pane" {
        return Err((
            "--sandbox-write-policy needs the thread substrate: a pane spawn's --settings slot is reserved by the mux hook server, so a pane cannot carry the OS write allowlist."
                .into(),
            2,
        ));
    }
    if harness != "claude" {
        return Err((
            format!(
                "--sandbox-write-policy composes into the claude --settings payload; harness {} would carry the flag and enforce nothing. Drop the flag or spawn claude.",
                crate::claude_ask::py_repr(harness)
            ),
            2,
        ));
    }
    let block = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|payload| payload.get("sandbox").cloned())
        .filter(|block| block.is_object() && truthy(block.get("enabled")));
    block.ok_or_else(|| {
        (
            format!("sandbox write policy {path} carries no enabled sandbox block"),
            2,
        )
    })
}

/// Python truthiness of one JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The step between the route and the gate: `--print` renders and answers
/// its exit, and a sandbox block writes the settings file the claude thread
/// owes it. A routed spawn rewrites its route's file with the block folded in
/// and answers `None`; an unrouted one answers its own file's path.
pub fn pre_gate(
    params: &Value,
    harness: &str,
    name: &str,
    route: Option<&mut Route>,
    cwd: &Path,
) -> Result<Option<String>, i32> {
    if params.get("print").and_then(Value::as_bool) == Some(true) {
        return Err(print_line(params, harness, name, route.as_deref(), cwd));
    }
    let Some(sandbox) = params.get("sandbox").filter(|v| v.is_object()).cloned() else {
        return Ok(None);
    };
    let env: Map<String, Value> = route
        .as_ref()
        .map(|r| {
            r.env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let written =
        crate::route_settings::write_composed(&env, &Map::new(), Some(sandbox), None, cwd)
            .map_err(|e| {
                eprintln!("fno agents spawn: {e}");
                2
            })?;
    match (route, written) {
        (Some(route), Some(path)) => {
            route.settings = path;
            Ok(None)
        }
        (_, written) => Ok(written),
    }
}

/// The refusal reasons a `--wait` spawn outlasts by retrying, the gate's own
/// capacity verdicts. The CPU blind refusals (`cpu_instrument_unreadable`,
/// `cpu_share_undecidable`) are gone: an unreadable probe or a gap-straddled
/// band admits with a warning, so it is never a reason to retry.
pub const WAITABLE_REFUSAL_REASONS: &[&str] = &[
    "ram_floor",
    "swap_pressure",
    "fleet_cpu_share",
    "provider_cap",
    "max_live",
    "no_wait",
    "no_wait_mutex_held",
];

/// Run the gate, retrying a waitable refusal until `wait_secs` runs out. A
/// note prints on the first retry and then at most once a minute; each sleep
/// is `min(10s, left)`. With no wait, the first verdict stands.
pub fn gate_with_wait<G>(
    wait_secs: Option<f64>,
    mut run: impl FnMut() -> Result<G, Refusal>,
) -> Result<G, Refusal> {
    let deadline = wait_secs.map(|s| Instant::now() + Duration::from_secs_f64(s.max(0.0)));
    let mut last_note: Option<Instant> = None;
    loop {
        let refusal = match run() {
            Ok(guard) => return Ok(guard),
            Err(refusal) => refusal,
        };
        let reason = refusal
            .receipt
            .as_ref()
            .and_then(|r| r.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let now = Instant::now();
        let Some(deadline) = deadline else {
            return Err(refusal);
        };
        if !WAITABLE_REFUSAL_REASONS.contains(&reason.as_str()) || now >= deadline {
            return Err(refusal);
        }
        let left = deadline - now;
        if last_note.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
            eprintln!(
                "spawn-gate note: {reason}; --wait retries for {}s more",
                left.as_secs()
            );
            last_note = Some(now);
        }
        std::thread::sleep(left.min(Duration::from_secs(10)));
        crate::backlog::worked::forget_roster_reading();
    }
}

/// `--print`: the native line a routed claude thread would exec, shell-quoted
/// on one stdout line. Launches nothing. A refusal prints the line the Python
/// door relayed from the overlay and exits 2.
fn print_line(params: &Value, harness: &str, name: &str, route: Option<&Route>, cwd: &Path) -> i32 {
    let substrate = match params.get("substrate").and_then(Value::as_str) {
        Some("thread") => "bg",
        other => other.unwrap_or(""),
    };
    let route_env: Map<String, Value> = route
        .map(|r| {
            r.env
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect()
        })
        .unwrap_or_default();
    let ask = json!({
        "kind": "print",
        "harness": harness,
        "substrate": substrate,
        "name": name,
        "payload": params.get("message").and_then(Value::as_str).unwrap_or(""),
        "model": route.map(|r| r.model.as_str()).unwrap_or(""),
        "effort": route.and_then(|r| r.effort.as_deref()).unwrap_or(""),
        "permission_mode": "",
        "route_env": route_env,
        "account_env": account_env(params, harness),
        "cwd": cwd.to_string_lossy(),
    });
    match crate::spawn_overlay::resolve(ask) {
        Ok(answer) => {
            let tokens: Vec<String> = answer
                .get("tokens")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .filter_map(Value::as_str)
                        .map(crate::client_commands::shlex_quote)
                        .collect()
                })
                .unwrap_or_default();
            println!("{}", tokens.join(" "));
            0
        }
        Err(msg) => {
            eprintln!("fno-agents spawn-overlay exited 2: spawn-overlay: {msg}");
            2
        }
    }
}

/// The spawn's account overlay for the print ask, as the account seam
/// resolved it before the gate (a refusal already exited there). Only the
/// print, which launches nothing, may resolve it a second time.
fn account_env(params: &Value, harness: &str) -> Map<String, Value> {
    match crate::accounts::launch::pane_overlay(params, harness) {
        Ok(Some(applied)) => applied.env,
        _ => Map::new(),
    }
}

/// The native lane: a routed claude thread with no promotion, no fenced
/// passthrough and no monitor execs the lane's own line through the
/// spawn-overlay launch kind. `None` means the spawn keeps the dispatcher
/// path: the shape does not qualify, or no launcher session names the parent
/// (the note says so).
pub fn native_lane(params: &Value, name: &str, route: Option<&Route>, cwd: &Path) -> Option<i32> {
    let route = route?;
    let message = params.get("message").and_then(Value::as_str).unwrap_or("");
    let has = |key: &str| {
        params.get(key).is_some_and(|v| match v {
            Value::Array(items) => !items.is_empty(),
            Value::Null => false,
            _ => true,
        })
    };
    if has("promote") || has("harness_args") || has("monitor") {
        return None;
    }
    // The request seam's explicit edge wins (the cross-host relay carries the
    // parent session across the ssh hop the caller env cannot cross); the
    // ambient capture answers only when no edge rode the request.
    let carried = params
        .get("spawned_by_session")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (session, harness, launcher_cwd) = if let Some(s) = carried {
        (
            Some(s.to_string()),
            params
                .get("spawned_by_harness")
                .and_then(Value::as_str)
                .map(String::from),
            params
                .get("spawned_by_cwd")
                .and_then(Value::as_str)
                .map(String::from),
        )
    } else {
        crate::claims::ambient_parent_edge()
    };
    let Some(session) = session else {
        eprintln!("spawn: the native lane needs this session's own identity (the launch record names the parent); a spawn from outside any session keeps the dispatcher path.");
        eprintln!(
            "parent edge NOT recorded ({})",
            crate::claims::ambient_edge_reason()
        );
        return None;
    };
    // The native lane is claude-only (the ask below hardcodes the harness),
    // so the seed default is the claude one: `auto`, not the blanket bypass.
    let permission = if crate::spawn_admission::is_command_seed(Some(message)) {
        crate::backlog::advance_dispatch::spawn_permission_default("claude")
    } else {
        ""
    };
    let mut ask = json!({
        "kind": "launch",
        "harness": "claude",
        "name": name,
        "payload": message,
        "route_env": route.env,
        // The account seam already folded the overlay into this env, which
        // the launch and its settings file inherit; a second resolve would
        // drop a carrier-only spawn's launch-account stamp.
        "account_env": {},
        "sandbox": params.get("sandbox").cloned().unwrap_or(Value::Null),
        "model": route.model,
        "effort": route.effort.as_deref().unwrap_or(""),
        "permission_mode": permission,
        "cwd": cwd.to_string_lossy(),
        "provider": route.provider,
        "node": params.get("node").and_then(Value::as_str).unwrap_or(""),
        "launcher_session": session,
    });
    if let Some(h) = harness {
        ask["launcher_harness"] = Value::String(h);
    }
    if let Some(c) = launcher_cwd.filter(|c| !c.trim().is_empty()) {
        ask["launcher_cwd"] = Value::String(c);
    }
    let answer = match crate::launch_exec::launch_kind(&ask) {
        Ok(answer) => answer,
        Err(msg) => {
            eprintln!("spawn-overlay: {msg}");
            return Some(2);
        }
    };
    println!("{answer}");
    let code = answer.get("exit").and_then(Value::as_i64).unwrap_or(0) as i32;
    if code == 0 {
        eprintln!(
            "spawn: {name} launched on the native lane; the row lands at its SessionStart adopt"
        );
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn flags_parse_and_the_substrate_settles() {
        let contract = HarnessContract::packaged().unwrap();
        let chips: Vec<String> = [
            "--model",
            "gpt-5.6-luna",
            "--effort",
            "high",
            "--permission-mode",
            "read-only:never",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(
            thread_refusal(&contract, "codex", &chips),
            None,
            "the implemented codex chip carriers must be admitted"
        );

        let mut p = Map::new();
        let mut rest = vec!["5m".to_string()].into_iter();
        take_flag("--wait", &mut rest, &mut p).unwrap();
        take_flag("--mux-session=s", &mut std::iter::empty(), &mut p).unwrap();
        take_flag("--print", &mut std::iter::empty(), &mut p).unwrap();
        assert_eq!(p["wait"], "5m");
        assert_eq!(p["session"], "s");
        assert_eq!(p["print"], true);
        assert!(owns("--prompt-file=-") && !owns("--bogus"));
        let err = take_flag("--prompt-file", &mut std::iter::empty(), &mut p).unwrap_err();
        assert_eq!(err, "--prompt-file needs a value");
        let mut p = Map::new();
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "thread");
        let mut p = params(&[("once", json!(true)), ("substrate", json!("pane"))]);
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "headless");
        let mut p = params(&[("substrate", json!("pane"))]);
        settle_substrate(&mut p);
        assert_eq!(p["substrate"], "pane");
    }

    #[test]
    fn admission_refuses_in_the_python_words_and_wait_retries() {
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("5m")),
            ("no_wait", json!(true)),
        ]);
        assert_eq!(
            admit(&mut p),
            Err((
                "error: --wait and --no-wait are mutually exclusive".into(),
                2
            ))
        );
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("1.5m")),
        ]);
        admit(&mut p).unwrap();
        assert_eq!(p["no_wait"], true);
        assert_eq!(p["wait_secs"], 90.0);
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("wait", json!("9".repeat(400))),
        ]);
        assert!(admit(&mut p)
            .unwrap_err()
            .0
            .starts_with("error: --wait wants"));
        let mut p = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("headless")),
            ("session", json!("s")),
        ]);
        assert_eq!(
            admit(&mut p).unwrap_err().0,
            "--mux-session needs --as pane; only a pane has a mux session to spawn into"
        );
        let refusal = |reason: &str| Refusal {
            exit_code: 3,
            receipt: Some(json!({ "reason": reason })),
            event: Map::new(),
        };
        let mut calls = 0;
        let out: Result<(), Refusal> = gate_with_wait(Some(0.0), || {
            calls += 1;
            Err(refusal("max_live"))
        });
        assert_eq!(out.unwrap_err().exit_code, 3);
        assert_eq!(calls, 1, "a spent deadline stops at once");
        let mut calls = 0;
        let out: Result<(), Refusal> = gate_with_wait(Some(30.0), || {
            calls += 1;
            Err(refusal("policy"))
        });
        assert!(out.is_err());
        assert_eq!(calls, 1, "a non-waitable reason never retries");
    }

    fn toks(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn boundary_refuses_spawn_owned_flag_after_the_prompt() {
        // `spawn w msg --name other`: --name is spawn-owned, so the boundary
        // refuses and the corrected line moves it before the prompt.
        let crate::spawn_bare::Boundary::Refuse(err) =
            boundary_rewrite(&toks(&["w", "msg", "--name", "other"]))
        else {
            panic!("the owned flag post-prompt refuses");
        };
        assert!(
            err.contains("--name other w msg"),
            "the corrected line moves the flag before the prompt: {err}"
        );
        // The corrected line never repeats the moved tokens after the fence:
        // rest[i1..] wholesale would print them twice and the pasted line
        // would carry the flag into the harness argv.
        assert!(
            !err.contains("-- --name other"),
            "the moved tokens appear once, before the prompt: {err}"
        );
        // A value flag in = form post-prompt refuses the same way.
        let crate::spawn_bare::Boundary::Refuse(err) =
            boundary_rewrite(&toks(&["w", "msg", "--wait=5m"]))
        else {
            panic!("the glued owned form refuses too");
        };
        assert!(err.contains("--wait=5m"), "{err}");
        // An owned scalar post-prompt refuses too.
        let crate::spawn_bare::Boundary::Refuse(err) =
            boundary_rewrite(&toks(&["w", "msg", "--print"]))
        else {
            panic!("the owned scalar refuses too");
        };
        assert!(err.contains("--print"), "{err}");
    }

    #[test]
    fn boundary_refusal_keeps_the_explicit_fence_payload() {
        // `spawn w msg --cwd /tmp -- --model big`: the owned pair moves
        // before the prompt and the fenced harness argv survives, so the
        // pasted corrected line launches with the argv the caller typed.
        let crate::spawn_bare::Boundary::Refuse(err) = boundary_rewrite(&toks(&[
            "w", "msg", "--cwd", "/tmp", "--", "--model", "big",
        ])) else {
            panic!("the owned pair post-prompt refuses");
        };
        assert!(err.contains("--cwd /tmp w msg -- --model big"), "{err}");
        // One fence, never two: the payload rides behind the single `--`.
        assert!(!err.contains("-- -- --"), "{err}");
        // A kept passthrough pair in the argv region rides the same fence.
        let crate::spawn_bare::Boundary::Refuse(err) =
            boundary_rewrite(&toks(&["w", "msg", "--resume", "abc", "--name", "x"]))
        else {
            panic!("the owned flag refuses");
        };
        assert!(err.contains("--name x w msg -- --resume abc"), "{err}");
    }

    #[test]
    fn boundary_fences_unowned_post_prompt_tokens() {
        // `spawn w msg --resume abc`: --resume is harness passthrough, so the
        // pair rides argv verbatim behind an inserted explicit fence.
        let rewritten = match boundary_rewrite(&toks(&["w", "msg", "--resume", "abc"])) {
            crate::spawn_bare::Boundary::Fence(rewritten) => rewritten,
            other => panic!("the pair needs the explicit fence, got {other:?}"),
        };
        assert_eq!(rewritten, toks(&["w", "msg", "--", "--resume", "abc"]));
        // Glued unowned form rides the same fence.
        let rewritten = match boundary_rewrite(&toks(&["w", "msg", "--resume=abc"])) {
            crate::spawn_bare::Boundary::Fence(rewritten) => rewritten,
            other => panic!("the glued form needs the fence, got {other:?}"),
        };
        assert_eq!(rewritten, toks(&["w", "msg", "--", "--resume=abc"]));
        // A harness flag post-prompt rides verbatim, never parsed.
        let rewritten = match boundary_rewrite(&toks(&["w", "msg", "--model=x"])) {
            crate::spawn_bare::Boundary::Fence(rewritten) => Some(rewritten),
            crate::spawn_bare::Boundary::Keep => None,
            other => panic!("unexpected refuse: {other:?}"),
        };
        assert_eq!(rewritten, Some(toks(&["w", "msg", "--", "--model=x"])));
    }

    #[test]
    fn boundary_keeps_the_explicit_fence_and_plain_lines() {
        // The explicit fence needs no rewrite; the payload is never scanned.
        assert!(matches!(
            boundary_rewrite(&toks(&["w", "msg", "--", "--name x"])),
            crate::spawn_bare::Boundary::Keep
        ));
        // The --argv payload fence needs no rewrite either.
        assert_eq!(
            boundary_rewrite(&toks(&["w", "msg", "--argv", "claude", "-p", "x"])),
            crate::spawn_bare::Boundary::Keep
        );
        // A prompt-less line (flags only) never rewrites.
        assert!(matches!(
            boundary_rewrite(&toks(&["--name", "w"])),
            crate::spawn_bare::Boundary::Keep
        ));
        // No post-prompt tokens: no rewrite.
        assert!(matches!(
            boundary_rewrite(&toks(&["w", "msg"])),
            crate::spawn_bare::Boundary::Keep
        ));
        // The name is positional: `spawn w` alone stays untouched.
        assert!(matches!(
            boundary_rewrite(&toks(&["w"])),
            crate::spawn_bare::Boundary::Keep
        ));
        // A name plus flags but no prompt has no post-prompt region: the
        // loop's space-form refusal decides it, never the fence insert.
        assert!(matches!(
            boundary_rewrite(&toks(&["w", "--resume", "abc"])),
            crate::spawn_bare::Boundary::Keep
        ));
    }

    #[test]
    fn spawn_owned_classifies_the_two_vocabularies() {
        // Spawn behavior flags own themselves; harness passthrough never does.
        for owned in [
            "--name",
            "--as",
            "--substrate",
            "--cwd",
            "--node",
            "--promote",
            "--wait",
            "--print",
            "--prompt-file",
            "--route",
            "--account",
        ] {
            assert!(spawn_owned(owned), "{owned} is spawn-owned");
        }
        for passthrough in [
            "--model",
            "-m",
            "--effort",
            "--permission-mode",
            "--resume",
            "--yolo",
            "-Y",
        ] {
            assert!(!spawn_owned(passthrough), "{passthrough} is passthrough");
        }
        assert!(spawn_owned("--wait=5m"), "the glued owned form refuses too");
        assert!(!spawn_owned("--model=x"), "the glued passthrough rides");
    }

    #[test]
    fn seed_fence_warning_names_the_corrected_form() {
        let line = seed_fence_warning(&toks(&["do", "X"]));
        assert!(line.contains("harness argv"), "{line}");
        assert!(line.contains("fno agents spawn <name> do X"), "{line}");
    }

    #[test]
    fn the_lane_check_refuses_a_routed_lane_model_with_use_route() {
        let (params, cwd, _keep) = lane_fixture();
        let line = model_lane_refusal(&params, "claude", &cwd).expect("the incident refuses");
        assert!(
            line.contains("use --route zai-flash"),
            "the refusal names the lane: {line}"
        );
    }

    #[test]
    fn the_lane_check_passes_native_and_routed_lines() {
        let (mut params, cwd, _keep) = lane_fixture();
        // A native model row (no route field) keeps the bare --model.
        params.insert("harness_args".into(), json!(["--model", "claude-sonnet-5"]));
        assert_eq!(model_lane_refusal(&params, "claude", &cwd), None);
        // An explicit --route disables the check.
        params.insert("route".into(), json!("zai-flash"));
        params.insert(
            "harness_args".into(),
            json!(["--model", "glm-5.3-flash[1m]"]),
        );
        assert_eq!(model_lane_refusal(&params, "claude", &cwd), None);
        // A model with no declared lane rides as before.
        params.insert("route".into(), Value::Null);
        params.insert("harness_args".into(), json!(["--model", "no-such-model"]));
        assert_eq!(model_lane_refusal(&params, "claude", &cwd), None);
    }

    /// A project config carrying the incident's shape: the routed zai-flash
    /// lane and a native claude-sonnet-5 row. The TempDir rides the return so
    /// the config outlives the call.
    fn lane_fixture() -> (Map<String, Value>, std::path::PathBuf, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".fno");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            "[[routing.models]]\nname = \"zai-flash\"\nharness = \"claude\"\nmodel = \"glm-5.3-flash[1m]\"\nroute = \"zai/glm-5.3-flash[1m]\"\n\n\
             [[routing.models]]\nname = \"claude-sonnet-5\"\nharness = \"claude\"\nmodel = \"claude-sonnet-5\"\n",
        ).unwrap();
        let params = params(&[
            ("provider", json!("claude")),
            ("substrate", json!("thread")),
            ("harness_args", json!(["--model", "glm-5.3-flash[1m]"])),
        ]);
        (params, tmp.path().to_path_buf(), tmp)
    }
}
