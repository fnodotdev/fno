//! One refusal shape for every fno PreToolUse guard.
//!
//! Claude shows a hook deny as its own permission denial, so a bare reason
//! reads as the harness refusing and leaves the person no way out. Every
//! refusal here starts `[fno <guard>]`, then names a `Way out:` (the safe
//! form of the same intent) and an `Override:` (how the person runs it
//! anyway). [`CONTRACTS`] holds both lines per guard; a guard missing from
//! it fails the render test, not a user's turn.
//!
//! fno owns no permission layer. On a Claude session a person attends, a
//! stop returns `ask`, so the harness's own prompt is the person's key to
//! run the call once. Everywhere else it returns `deny` with the same text:
//! codex parses `ask` but runs the call anyway (its hooks docs: "parsed but
//! not supported yet ... continues the tool call"), and an unattended worker
//! has nobody to answer a prompt, so `ask` would wedge it.

use serde_json::json;

/// The way out and the override for one guard tag.
pub(crate) struct Contract {
    pub guard: &'static str,
    pub way_out: &'static str,
    pub override_hint: &'static str,
}

const PRESET_STRICT: &str = "this guard runs only under `guards.preset = \"strict\"`; set `guards.preset = \"standard\"` in .fno/config.toml to stop it.";
const PRESET_OFF: &str =
    "set `guards.preset = \"off\"` in .fno/config.toml to stop every gatable fno guard.";
const OWN_SHELL: &str = "a person runs the same line in their own shell, where no fno hook runs.";

pub(crate) const CONTRACTS: &[Contract] = &[
    Contract {
        guard: "bg-process guard",
        way_out: "give the command a bound: `timeout <secs> <cmd>`, or `bash scripts/lib/loadgen.sh start <label> <seconds>` for load.",
        override_hint: PRESET_STRICT,
    },
    Contract {
        guard: "bin-install guard",
        way_out: "pin your build with `export FNO_AGENTS_FRONT=\"$PWD/crates/fno-agents/target/debug/fno-agents\"`; deploy with `fno doctor update`.",
        override_hint: PRESET_STRICT,
    },
    Contract {
        guard: "git-protection",
        way_out: "push and merge through `fno do pr push` and `fno do pr merge`; the reason above names the form for this command.",
        override_hint: PRESET_OFF,
    },
    Contract {
        guard: "backlog-impostor guard",
        way_out: "prefix the command: `fno backlog ...`.",
        override_hint: "run the third-party CLI by its full path, `~/.bun/bin/backlog`, or set `guards.preset = \"off\"` in .fno/config.toml.",
    },
    Contract {
        guard: "recursive-grep guard",
        way_out: "use `rg <pattern> <path>` or the harness Grep tool; both honor .gitignore.",
        override_hint: PRESET_STRICT,
    },
    Contract {
        guard: "test-run guard",
        way_out: "run `fno doctor test <changed test files>`, or push and let CI run the suite.",
        override_hint: "prefix `FNO_TEST_FULL=1` for a whole-suite run; this guard runs only under `guards.preset = \"strict\"`.",
    },
    Contract {
        guard: "effect guard",
        way_out: "the reason above names the request digest; retry the same call unchanged once it is approved.",
        override_hint: "approve it with `fno inbox approvals decide <digest> --as <principal> --approve`.",
    },
    Contract {
        guard: "refusal-streak",
        way_out: "file the blocker (`fno backlog idea \"...\"`) or mail your lead, then move to other work.",
        override_hint: "change the command, or wait 10 minutes for the streak to expire.",
    },
    Contract {
        guard: "spawn gate",
        way_out: "`fno agents admit status` shows the queue; the reason above names the remedy.",
        override_hint: OWN_SHELL,
    },
    Contract {
        guard: "graph-write-protect",
        way_out: "change state through `fno backlog` or `fno do state`; they own these files.",
        override_hint: OWN_SHELL,
    },
    Contract {
        guard: "generated-write-guard",
        way_out: "edit the source file the reason names, then run its regen command.",
        override_hint: OWN_SHELL,
    },
    Contract {
        guard: "lead guard",
        way_out: "hand the edit to a worker (`fno agents spawn`) or a subagent.",
        override_hint: "set `lead.implementation_guard = \"warn\"` (or `\"off\"`) in .fno/config.toml.",
    },
    Contract {
        guard: "law",
        way_out: "the rule above names what it wants instead.",
        override_hint: "the person who owns the rule edits or retires it with `/fno:law`.",
    },
    Contract {
        guard: "join-partition guard",
        way_out: "edit only the files your band owns; the reason above names the band.",
        override_hint: "the join lead widens the band policy under `.fno/join-partition/`.",
    },
    Contract {
        guard: "send-message guard",
        way_out: "send with `fno agents mail send <to> \"<message>\"`.",
        override_hint: "SendMessage opens again by itself while fno mail is unhealthy.",
    },
    Contract {
        guard: "subagent-worktree guard",
        way_out: "use absolute paths: Read, Edit and Write take them, and git runs as `git -C <path> ...`.",
        override_hint: "call EnterWorktree from the top-level session instead of the subagent.",
    },
];

