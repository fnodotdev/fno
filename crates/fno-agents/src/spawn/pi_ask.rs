//! The headless lane of pi and of its fork omp: one `--print --mode json`
//! turn run to completion.
//!
//! STATELESS like the opencode and agy one-shots: `--no-session` keeps the
//! harness from writing a session file, no session id is minted, and no
//! registry row is created. pi's route (`--model`, `--provider`,
//! `--thinking`) is the one [`crate::pi::pi_route`] already builds for the
//! keeper and pane lanes. omp reads its route from its own settings unless
//! the caller names `--model` / `--thinking`, and the two harnesses print the
//! same `turn_end` / `agent_end` events, so one parser reads both.
//!
//! pi exits 0 when the model call fails. The failure is only in the event
//! stream: the final assistant message carries `stopReason: "error"` and an
//! `errorMessage`. The exit code alone would report a dead worker as a clean
//! reply, so [`parse_pi_stream`] reads the stream and the exit code is the
//! second check.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::opencode_ask::{AskOutcome, DEFAULT_ONE_SHOT_TIMEOUT};

use super::name_collision::{self as spawn_collision, Resolution};

/// A footnote command seed (`/fno:command`, either sigil) is a command, so it rides
/// bare; a prose message keeps the `[from:]` envelope.
fn pi_prompt(message: &str, from_name: &str, harness: &str) -> String {
    let seed = crate::provider::render_command_seed(
        if message.is_empty() { "hello" } else { message },
        harness,
    );
    let first = seed.split_whitespace().next().unwrap_or("");
    if crate::provider::parse_command_token(first).is_some() {
        seed
    } else {
        format!("[from: {from_name}]\n\n{seed}")
    }
}

/// `pi --print --mode json --no-session <route> <fenced args> <prompt>`.
fn build_pi_argv(
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    harness_args: &[String],
) -> Vec<String> {
    let route = crate::pi::pi_route(model.unwrap_or(""), effort.unwrap_or(""), "", "");
    let mut argv: Vec<String> = ["pi", "--print", "--mode", "json", "--no-session"]
        .iter()
        .map(|t| t.to_string())
        .collect();
    argv.extend(route.tokens);
    argv.extend(harness_args.iter().cloned());
    // argv-fence: exempt. Probed on pi 0.84.2: `pi -- --version` PRINTS the
    // version, so `--` is not an end-of-options marker and a fence would be
    // decoration (see `pi_run_tail` in provider.rs). `--print` is a boolean
    // flag, so there is no equal-form either. The prompt is never
    // flag-shaped: `pi_prompt` makes it start with `[from:` or a command sigil.
    argv.push(prompt.to_string());
    argv
}

/// `omp --print --mode json --no-session [--model m] [--thinking e] <fenced
/// args> -- <prompt>`. Unlike pi, omp's parser treats `--` as the POSIX
/// positional separator (cli/args.ts), so the prompt rides behind a real fence.
fn build_omp_argv(
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    harness_args: &[String],
) -> Vec<String> {
    let mut argv = crate::harness_capabilities::render_session_argv("omp", "headless_create", None)
        .expect("embedded omp headless-create capability");
    argv.push("--no-session".to_string());
    for (flag, value) in [("--model", model), ("--thinking", effort)] {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            argv.push(flag.to_string());
            argv.push(value.to_string());
        }
    }
    argv.extend(harness_args.iter().cloned());
    argv.push("--".to_string());
    argv.push(prompt.to_string());
    argv
}

/// The reply text of the last assistant message in a `--mode json` stream, or
/// the model's own error when that message stopped on one.
fn parse_pi_stream(harness: &str, stdout: &str) -> Result<String, String> {
    let mut last: Option<Value> = None;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let assistant = |m: &Value| m.get("role").and_then(Value::as_str) == Some("assistant");
        match event.get("type").and_then(Value::as_str) {
            Some("turn_end") => {
                if let Some(m) = event.get("message").filter(|m| assistant(m)) {
                    last = Some(m.clone());
                }
            }
            Some("agent_end") => {
                let found = event
                    .get("messages")
                    .and_then(Value::as_array)
                    .and_then(|ms| ms.iter().rev().find(|m| assistant(m)));
                if let Some(m) = found {
                    last = Some(m.clone());
                }
            }
            _ => {}
        }
    }
    let message = last.ok_or_else(|| format!("{harness} emitted no assistant message"))?;
    let stop = message
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("");
    // pi's stop reasons are stop | length | toolUse | error | aborted | deferred.
    // Only `stop` is a finished reply: `length` is a cut-off answer.
    if stop != "stop" {
        let detail = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or("the turn did not end on stop");
        return Err(format!("{harness} stopped with {stop}: {detail}"));
    }
    let text: String = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    match text.trim() {
        "" => Err(format!("{harness} ended its turn with no text")),
        reply => Ok(reply.to_string()),
    }
}

