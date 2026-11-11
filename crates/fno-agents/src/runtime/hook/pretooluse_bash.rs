//! One Bash PreToolUse entry for the guards that share the same payload.
//!
//! Keep the former registration order. Every guard runs in process. A
//! denial from any guard is returned after all have recorded their
//! decision. The guardrail preset owns whether a guard runs at all: a
//! preset-disabled guard answers nothing and emits no row.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn run(_args: &[String]) -> i32 {
    let raw = super::read_stdin();
    let mut payload: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut refusals = Vec::new();
    let mut notices = Vec::new();

    // One config resolution for the whole chain: guard_enabled re-walks the
    // config candidates per call, and this hook runs on every Bash turn.
    let preset = crate::agents_config::guard_preset(&cwd);

    if crate::agents_config::preset_runs(preset, "bg-process") {
        let bg_refusal = super::bg_process_guard::judge(&payload);
        super::emit_guard_decision(&cwd, "bg-process-guard", "Bash", bg_refusal.is_some());
        refusals.extend(bg_refusal);
    }

    if crate::agents_config::preset_runs(preset, "bin-install") {
        let bin_refusal = super::bin_install_guard::judge(&payload);
        super::emit_guard_decision(&cwd, "bin-install-guard", "Bash", bin_refusal.is_some());
        refusals.extend(bin_refusal);
    }

    if crate::agents_config::preset_runs(preset, "git-protection") {
        // Git reads and the guard's state run in the process cwd, never the
        // payload cwd. A pacing notice warns and proceeds; only a refusal
        // blocks.
        match super::git_protection::judge(&payload, &process_cwd) {
            super::git_protection::Verdict::Block(reason) => {
                super::emit_guard_decision(&cwd, "git-protection", "Bash", true);
                refusals.push(reason);
            }
            super::git_protection::Verdict::Warn(reason) => {
                super::emit_guard_decision(&cwd, "git-protection", "Bash", false);
                notices.push(reason);
            }
            super::git_protection::Verdict::Allow => {
                super::emit_guard_decision(&cwd, "git-protection", "Bash", false);
            }
        }
    }

    if crate::agents_config::preset_runs(preset, "backlog-impostor") {
        let impostor_refusal = super::backlog_impostor::judge(&payload);
        super::emit_guard_decision(
            &cwd,
            "backlog-impostor-guard",
            "Bash",
            impostor_refusal.is_some(),
        );
        refusals.extend(impostor_refusal);
    }

    if crate::agents_config::preset_runs(preset, "recursive-grep") {
        let grep_refusal = super::recursive_grep_guard::judge(&payload);
        super::emit_guard_decision(&cwd, "recursive-grep-guard", "Bash", grep_refusal.is_some());
        refusals.extend(grep_refusal);
    }

    let test = if crate::agents_config::preset_runs(preset, "test-run") {
        super::test_run_guard::evaluate(&payload)
    } else {
        super::test_run_guard::Evaluation {
            should_log: false,
            refusal: None,
            stage: "preset-disabled",
        }
    };
    if test.should_log {
        super::emit_guard_decision(
            &process_cwd,
            "test-run-guard",
            "Bash",
            test.refusal.is_some(),
        );
    }
    refusals.extend(test.refusal);

    if crate::agents_config::preset_runs(preset, "effect") {
        let effect_refusal = crate::effect_gate::judge(&payload, &cwd);
        super::emit_guard_decision(&cwd, "effect-guard", "Bash", effect_refusal.is_some());
        refusals.extend(effect_refusal);
    }

    if crate::agents_config::preset_runs(preset, "refusal-streak") {
        let streak_refusal = super::refusal_streak::judge_pre(&payload);
        super::emit_guard_decision(&cwd, "refusal-streak", "Bash", streak_refusal.is_some());
        refusals.extend(streak_refusal);
    }

    let mut renamed = false;
    if refusals.is_empty() {
        if let Some((note, rewrote)) = name_unnamed_claude_bg(&mut payload, &cwd) {
            notices.push(note);
            renamed = rewrote;
        }
        if let Some(reason) = gate_native_launch(&payload, &cwd) {
            refusals.push(reason);
        }
    }
    if !refusals.is_empty() {
        // A notice rides a refusal so the operator reads one sentence.
        refusals.extend(notices);
        return super::emit_block(&refusals.join("\n\n"));
    }
    record_launch(&payload, &cwd);
    if renamed {
        let input = payload.pointer("/tool_input").unwrap_or(&Value::Null);
        return super::emit_rewrite(input, &notices.join("\n\n"));
    }
    if notices.is_empty() {
        super::emit_allow()
    } else {
        super::emit_warn(&notices.join("\n\n"))
    }
}

