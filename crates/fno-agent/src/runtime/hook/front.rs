//! `fno-agent hook front <Event> [--leg <name> [args...]]` - every claude
//! hook of one event in one process.
//!
//! hooks.json once registered one command per leg: a prompt started eleven
//! hook processes, a tool call three more, a session start five, each a bash
//! plus usually an fno-agent or jq. This entry runs the same legs in
//! process, each on its own thread under its former registered timeout (as
//! Claude ran them, in parallel), and prints one merged answer.
//!
//! Fail modes match the parallel hooks: a leg that answers nonzero, panics
//! or runs past its timeout is skipped with one stderr line, and the other
//! legs still answer. A late leg's output is dropped: it was captured on its
//! own thread and is never printed. Merge per event: UserPromptSubmit and
//! SessionStart join `additionalContext` in table order (plain stdout is
//! context there); a `decision: block` outranks context. Stop joins every
//! block reason into one block at exit 0. PostToolUse joins context.
//!
//! `--leg <name>` runs one leg alone and prints its own answer: the scripts
//! codex, pi and opencode still run by name are one-line shims into it.
//!
//! A leg never prints with `println!`: it answers through
//! [`super::emit_line`] so the capture on its thread holds the text, and a
//! child process it starts never inherits stdout.

mod common;
mod post_tool;
mod session_start;
mod stop;
mod user_prompt;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) use common::*;

/// One former hooks.json leg: its name (the script stem), its registered
/// timeout, and the entry that answers it.
pub(crate) struct Leg {
    pub name: &'static str,
    pub timeout: Duration,
    pub run: fn(&[String]) -> i32,
}

impl Leg {
    pub const fn new(name: &'static str, secs: u64, run: fn(&[String]) -> i32) -> Self {
        Leg {
            name,
            timeout: Duration::from_secs(secs),
            run,
        }
    }
}

/// Claude's own default for a command hook with no `timeout` (600 s on
/// every event this front serves but UserPromptSubmit, whose legs all carry
/// a registered one).
pub(crate) const CLAUDE_DEFAULT_SECS: u64 = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Event {
    UserPromptSubmit,
    Stop,
    SessionStart,
    PostToolUse,
}

impl Event {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "UserPromptSubmit" => Some(Event::UserPromptSubmit),
            "Stop" => Some(Event::Stop),
            "SessionStart" => Some(Event::SessionStart),
            "PostToolUse" => Some(Event::PostToolUse),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Event::UserPromptSubmit => "UserPromptSubmit",
            Event::Stop => "Stop",
            Event::SessionStart => "SessionStart",
            Event::PostToolUse => "PostToolUse",
        }
    }

    /// The event's legs, in the former hooks.json order.
    fn legs(self) -> Vec<Leg> {
        match self {
            Event::UserPromptSubmit => user_prompt::legs(),
            Event::Stop => stop::legs(),
            Event::SessionStart => session_start::legs(),
            Event::PostToolUse => post_tool::legs(),
        }
    }
}

const USAGE: &str = "usage: fno-agent hook front <UserPromptSubmit|Stop|SessionStart|PostToolUse> [--leg <name> [args...]]";

pub fn run(args: &[String]) -> i32 {
    let Some(event) = args.first().and_then(|e| Event::parse(e)) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let raw = super::read_stdin();
    match args.get(1).map(String::as_str) {
        None => {
            super::emit_line(&run_event(event, &raw).to_string());
            0
        }
        Some("--leg") => {
            let Some(name) = args.get(2) else {
                eprintln!("{USAGE}");
                return 2;
            };
            run_one(event, name, args.get(3..).unwrap_or_default(), &raw)
        }
        Some(other) => {
            eprintln!("fno-agent hook front: unknown argument {other:?}; {USAGE}");
            2
        }
    }
}

/// `--leg <name>`: one leg alone, its own answer and exit code. The name is
/// looked up across every event's table, so a codex PostCompact shim can
/// name a SessionStart leg.
fn run_one(event: Event, name: &str, rest: &[String], raw: &str) -> i32 {
    let tables = [
        event,
        Event::UserPromptSubmit,
        Event::Stop,
        Event::SessionStart,
        Event::PostToolUse,
    ];
    let Some(leg) = tables
        .iter()
        .flat_map(|e| e.legs())
        .find(|leg| leg.name == name)
    else {
        eprintln!("fno-agent hook front: no leg {name:?}");
        return 2;
    };
    let (code, out) = super::captured(raw, || (leg.run)(rest));
    if !out.is_empty() {
        print!("{out}");
    }
    code
}

/// Every leg of `event` on its own thread under its timeout, merged.
pub(crate) fn run_event(event: Event, raw: &str) -> Value {
    run_legs(event, event.legs(), raw)
}