/// Orchestrate one `spawn --substrate headless --harness pi|omp`: validate,
/// check the registry (fail-closed) for a name collision, then run the turn.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_pi_once(
    harness: &str,
    home: &crate::paths::AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    model: Option<&str>,
    effort: Option<&str>,
    harness_args: &[String],
) -> AskOutcome {
    use crate::claude_ask::{emit_event, validate_spawn_inputs};

    if let Err(msg) = validate_spawn_inputs(name, from_name) {
        return AskOutcome::err(msg, 2);
    }
    let events = home.events_jsonl();
    let registry = match crate::state::load_registry(&home.registry_json()) {
        Ok(r) => r,
        Err(e) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "registry-read".into()),
                    ("name", name.into()),
                    ("provider", harness.into()),
                    ("error", e.to_string().into()),
                ],
            );
            return AskOutcome::err(format!("registry read failed: {e}"), 12);
        }
    };
    match spawn_collision::resolve(home, &registry, name) {
        Resolution::Refuse(line) => return AskOutcome::err(line, 2),
        Resolution::Registry(line) => return AskOutcome::err(line, 12),
        _ => {}
    }

    let prompt = pi_prompt(message, from_name, harness);
    let argv = if harness == "omp" {
        build_omp_argv(&prompt, model, effort, harness_args)
    } else {
        build_pi_argv(&prompt, model, effort, harness_args)
    };
    let log_path = home
        .root()
        .join("agents")
        .join("logs")
        .join(format!("{name}.jsonl"));
    let timeout = timeout.or(Some(DEFAULT_ONE_SHOT_TIMEOUT));
    // The row is opt-in (`fno agents spawn --mux`); without it the run keeps
    // no row and stamps none.
    let row = (std::env::var(crate::claude_ask::SHOW_ROW_ENV)
        .ok()
        .as_deref()
        == Some("1"))
    .then_some(home);
    let run = crate::opencode_ask::run_one_shot(harness, &argv, &log_path, timeout, cwd, name, row)
        .and_then(|stdout| parse_pi_stream(harness, &stdout).map_err(|msg| (1, msg)));
    match run {
        Ok(reply) => AskOutcome::ok_reply(format!("{reply}\n")),
        Err((code, msg)) => {
            emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", format!("{harness}-once").into()),
                    ("name", name.into()),
                    ("provider", harness.into()),
                    ("error", msg.clone().into()),
                ],
            );
            AskOutcome::err(msg, code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape pi 0.99 prints for a model failure, measured 2026-10-16: exit
    // 0, empty content, and the cause only in `stopReason` + `errorMessage`.
    const ERROR_STREAM: &str = concat!(
        r#"{"type":"agent_start"}"#,
        "\n",
        r#"{"type":"turn_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Codex error: model not supported"}}"#,
        "\n",
        r#"{"type":"agent_end","messages":[{"role":"user","content":[{"type":"text","text":"Reply pong"}]},{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Codex error: model not supported"}]}"#,
        "\n",
        r#"{"type":"agent_settled"}"#,
    );

    #[test]
    fn a_model_error_exiting_zero_is_not_a_reply() {
        let err = parse_pi_stream("pi", ERROR_STREAM).unwrap_err();
        assert_eq!(
            err,
            "pi stopped with error: Codex error: model not supported"
        );
        let cut_off = r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"par"}],"stopReason":"length"}}"#;
        assert_eq!(
            parse_pi_stream("pi", cut_off).unwrap_err(),
            "pi stopped with length: the turn did not end on stop"
        );
    }

    #[test]
    fn the_reply_is_the_last_assistant_text_across_tool_turns() {
        let stream = concat!(
            r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"toolCall","name":"read"}],"stopReason":"toolUse"}}"#,
            "\n",
            r#"{"type":"turn_end","message":{"role":"assistant","content":[{"type":"text","text":"po"},{"type":"thinking","thinking":"x"},{"type":"text","text":"ng\n"}],"stopReason":"stop"}}"#,
        );
        assert_eq!(parse_pi_stream("pi", stream).as_deref(), Ok("pong"));
    }
}