/// One guard's refusal: the tag and the guard's own reason text.
pub(crate) struct Refusal {
    guard: &'static str,
    why: String,
}

impl Refusal {
    pub(crate) fn new(guard: &'static str, why: impl Into<String>) -> Self {
        Self {
            guard,
            why: why.into(),
        }
    }

    pub(crate) fn guard(&self) -> &'static str {
        self.guard
    }

    fn contract(&self) -> Option<&'static Contract> {
        CONTRACTS.iter().find(|c| c.guard == self.guard)
    }

    /// `[fno <guard>] <why>`, then the `Way out:` and `Override:` lines. A
    /// reason that already carries an `[fno ...]` tag keeps one tag.
    pub(crate) fn render(&self) -> String {
        let why = strip_tag(self.why.trim());
        match self.contract() {
            Some(c) => format!(
                "[fno {}] {why}\nWay out: {}\nOverride: {}",
                c.guard, c.way_out, c.override_hint
            ),
            None => format!("[fno {}] {why}", self.guard),
        }
    }
}

fn strip_tag(why: &str) -> &str {
    if why.starts_with("[fno ") {
        if let Some(end) = why.find("] ") {
            return &why[end + 2..];
        }
    }
    why
}

/// True when the calling harness shows a PreToolUse `ask` to a person who
/// can answer it: a Claude session that is attended and is no fno worker.
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
    let hosted = env("FNO_HOOK_HARNESS").is_some_and(|h| h != "claude");
    env("CLAUDECODE").as_deref() == Some("1")
        && !hosted
        && env("CLAUDE_CODE_SESSION_ATTENDED").as_deref() != Some("0")
        && !foreign
}

/// Print one PreToolUse answer for these refusals, notices riding along,
/// and say whether it was an `ask`. Always exit 0.
pub(crate) fn emit(refusals: &[Refusal], notices: &[String]) -> (i32, bool) {
    let ask = harness_asks();
    let mut parts: Vec<String> = refusals.iter().map(Refusal::render).collect();
    parts.extend(notices.iter().cloned());
    let reason = parts.join("\n\n");
    println!("{}", answer(&reason, ask));
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

    /// Every guard that stops a call renders its tag, a way out and an
    /// override; an empty line in the table fails here, naming the guard.
    #[test]
    fn every_guard_renders_tag_way_out_and_override() {
        for c in CONTRACTS {
            assert!(!c.way_out.trim().is_empty(), "{}: empty way_out", c.guard);
            assert!(
                !c.override_hint.trim().is_empty(),
                "{}: empty override_hint",
                c.guard
            );
            let text = Refusal::new(c.guard, format!("[fno {}] why it stopped", c.guard)).render();
            assert!(
                text.starts_with(&format!("[fno {}] why it stopped\n", c.guard)),
                "{}: one tag, then the reason: {text}",
                c.guard
            );
            assert!(text.contains("\nWay out: "), "{}: {text}", c.guard);
            assert!(text.contains("\nOverride: "), "{}: {text}", c.guard);
        }
        // Every tag a guard passes is in the table.
        let source = [
            include_str!("pretooluse_bash.rs"),
            include_str!("write_gate.rs"),
            include_str!("lead_guard.rs"),
            include_str!("rules.rs"),
            include_str!("join_partition_guard.rs"),
            include_str!("send_message_guard.rs"),
            include_str!("subagent_worktree_guard.rs"),
            include_str!("bg_process_guard.rs"),
            include_str!("bin_install_guard.rs"),
            include_str!("backlog_impostor.rs"),
            include_str!("recursive_grep_guard.rs"),
            include_str!("test_run_guard.rs"),
            include_str!("git_protection/mod.rs"),
            include_str!("../../gate/effect_gate.rs"),
        ]
        .concat();
        let used = regex::Regex::new(r#"Refusal::new\(\s*"([^"]+)""#).unwrap();
        for caps in used.captures_iter(&source) {
            assert!(
                CONTRACTS.iter().any(|c| c.guard == &caps[1]),
                "{}: no contract row",
                &caps[1]
            );
        }
        // And every row names a guard some caller passes.
        for c in CONTRACTS {
            assert!(
                source.contains(&format!("\"{}\"", c.guard)),
                "{}: no caller",
                c.guard
            );
        }

        // Only an attended Claude session that is no fno worker gets `ask`.
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        assert!(asks_with(env(&[("CLAUDECODE", "1")])));
        assert!(asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ATTENDED", "1")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ATTENDED", "0")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("FNO_AGENT_SELF", "w1")
        ])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("CODEX_THREAD_ID", "t")
        ])));
        assert!(!asks_with(env(&[("CODEX_THREAD_ID", "t")])));
        assert!(!asks_with(env(&[
            ("CLAUDECODE", "1"),
            ("FNO_HOOK_HARNESS", "pi")
        ])));
        assert!(!asks_with(env(&[])));
        assert_eq!(
            answer("r", true)["hookSpecificOutput"]["permissionDecision"],
            "ask"
        );
        assert!(answer("r", true).get("decision").is_none());
        assert_eq!(
            answer("r", false)["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        assert_eq!(answer("r", false)["decision"], "block");
    }
}
