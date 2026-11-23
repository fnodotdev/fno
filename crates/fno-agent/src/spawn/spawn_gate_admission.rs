//! The machine admission boundary of the spawn gate: the runaway brake, and
//! the one caller key every arm here shares. The census and the process
//! ceiling live in the fno crate's own agent-spawn door (this crate never
//! links fno; it shells the binary at runtime), so a `fno`-front invocation
//! carries all three arms and a door this crate answers carries the brake.

use crate::spawn_gate::{Refusal, EXIT_FLEET_STOP};

/// True when this process carries a worker identity: the agent-spawn doors'
/// one caller key. FNO_AGENT_SELF names a spawned worker. A claude thread
/// worker carries none (the supervisor strips it), so CLAUDECODE=1 with
/// CLAUDE_CODE_SESSION_ATTENDED=0, the harness's own unattended marker,
/// counts too. An attended session, or one without both markers, is the
/// user's own typed command, and no gate may refuse or hold it.
pub(crate) fn gate_agent_origin() -> bool {
    std::env::var_os("FNO_AGENT_SELF").is_some_and(|name| !name.is_empty())
        || (std::env::var_os("CLAUDECODE").is_some_and(|v| v == "1")
            && std::env::var_os("CLAUDE_CODE_SESSION_ATTENDED").is_some_and(|v| v == "0"))
}

/// The machine brake as a gate arm. The default is admit, and only an
/// agent-origin caller is held; the arm attributes load before it arms the
/// brake, so a hold here means fno's own fan-out. The user's pane taps admit
/// through the default door.
pub(crate) fn process_admission_gate() -> Result<(), Refusal> {
    if !gate_agent_origin() {
        return Ok(());
    }
    // The fleet map's takes-work toggle: the user said this machine takes
    // no new agent work, so no load figure needs to agree.
    let home = crate::paths::AgentsHome::from_env();
    if !crate::fleet_beacon::takes_work(&home) {
        return Err(Refusal::code(EXIT_FLEET_STOP)
            .ev("reason", serde_json::json!("machine-takes-no-work"))
            .ev(
                "detail",
                serde_json::json!(format!(
                    "this machine takes no new agent work; `fno agent fleet top --machines --takes-work on` clears {}",
                    crate::fleet_beacon::takes_work_path(&home).display()
                )),
            ));
    }
    match crate::machine_watch::brake_holds() {
        None => Ok(()),
        Some(hold) => {
            // One line: the verdict line carries the hold (reason, seconds
            // left) as its detail figure; the exit code carries the class.
            Err(Refusal::code(EXIT_FLEET_STOP)
                .ev("reason", serde_json::json!("machine-runaway"))
                .ev("detail", serde_json::json!(hold)))
        }
    }
}

/// The answering side of an `ask --answer` call, resolved once at the
/// dispatch boundary so the answer doors take it as a parameter and tests
/// build it directly. The grant admits on positive proof only: `human_proof`
/// holds when [`crate::claims::attended_human_proof`] passes, and absence of
/// agent markers is not proof, because an agent that stripped its markers
/// reads the same as a user shell.
#[derive(Debug, PartialEq)]
pub(crate) struct AnswerCaller {
    /// Some for a caller carrying a worker marker (FNO_AGENT_SELF, or the
    /// claude thread markers `gate_agent_origin` reads when the supervisor
    /// stripped the name); the value is the row or session name.
    pub(crate) agent_name: Option<String>,
    /// The attended-human proof: no harness in this call's ancestry and
    /// stdin is a terminal.
    pub(crate) human_proof: bool,
}

impl AnswerCaller {
    /// Test-only constructors: production resolves the caller once at the
    /// dispatch boundary with [`AnswerCaller::from_env`].
    #[cfg(test)]
    pub(crate) fn agent(name: &str) -> Self {
        Self {
            agent_name: Some(name.to_string()),
            human_proof: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn human() -> Self {
        Self {
            agent_name: None,
            human_proof: true,
        }
    }

    /// Neither a worker marker nor human proof: the grant refuses it.
    #[cfg(test)]
    pub(crate) fn unproven() -> Self {
        Self {
            agent_name: None,
            human_proof: false,
        }
    }

    pub(crate) fn from_env() -> Self {
        let agent_name = std::env::var_os("FNO_AGENT_SELF")
            .map(|value| value.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                if gate_agent_origin() {
                    Some(
                        std::env::var("CLAUDE_CODE_SESSION_ID")
                            .ok()
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "claude-thread-worker".to_string()),
                    )
                } else {
                    None
                }
            });
        let human_proof = agent_name.is_none() && crate::claims::attended_human_proof().is_ok();
        Self {
            agent_name,
            human_proof,
        }
    }

    /// The stamp the answered event records: the worker row name for a
    /// worker, `user` only on the positive proof, else `unknown`.
    pub(crate) fn stamp(&self) -> String {
        if let Some(name) = &self.agent_name {
            return name.clone();
        }
        if self.human_proof {
            return "user".to_string();
        }
        "unknown".to_string()
    }

