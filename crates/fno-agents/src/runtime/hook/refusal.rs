//! One refusal shape for every fno PreToolUse guard.
//!
//! Claude shows a hook deny as its own permission denial, so a bare reason
//! reads as the harness refusing and leaves the person no way out. Every
//! refusal here starts `[fno <guard>]`, then names a `Way out:` (the safe
//! form of the same intent) and an `Override:` (how the person runs it
//! anyway). [`Guard::contract`] holds both lines; its match is exhaustive,
//! so a new guard without them does not compile.
//!
//! fno owns no permission layer. On a Claude session a person attends, a
//! stop returns `ask`, so the harness's own prompt is the person's key to
//! run the call once. Everywhere else it returns `deny` with the same text:
//! codex parses `ask` but runs the call anyway (its hooks docs: "parsed but
//! not supported yet ... continues the tool call"), and an unattended worker
//! has nobody to answer a prompt, so `ask` would wedge it.

use crate::harness::{AsHarness, Harness};
use serde_json::json;

/// Every fno guard that can stop a call or leave a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guard {
    BgProcess,
    BinInstall,
    BroadKill,
    GitProtection,
    BacklogImpostor,
    RecursiveGrep,
    TestRun,
    Effect,
    RefusalStreak,
    SpawnGate,
    GraphWrite,
    ConfigWrite,
    GeneratedWrite,
    Lead,
    Law,
    JoinPartition,
    SendMessage,
    SubagentWorktree,
}

const PRESET_STRICT: &str = "this guard runs only under `guards.preset = \"strict\"`; set `guards.preset = \"standard\"` in .fno/config.toml to stop it.";
const PRESET_OFF: &str =
    "set `guards.preset = \"off\"` in .fno/config.toml to stop every gatable fno guard.";
const OWN_SHELL: &str = "a person runs the same line in their own shell, where no fno hook runs.";

impl Guard {
    #[cfg(test)]
    const ALL: [Guard; 18] = [
        Guard::BgProcess,
        Guard::BroadKill,
        Guard::BinInstall,
        Guard::GitProtection,
        Guard::BacklogImpostor,
        Guard::RecursiveGrep,
        Guard::TestRun,
        Guard::Effect,
        Guard::RefusalStreak,
        Guard::SpawnGate,
        Guard::GraphWrite,
        Guard::ConfigWrite,
        Guard::GeneratedWrite,
        Guard::Lead,
        Guard::Law,
        Guard::JoinPartition,
        Guard::SendMessage,
        Guard::SubagentWorktree,
    ];

    /// The name inside `[fno <tag>]`.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Guard::BgProcess => "bg-process guard",
            Guard::BinInstall => "bin-install guard",
            Guard::BroadKill => "broad-kill guard",
            Guard::GitProtection => "git-protection",
            Guard::BacklogImpostor => "backlog-impostor guard",
            Guard::RecursiveGrep => "recursive-grep guard",
            Guard::TestRun => "test-run guard",
            Guard::Effect => "effect guard",
            Guard::RefusalStreak => "refusal-streak",
            Guard::SpawnGate => "spawn gate",
            Guard::GraphWrite => "graph-write-protect",
            Guard::ConfigWrite => "claude-config-write-guard",
            Guard::GeneratedWrite => "generated-write-guard",
            Guard::Lead => "lead guard",
            Guard::Law => "law",
            Guard::JoinPartition => "join-partition guard",
            Guard::SendMessage => "send-message guard",
            Guard::SubagentWorktree => "subagent-worktree guard",
        }
    }

    /// The `Way out:` and `Override:` lines.
    fn contract(self) -> (&'static str, &'static str) {
        match self {
            Guard::BgProcess => ("give the command a bound: `timeout <secs> <cmd>`, or `bash scripts/lib/loadgen.sh start <label> <seconds>` for load.", PRESET_STRICT),
            Guard::BinInstall => ("pin your build with `export FNO_AGENTS_FRONT=\"$PWD/crates/fno-agents/target/debug/fno-agents\"`; deploy with `fno doctor update`.", PRESET_STRICT),
            Guard::BroadKill => (
                "stop only what you started: `kill <pid>`, or `kill -- -<pgid>` for the process group of a command you launched; measure fleet load with `fno doctor footprint`.",
                PRESET_OFF,
            ),
            Guard::GitProtection => ("push and merge through `fno do pr push` and `fno do pr merge`; the reason above names the form for this command.", PRESET_OFF),
            Guard::BacklogImpostor => ("prefix the command: `fno backlog ...`.", PRESET_OFF),
            Guard::RecursiveGrep => ("use `rg <pattern> <path>` or the harness Grep tool; both honor .gitignore.", PRESET_STRICT),
            Guard::TestRun => ("run `fno doctor test <changed test files>`, or push and let CI run the suite.", "prefix `FNO_TEST_FULL=1` for a whole-suite run; this guard runs only under `guards.preset = \"strict\"`."),
            Guard::Effect => ("retry the same call unchanged once its request is approved.", "when the reason names a request digest, approve it with `fno inbox approvals decide <digest> --as <principal> --approve`; a core-policy deny has no override."),
            Guard::RefusalStreak => ("file the blocker (`fno backlog idea \"...\"`) or mail your lead, then move to other work.", "change the command, or wait 10 minutes for the streak to expire."),
            Guard::SpawnGate => ("`fno agents admit status` shows the queue; the reason above names the remedy.", OWN_SHELL),
            Guard::GraphWrite => ("change state through `fno backlog` or `fno do state`; they own these files.", OWN_SHELL),
            Guard::ConfigWrite => ("write scratch in this session's job dir; the reason above names it.", OWN_SHELL),
            Guard::GeneratedWrite => ("edit the source file the reason names, then run its regen command.", OWN_SHELL),
            Guard::Lead => ("leave source edits to the session that owns the work; a lead writes plans, notes and mail.", "set `lead.implementation_guard = \"warn\"` (or `\"off\"`) in .fno/config.toml."),
            Guard::Law => ("the rule above names what it wants instead.", "the person who owns the rule edits or retires it with `/fno:law`."),
            Guard::JoinPartition => ("edit only the files your band owns; the reason above names the band.", "the join lead widens the band policy under `.fno/join-partition/`."),
            Guard::SendMessage => ("send with `fno agents mail send <to> \"<message>\"`.", "SendMessage opens again by itself while fno mail is unhealthy."),
            Guard::SubagentWorktree => ("use absolute paths: Read, Edit and Write take them, and git runs as `git -C <path> ...`.", "call EnterWorktree from the top-level session instead of the subagent."),
        }
    }
}