fn run_legs(event: Event, legs: Vec<Leg>, raw: &str) -> Value {
    let started = Instant::now();
    let mut waits = Vec::with_capacity(legs.len());
    for leg in legs {
        let (tx, rx) = mpsc::channel::<Option<(i32, String)>>();
        let payload = raw.to_string();
        let entry = leg.run;
        let spawned = std::thread::Builder::new()
            .name(format!("hook-front-{}", leg.name))
            .spawn(move || {
                let answer = std::panic::catch_unwind(|| super::captured(&payload, || entry(&[])));
                // The receiver has gone once the bound fired.
                tx.send(answer.ok()).ok();
            });
        match spawned {
            Ok(_) => waits.push((leg.name, leg.timeout, rx)),
            Err(e) => eprintln!("hook front: {} not started: {e}; skipped", leg.name),
        }
    }
    let mut answers: Vec<String> = Vec::new();
    for (name, timeout, rx) in waits {
        let left = timeout.saturating_sub(started.elapsed());
        match rx.recv_timeout(left) {
            Ok(Some((0, out))) => answers.push(out),
            Ok(Some((code, _))) => eprintln!("hook front: {name} answered exit {code}; skipped"),
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("hook front: {name} panicked; skipped")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!(
                    "hook front: {name} ran past its {}s timeout; skipped",
                    timeout.as_secs()
                )
            }
        }
    }
    merge(event, &answers)
}

/// One leg answer, in any shape a former hook printed.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Answer {
    decision: Option<String>,
    reason: Option<String>,
    system_message: Option<String>,
    #[serde(rename = "continue")]
    keep_going: Option<bool>,
    stop_reason: Option<String>,
    hook_specific_output: Option<Specific>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Specific {
    additional_context: Option<String>,
}

/// Split one leg's stdout into JSON answers and plain text. A whole-output
/// JSON object (a pretty-printed one spans lines) is one answer; otherwise
/// each line is an answer when it parses as an object, and plain text when
/// it does not.
fn split_answer(out: &str) -> (Vec<Answer>, Vec<String>) {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
        let answer = serde_json::from_value(Value::Object(map)).unwrap_or_default();
        return (vec![answer], Vec::new());
    }
    let mut answers = Vec::new();
    let mut plain = Vec::new();
    for line in trimmed.lines() {
        match serde_json::from_str::<Value>(line.trim()) {
            Ok(Value::Object(map)) => {
                answers.push(serde_json::from_value(Value::Object(map)).unwrap_or_default())
            }
            _ => plain.push(line.to_string()),
        }
    }
    let text = plain.join("\n").trim().to_string();
    (
        answers,
        if text.is_empty() {
            Vec::new()
        } else {
            vec![text]
        },
    )
}

/// Fold every leg's answer into the one answer for `event`.
pub(crate) fn merge(event: Event, outs: &[String]) -> Value {
    let mut blocks: Vec<String> = Vec::new();
    let mut context: Vec<String> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut halt: Option<String> = None;
    // Plain stdout reaches the model on these two events only.
    let plain_is_context = matches!(event, Event::UserPromptSubmit | Event::SessionStart);
    for out in outs {
        let (answers, plain) = split_answer(out);
        for answer in answers {
            if answer.decision.as_deref() == Some("block") {
                blocks.push(answer.reason.unwrap_or_else(|| "blocked".to_string()));
            }
            if answer.keep_going == Some(false) && halt.is_none() {
                halt = Some(answer.stop_reason.unwrap_or_default());
            }
            context.extend(
                answer
                    .hook_specific_output
                    .and_then(|s| s.additional_context)
                    .filter(|c| !c.is_empty()),
            );
            system.extend(answer.system_message.filter(|s| !s.is_empty()));
        }
        if plain_is_context {
            context.extend(plain);
        }
    }

    let mut out = Map::new();
    if !blocks.is_empty() {
        out.insert("decision".into(), json!("block"));
        out.insert("reason".into(), json!(blocks.join("\n\n")));
    }
    if let Some(reason) = halt {
        out.insert("continue".into(), json!(false));
        if !reason.is_empty() {
            out.insert("stopReason".into(), json!(reason));
        }
    }
    if !context.is_empty() && event != Event::Stop {
        out.insert(
            "hookSpecificOutput".into(),
            json!({"hookEventName": event.name(), "additionalContext": context.join("\n\n")}),
        );
    }
    if !system.is_empty() {
        out.insert("systemMessage".into(), json!(system.join("\n")));
    }
    Value::Object(out)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // asserts read JSON answers by key
mod tests;