    /// The refusal for a caller without the human proof asking an answer
    /// door to grant. None when the caller carries the positive human proof
    /// or the answer denies: deny is the conservative direction, and the
    /// proven caller is the user's own typed command.
    pub(crate) fn allow_refusal(&self, answer: &str) -> Option<String> {
        if !answer_grants(answer) || self.human_proof {
            return None;
        }
        Some(format!(
            "ask --answer: only the user at their terminal grants a permission answer (caller: {}, refused answer: {}); a person answers it at the owning session",
            self.stamp(),
            answer,
        ))
    }
}

/// True when the answer grants. The contract's vocabulary is allow_once,
/// allow_always, deny, matched exact; the lowercased allow prefix is the
/// grant class, so casing cannot dodge the door.
pub(crate) fn answer_grants(answer: &str) -> bool {
    answer.to_ascii_lowercase().starts_with("allow")
}

/// Fixtures that exercise the agent-fan-out refusal arms carry the worker
/// identity those arms key on; removed on drop, before the env lock.
#[cfg(test)]
pub(crate) struct AgentSelfFixture;
#[cfg(test)]
impl AgentSelfFixture {
    pub(crate) fn set() -> Self {
        std::env::set_var("FNO_AGENT_SELF", "gate-fixture-worker");
        AgentSelfFixture
    }
}
#[cfg(test)]
impl Drop for AgentSelfFixture {
    fn drop(&mut self) {
        std::env::remove_var("FNO_AGENT_SELF");
    }
}

#[cfg(test)]
mod answer_caller_tests {
    use super::*;
    use crate::claims::EnvVarGuard;

    #[test]
    fn the_grant_admits_only_the_proven_human_and_every_deny() {
        let agent = AnswerCaller::agent("w-fix");
        let unproven = AnswerCaller::unproven();
        let human = AnswerCaller::human();
        let refusal = agent.allow_refusal("allow_once").expect("refused");
        assert!(refusal.contains("only the user"), "{refusal}");
        assert!(refusal.contains("w-fix"), "{refusal}");
        assert!(unproven.allow_refusal("allow_once").is_some());
        assert!(human.allow_refusal("allow_once").is_none());
        // Deny stays open for every caller, and casing cannot dodge the
        // gate: the prefix match is case-insensitive.
        assert!(agent.allow_refusal("deny").is_none());
        assert!(unproven.allow_refusal("deny").is_none());
        assert!(human.allow_refusal("deny").is_none());
        assert!(agent.allow_refusal("ALLOW_ALWAYS").is_some());
    }

    #[test]
    fn the_stamp_names_worker_user_on_proof_and_unknown() {
        assert_eq!(AnswerCaller::agent("w-fix").stamp(), "w-fix");
        assert_eq!(AnswerCaller::human().stamp(), "user");
        assert_eq!(AnswerCaller::unproven().stamp(), "unknown");
    }

    #[test]
    fn from_env_resolves_the_worker_name() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _agent = AgentSelfFixture::set();
        let caller = AnswerCaller::from_env();
        assert_eq!(caller.agent_name.as_deref(), Some("gate-fixture-worker"));
    }

    #[test]
    fn from_env_demands_the_human_proof_for_the_user_stamp() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _pid = EnvVarGuard::unset("FNO_SESSION_PID");
        let _harness = EnvVarGuard::unset("FNO_SESSION_HARNESS");
        let _self = EnvVarGuard::unset("FNO_AGENT_SELF");
        let _attended = EnvVarGuard::unset("CLAUDE_CODE_SESSION_ATTENDED");
        let ppid: u32 = unsafe { libc::getppid() } as u32;
        crate::claims::set_test_stdin_tty(Some(true));
        let _tty = crate::claims::proof_tests::TtyGuard;
        // The proven-user arm: a plain shell chain and a terminal.
        let _plain = crate::claims::proof_tests::CensusGuard::chain(&[
            (std::process::id(), ppid, "fno-agent ask w --answer deny"),
            (ppid, 1, "-zsh"),
            (1, 0, "launchd"),
        ]);
        let proven = AnswerCaller::from_env();
        // The headless arm: the same chain without a terminal.
        crate::claims::set_test_stdin_tty(Some(false));
        let headless = AnswerCaller::from_env();
        drop(_plain);
        // The held arm: a harness sits at ppid, where the walk starts.
        let _held = crate::claims::proof_tests::CensusGuard::chain(&[
            (std::process::id(), ppid, "fno-agent ask w --answer deny"),
            (ppid, 1, "claude --resume /t/lead.jsonl --name lead"),
            (1, 0, "launchd"),
        ]);
        crate::claims::set_test_stdin_tty(Some(true));
        let held = AnswerCaller::from_env();
        assert!(
            proven.human_proof,
            "a plain chain and a tty read as the user"
        );
        assert_eq!(proven.stamp(), "user");
        assert!(!headless.human_proof, "no tty, no proof");
        assert_eq!(headless.stamp(), "unknown");
        assert!(held.allow_refusal("allow_once").is_some());
        assert!(!held.human_proof, "a harness at ppid is not the user");
    }
}