/// One guard's refusal: the guard and its own reason text.
pub(crate) struct Refusal {
    guard: Guard,
    why: String,
}

impl Refusal {
    pub(crate) fn new(guard: Guard, why: impl Into<String>) -> Self {
        Self {
            guard,
            why: why.into(),
        }
    }

    pub(crate) fn guard(&self) -> &'static str {
        self.guard.tag()
    }

    /// `[fno <guard>] <why>`, then the `Way out:` and `Override:` lines. A
    /// reason that already carries an `[fno ...]` tag keeps one tag.
    pub(crate) fn render(&self) -> String {
        let why = strip_tag(self.why.trim());
        let (way_out, override_hint) = self.guard.contract();
        format!(
            "[fno {}] {why}\nWay out: {way_out}\nOverride: {override_hint}",
            self.guard.tag()
        )
    }
}

fn strip_tag(why: &str) -> &str {
    if why.starts_with("[fno ") {
        if let Some(end) = why.find("] ") {
            return why.get(end + 2..).unwrap_or(why);
        }
    }
    why
}

/// True when the calling harness shows a PreToolUse `ask` to a person who
/// can answer it: a Claude session marked attended that is no fno worker.
pub(crate) fn harness_asks() -> bool {
    asks_with(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

fn asks_with(env: impl Fn(&str) -> Option<String>) -> bool {
    let foreign = [
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "FNO_AGENT_SELF",
    ]
    .iter()
    .any(|k| env(k).is_some());
    // The agy and pi guard host runs these guards with the wire's name set.
    let hosted = env("FNO_HOOK_HARNESS").is_some_and(|h| h.as_harness() != Harness::Claude);
    let claude = env("CLAUDECODE").as_deref() == Some("1") || env("CLAUDE_PLUGIN_ROOT").is_some();
    // Attendance must be positive: a prompt nobody answers wedges a worker,
    // while a deny only costs one retry.
    let attended = env("CLAUDE_CODE_SESSION_ATTENDED").as_deref() == Some("1");
    claude && attended && !hosted && !foreign
}

/// Print one PreToolUse answer for these refusals, notices riding along,
/// and say whether it was an `ask`. Always exit 0.
pub(crate) fn emit(refusals: &[Refusal], notices: &[String]) -> (i32, bool) {
    let ask = harness_asks();
    let mut parts: Vec<String> = refusals.iter().map(Refusal::render).collect();
    parts.extend(notices.iter().cloned());
    let reason = parts.join("\n\n");
    super::emit_line(&answer(&reason, ask).to_string());
    (0, ask)
}

/// `emit` for a single refusal.
pub(crate) fn emit_one(refusal: Refusal) -> i32 {
    emit(&[refusal], &[]).0
}

fn answer(reason: &str, ask: bool) -> serde_json::Value {
    if ask {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": reason
            }
        })
    } else {
        json!({
            "decision": "block",
            "reason": reason,
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every guard renders one tag, a way out and an override, and only an
    /// attended Claude session that is no fno worker gets `ask`.
    #[test]
    fn every_guard_renders_tag_way_out_and_override_and_only_attended_claude_asks() {
        for g in Guard::ALL {
            let (way_out, override_hint) = g.contract();
            assert!(!way_out.trim().is_empty(), "{}: empty way out", g.tag());
            assert!(
                !override_hint.trim().is_empty(),
                "{}: empty override",
                g.tag()
            );
            let text = Refusal::new(g, format!("[fno {}] why it stopped", g.tag())).render();
            let head = format!("[fno {}] why it stopped\nWay out: ", g.tag());
            assert!(text.starts_with(&head), "{}: {text}", g.tag());
            assert!(text.contains("\nOverride: "), "{}: {text}", g.tag());
        }

        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        const ATT: (&str, &str) = ("CLAUDE_CODE_SESSION_ATTENDED", "1");
        assert!(asks_with(env(&[("CLAUDECODE", "1"), ATT])));
        assert!(asks_with(env(&[("CLAUDE_PLUGIN_ROOT", "/p"), ATT])));
        assert!(
            !asks_with(env(&[("CLAUDECODE", "1")])),
            "attendance unknown"
        );
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ATTENDED", "0")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ATT,
            ("FNO_AGENT_SELF", "w1")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ATT,
            ("CODEX_THREAD_ID", "t")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDE_PLUGIN_ROOT", "/p"),
            ATT,
            ("FNO_HOOK_HARNESS", "pi")
        ])));
        assert!(!asks_with(env(&[ATT])));
        let ask = answer("r", true);
        assert_eq!(
            ask.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&json!("ask"))
        );
        assert!(ask.pointer("/decision").is_none());
        let deny = answer("r", false);
        assert_eq!(
            deny.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&json!("deny"))
        );
        assert_eq!(deny.pointer("/decision"), Some(&json!("block")));
    }
}