/// An unnamed `claude --bg` line gets a derived `--name` written into the
/// tool input, so the gate and the launch record see the name too. Answers
/// the notice and whether the input was rewritten; a harness that drops
/// `updatedInput` runs the line as typed, which is the warn-and-allow case.
fn name_unnamed_claude_bg(payload: &mut Value, cwd: &Path) -> Option<(String, bool)> {
    let cmd = payload
        .pointer("/tool_input/command")?
        .as_str()?
        .to_string();
    if !crate::launch_record::may_launch(&cmd) || cmd.contains("agents spawn") {
        return None;
    }
    let launches = crate::launch_record::parse_command(&cmd, &cwd.to_string_lossy());
    match crate::launch_record::unnamed_claude_bg(&cmd, &launches)? {
        crate::launch_record::UnnamedBg::Rewrite { command, name } => {
            *payload.pointer_mut("/tool_input/command")? = Value::String(command);
            Some((
                format!(
                    "claude --bg had no --name; this call adds --name {name}. A harness \
                     that ignores rewritten input runs it unnamed: type --name next time."
                ),
                true,
            ))
        }
        crate::launch_record::UnnamedBg::Warn(note) => Some((note, false)),
    }
}

/// Decision D1: a native launch line an agent types passes the same spawn
/// gate a spawn passes, before its launch record is written. A person typing
/// in a plain terminal runs no hook, so a human launch is never gated. The
/// hook holds nothing across the launch: the verdict is taken and the keys
/// released in the same call. A gate that cannot answer is not a refusal -
/// the hook is foreground-bounded and fails open, never wedging the Bash
/// call on a daemon that is down.
fn gate_native_launch(payload: &Value, cwd: &Path) -> Option<String> {
    let Some(cmd) = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
    else {
        return None;
    };
    if !crate::launch_record::may_launch(cmd) {
        return None;
    }
    // `fno agents spawn` runs the gate itself; gating it here too would
    // double-count the slot and double-queue the lane. The floor setter is
    // the exception: the learned-cap floor is the user's setting, so an
    // agent tool call that carries `--floor` is refused here. A person
    // typing the same line in a shell runs no PreToolUse hook and writes.
    if cmd.contains("agents spawn") {
        return refuse_floor_setter(cmd);
    }
    let launches = crate::launch_record::parse_command(cmd, &cwd.to_string_lossy());
    if launches.is_empty() {
        return None;
    }
    if let Some(reason) = crate::launch_record::claude_bg_refusal(&launches, claude_session_runs) {
        return Some(reason);
    }
    let home = crate::paths::AgentsHome::from_env();
    let caller_session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    for launch in launches {
        // Law D1521: a revival re-seats a row that already held a seat, so
        // it never passes the spawn gate. The real revival harm, a resume of
        // a session that still runs forking a copy, is refused above by
        // claude_bg_refusal. `claude respawn <job>` is the other revival
        // form: it parses as no launch at all, so the loop never sees it.
        if launch.resume.is_some() {
            continue;
        }
        let input = crate::spawn_gate::GateInput {
            name: launch.name.clone().unwrap_or_default(),
            substrate: crate::launch_record::native_substrate_word(&launch.harness).into(),
            flags: crate::spawn_gate::GateFlags {
                force: false,
                no_wait: true,
            },
            route_provider: None,
            node: launch.node.clone(),
            account: None,
            caller_session: caller_session.clone(),
            succession_scope: None,
            holder_pid: Some(std::process::id()),
            seed: None,
            session_phase: None,
            background_job: false,
        };
        let config_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match crate::spawn_gate::run_gate(&config_cwd, &home.registry_json(), input) {
            Ok(mut guard) => guard.release(),
            Err(refusal) => {
                if refusal.exit_code == crate::spawn_gate::EXIT_GATE_UNAVAILABLE {
                    eprintln!("pretooluse-bash: spawn gate unavailable; launch allowed");
                    continue;
                }
                let receipt = refusal.receipt.as_ref();
                let reason = receipt
                    .and_then(|r| r.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let remedy = receipt
                    .and_then(|r| r.get("remedy"))
                    .and_then(Value::as_str)
                    .unwrap_or("check fno agents admit status");
                let remedy = crate::capacity::native_remedy(remedy, receipt);
                let place = crate::spawn_gate_queue::place_note(receipt);
                return Some(format!(
                    "spawn gate refused the native launch ({reason}{place}): {remedy}"
                ));
            }
        }
    }
    None
}

/// A live process holds `id`: its argv names it, or claude's own listing
/// shows a hosted process for it. A daemon-hosted `--bg` session carries no
/// id in its argv, so the listing is the half that sees it.
fn claude_session_runs(id: &str) -> bool {
    crate::census::live_claude_holds_session(id)
        || crate::claude_roster::read_all_agents_union().holds_session(id)
}

/// Not a guard. An allowed command that starts a harness session leaves a
/// launch record, which the child claims at SessionStart: a `claude --bg`
/// child cannot see its launcher any other way (see launch_record.rs).
fn record_launch(payload: &Value, cwd: &Path) {
    let Some(cmd) = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
    else {
        return;
    };
    if !crate::launch_record::may_launch(cmd) {
        return;
    }
    let (ambient_session, harness, _) = crate::claims::ambient_parent_edge();
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(ambient_session);
    let home = crate::paths::AgentsHome::from_env();
    if let Err(error) = crate::launch_record::record_command(
        home.root(),
        cmd,
        &cwd.to_string_lossy(),
        session.as_deref(),
        harness.as_deref(),
        crate::claims::now_ms(),
    ) {
        eprintln!("pretooluse-bash: launch record not written: {error}");
    }
}

/// The learned-cap floor is the user's setting: the hook refuses an agent
/// tool call that runs the setter. The user's own shell line runs no
/// PreToolUse hook, so the same line still works from a person. The scan is
/// a token sweep like every probe here: `--floor` or `--floor=` before the
/// first `--` fence on an `agents spawn` line is the setter. A quote edge
/// flips payload state, so a quoted payload string never trips the flag
/// test.
fn refuse_floor_setter(cmd: &str) -> Option<String> {
    let mut at_spawn = false;
    let mut quoted = false;
    for tok in cmd.split_whitespace() {
        if tok == "--" {
            return None;
        }
        if !quoted && at_spawn && (tok == "--floor" || tok.starts_with("--floor=")) {
            return Some(
                "spawn-gate: the spawn cap floor is the user's setting, so an agent tool call \
                 cannot run it. The user sets it in a shell: fno agents spawn --floor N"
                    .to_string(),
            );
        }
        if tok.matches('"').count() % 2 == 1 {
            quoted = !quoted;
        }
        if tok == "spawn" {
            at_spawn = true;
        }
    }
    None
}

#[cfg(test)]
mod floor_setter_tests {
    use super::refuse_floor_setter;

    #[test]
    fn hook_refuses_a_floor_tool_call_and_passes_the_rest() {
        assert!(refuse_floor_setter("fno agents spawn --floor 8").is_some());
        assert!(refuse_floor_setter("fno agents spawn --floor=8 w1 hi").is_some());
        assert!(refuse_floor_setter("fno agents spawn w1 hi --floor 2").is_some());
        // Past the fence the flag is the harness's, and other lines never
        // refuse.
        assert!(refuse_floor_setter("fno agents spawn -- --floor 3").is_none());
        assert!(refuse_floor_setter("fno agents spawn w1 hi").is_none());
        assert!(refuse_floor_setter("fno agents admit status").is_none());
        assert!(refuse_floor_setter("git status").is_none());
        // A quoted payload string is payload, never the setter.
        assert!(refuse_floor_setter("fno agents spawn w1 \"use --floor N\"").is_none());
        assert!(refuse_floor_setter("fno agents spawn w1 \"say \\\"hi\\\" --floor x\"").is_none());
    }
}

#[cfg(test)]
mod native_launch_tests {
    use super::gate_native_launch;
    use crate::claims;
    use serde_json::json;
    use std::path::Path;

    /// A pinned footprint probe answers `hold`, so the only refusal the
    /// gate can reach in this sandbox is the CPU axis's.
    const HOLD_PAYLOAD: &str = r#"{"admission":{"verdict":"hold","axis":"fleet_cpu_share","reason":"fleet 62.5% over 50.0%","bound":"exact","share_low":0.625,"share_high":0.625,"fleet_cores":7.5,"machine_cores":7.5,"capacity_cores":12.0,"ceiling":0.5,"gap":null}}"#;

    fn hook_payload(cmd: &str) -> serde_json::Value {
        json!({
            "session_id": "hook-native-launch-test",
            "cwd": "/repo",
            "tool_name": "Bash",
            "tool_input": {"command": cmd}
        })
    }

    /// Law D1521: a revival re-seats a row that already held a seat, so it
    /// never passes the spawn gate. A fresh `claude --bg` still refuses on
    /// fleet_cpu_share (the positive control); a `--resume` of a stopped
    /// session passes - the resume of a RUNNING one is refused earlier, by
    /// claude_bg_refusal; and `claude respawn <job>`, the other revival
    /// form, parses as no launch and never reaches the gate either.
    #[test]
    fn a_revival_skips_the_spawn_gate_a_fresh_launch_does_not() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-hook-revival-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("claims-root")).unwrap();
        let priors = [
            (
                "FNO_CLAIMS_ROOT",
                std::env::var_os("FNO_CLAIMS_ROOT"),
                Some(dir.join("claims-root").into_os_string()),
            ),
            (
                "FNO_AGENTS_HOME",
                std::env::var_os("FNO_AGENTS_HOME"),
                Some(dir.clone().into_os_string()),
            ),
            ("FNO_SPAWN_GATE", std::env::var_os("FNO_SPAWN_GATE"), None),
            (
                "FNO_TEST_FOOTPRINT_PAYLOAD",
                std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD"),
                Some(HOLD_PAYLOAD.into()),
            ),
        ];
        for (key, _, value) in &priors {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }

        let fresh = gate_native_launch(
            &hook_payload("claude --bg --name w1 \"go\""),
            Path::new("/repo"),
        );
        let revival = gate_native_launch(
            &hook_payload(
                "claude --bg --name Linnea --resume 8bb54c80-a111-4a1b-839a-14ee77c0f52d \"Lead Linnea: revival\"",
            ),
            Path::new("/repo"),
        );
        let respawn =
            gate_native_launch(&hook_payload("claude respawn t-linnea"), Path::new("/repo"));

        for (key, prior, _) in &priors {
            match prior {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        let reason = fresh.expect("a fresh launch must still refuse under a CPU hold");
        assert!(reason.contains("fleet_cpu_share"), "{reason}");
        assert_eq!(
            revival, None,
            "law D1521: a revival never passes the spawn gate"
        );
        assert_eq!(
            respawn, None,
            "claude respawn is the other revival form: it never passes the gate either"
        );
    }
}
