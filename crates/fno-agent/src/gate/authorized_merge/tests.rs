use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct Fake {
    architect: Option<Blocker>,
    architect_finished: Cell<bool>,
    revoke_merge: bool,
    late_checks: Option<String>,
    late_pr_hold: Option<ProbeOutcome>,
    late_decline: Option<PathBuf>,
    /// Every look request the gate routed, as (pr, detail): a paint PR that
    /// merged with no recorded look must owe the lead exactly one.
    look_requests: RefCell<Vec<(u64, String)>>,
    facts: Option<PrFacts>,
    facts_after_effect: Option<PrFacts>,
    facts_error: Option<String>,
    /// Answer for the binding probe alone; `None` (the default) reads Clear.
    node_binding: Option<ProbeOutcome>,
    dispatch_hold: Option<ProbeOutcome>,
    outside_pr: Option<ProbeOutcome>,
    pr_hold: Option<ProbeOutcome>,
    merge_freeze: Option<ProbeOutcome>,
    rules: Option<ProbeOutcome>,
    /// Per-PR rules answer, so a test can hold one PR and clear the next.
    other_rules: RefCell<HashMap<u64, ProbeOutcome>>,
    review_posted: Option<ProbeOutcome>,
    /// The `reviewed_head` argument review_posted_gate last passed in, so a
    /// test can prove it threaded the attestation's own head, not a guess.
    review_posted_head_seen: RefCell<Option<String>>,
    review_hold: Option<ProbeOutcome>,
    lineage: Option<ProbeOutcome>,
    merge_result: Option<ProbeOutcome>,
    ci_base: Option<ProbeOutcome>,
    fresh_ci: Option<bool>,
    live_lanes: usize,
    ci_base_calls: RefCell<u32>,
    checks: Option<String>,
    /// GitHub's ruleset hold for the PR under decision; `None` (the
    /// default) reads no hold, so every pre-existing test keeps its behavior.
    github_block: Option<String>,
    /// Optional-review answer for the PR under decision; the default
    /// `Some(Some(0))` keeps every pre-existing test passing.
    optional_unresolved: Option<Option<i64>>,
    /// Rerun-recovery answer for the PR under decision.
    rerun_recovered: Option<bool>,
    /// The evals gate row for the PR under decision; `None` (the default)
    /// reads no gate row, so every pre-existing test keeps its behavior.
    evals_gate_row: Option<EvalsGateRead>,
    /// The expectation probe's answer; `None` (the default) reads false, so
    /// a head with no gate row merges unless a test seats the expectation.
    evals_gate_expected: Option<bool>,
    /// The coverage verdict's exit for the PR under decision, armed into
    /// `coverage_verdict`; None reads 0.
    coverage_exit: Option<i32>,
    /// The `fno backlog decisions` exit and stdout the head-grant read
    /// gets. `None` exit reads 0; `None` stdout reads empty (malformed).
    decisions_exit: Option<i32>,
    decisions_stdout: Option<Vec<u8>>,
    /// Every fno-shell argv, so a test can assert the subject the grant
    /// read asked for.
    fno_calls: RefCell<Vec<Vec<String>>>,
    covered_head: Option<String>,
    /// The merge queue's answer; `None` (the default) reads no queue.
    merge_queue: Option<Result<Option<String>, String>>,
    enabled: bool,
    floor: Option<String>,
    gh_ok: bool,
    gh_output: String,
    /// Answer for the REST recovery call alone, so a test can fail the
    /// `gh pr merge` and let the retry succeed.
    gh_recovery_ok: Option<bool>,
    /// Per-endpoint answers: the first key the joined argv contains wins,
    /// ahead of `gh_output`.
    gh_routes: Vec<(String, String)>,
    gh_calls: RefCell<Vec<Vec<String>>>,
    /// Simulated merge-slot claim: `None` is free, `Some(pr)` is held.
    slot: RefCell<Option<u64>>,
    slot_holder_err: bool,
    take_slot_err: bool,
    take_slot_calls: RefCell<Vec<u64>>,
    release_slot_calls: RefCell<Vec<u64>>,
    /// Simulated merge-slot metadata: has `mark_slot_retested` fired since
    /// the slot was last taken (mirrors the real claim's `metadata`).
    slot_retested: RefCell<bool>,
    /// Facts and checks for a PR other than the one under decision, keyed
    /// by PR number - a holder read in the slot logic.
    other_facts: RefCell<HashMap<u64, PrFacts>>,
    other_checks: RefCell<HashMap<u64, String>>,
    /// Dispatch holds keyed by PR, so a test can hold a holder without
    /// holding the PR under decision.
    other_holds: RefCell<HashMap<u64, ProbeOutcome>>,
    /// The main-ci verdict the gate reads; `None` (the default) reads the
    /// `pending` word, so every pre-existing test keeps its behavior.
    main_ci: Option<Result<Value, String>>,
    /// The main-repair exemption answer; `None` (the default) reads exempt.
    main_repair: Option<Option<String>>,
}

fn open_facts() -> PrFacts {
    PrFacts {
        number: 7,
        head_sha: "abc123".to_string(),
        head_ref: "feature/x".to_string(),
        base_ref: "main".to_string(),
        url: "https://github.com/o/r/pull/7".to_string(),
        body: Some("Backlog-Closure: x-aaaa\n".to_string()),
        state: "OPEN".to_string(),
        armed: false,
    }
}

fn clean() -> Fake {
    Fake {
        facts: Some(open_facts()),
        covered_head: Some("abc123".to_string()),
        enabled: true,
        gh_ok: true,
        optional_unresolved: Some(Some(0)),
        ..Default::default()
    }
}

struct ClaimsRootRestore(Option<std::ffi::OsString>);

impl Drop for ClaimsRootRestore {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
            None => std::env::remove_var("FNO_CLAIMS_ROOT"),
        }
    }
}

fn with_claims_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    let _env_lock = claims::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let restore = ClaimsRootRestore(std::env::var_os("FNO_CLAIMS_ROOT"));
    std::env::set_var("FNO_CLAIMS_ROOT", root);
    let result = f();
    drop(restore);
    result
}

/// code+class for each scenario: no attestation (stays silent, the gap
/// belongs to review_coverage_uncovered), an attestation with GitHub
/// lacking the post (Held, self-clearing), posted (clear), an unreadable
/// probe (Unknown, never a silent pass), and a round carried forward to a
/// new head with no fresh attestation event of its own (still found,
/// branch-wide, never silenced by an exact-head miss).
#[test]
fn review_posted_gate_covers_its_five_cases() {
    let temp = tempfile::TempDir::new().unwrap();
    with_claims_root(temp.path(), || {
        let spaces = temp.path().join("spaces");
        std::fs::create_dir_all(&spaces).unwrap();
        let saved = std::env::var("FNO_SPACES_DIR").ok();
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let facts = PrFacts {
            number: 931,
            head_sha: "h1sha".to_string(),
            head_ref: "feature/x".to_string(),
            base_ref: "main".to_string(),
            url: "https://github.com/o/r/pull/931".to_string(),
            body: None,
            state: "OPEN".to_string(),
            armed: false,
        };
        let gate = |outcome: ProbeOutcome| {
            crate::merge_gates::review_posted_gate(
                &Fake {
                    review_posted: Some(outcome),
                    ..Default::default()
                },
                &repo,
                &facts,
            )
        };

        assert_eq!(
            gate(ProbeOutcome::Refused("must not be asked".into())),
            None
        );

        let journal = crate::paths::events_path(&repo);
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(
            &journal,
            "{\"type\":\"review_attestation\",\"ts\":\"2026-10-16T10:00:00Z\",\"data\":{\"branch\":\"feature/x\",\"head_sha\":\"h1sha\",\"verdict\":\"pass\",\"reviewer\":\"code-review\"}}\n",
        )
        .unwrap();

        let blocker = gate(ProbeOutcome::Refused("not yet posted".into())).unwrap();
        assert_eq!(blocker.code, "review_not_posted");
        assert_eq!(blocker.class, BlockerClass::Held);

        assert_eq!(gate(ProbeOutcome::Clear), None);

        let blocker = gate(ProbeOutcome::Inconclusive("gh timed out".into())).unwrap();
        assert_eq!(blocker.code, "review_posted_unknown");
        assert_eq!(blocker.class, BlockerClass::Unknown);

        // A round carried forward under law d-608344c1 (rebase, interdiff
        // under the cap) never gets a fresh attestation EVENT at the new
        // head - only the original head carries one. The gate must still
        // find it (branch-wide, not exact-head) and probe for THAT head's
        // marker, never facts.head_sha.
        std::fs::write(
            &journal,
            "{\"type\":\"review_attestation\",\"ts\":\"2026-10-16T11:00:00Z\",\"data\":{\"branch\":\"feature/x\",\"head_sha\":\"h0sha-pre-rebase\",\"verdict\":\"pass\",\"reviewer\":\"code-review\"}}\n",
        )
        .unwrap();
        let fake = Fake {
            review_posted: Some(ProbeOutcome::Refused("not yet posted".into())),
            ..Default::default()
        };
        let blocker = crate::merge_gates::review_posted_gate(&fake, &repo, &facts)
            .expect("a carried-forward round with no posted review must still block");
        assert_eq!(blocker.code, "review_not_posted");
        assert_eq!(
            fake.review_posted_head_seen.borrow().as_deref(),
            Some("h0sha-pre-rebase")
        );

        match saved {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
    });
}

impl Probes for Fake {
    fn architect_evidence(
        &self,
        _cwd: &Path,
        facts: &PrFacts,
        run: bool,
        _warnings: &[Warning],
    ) -> Option<Blocker> {
        if run {
            self.architect_finished.set(true);
            if let Some(page) = &self.late_decline {
                std::fs::write(page, format!("---\nquestion_id: q-late\nstatus: answered\nask: May PR {} merge at {}?\nanswer: Hold for changes\n---\n", facts.number, facts.head_sha)).unwrap();
            }
        }
        self.architect.clone()
    }
    fn pr_facts(&self, _cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
        if let Some(error) = &self.facts_error {
            return Err(error.clone());
        }
        let main = self.facts.clone().ok_or_else(|| "no facts".to_string())?;
        if (pr.is_none() || pr == Some(main.number))
            && self
                .gh_calls
                .borrow()
                .iter()
                .any(|args| args.get(1).map(String::as_str) == Some("merge"))
        {
            if let Some(after) = &self.facts_after_effect {
                return Ok(after.clone());
            }
        }
        match pr {
            Some(n) if n != main.number => self
                .other_facts
                .borrow()
                .get(&n)
                .cloned()
                .ok_or_else(|| format!("no facts for pr {n}")),
            _ => Ok(main),
        }
    }
    fn node_binding(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.node_binding.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn dispatch_hold(&self, _cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
        if let Some(hold) = self.other_holds.borrow().get(&facts.number) {
            return hold.clone();
        }
        self.dispatch_hold.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn outside_pr(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.outside_pr.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn pr_hold(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.late_pr_hold
            .clone()
            .filter(|_| self.architect_finished.get())
            .or_else(|| self.pr_hold.clone())
            .unwrap_or(ProbeOutcome::Clear)
    }
    fn merge_freeze(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.merge_freeze.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn rules(&self, _cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
        if let Some(ruling) = self.other_rules.borrow().get(&facts.number) {
            return ruling.clone();
        }
        self.rules.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn review_posted(&self, _cwd: &Path, _facts: &PrFacts, reviewed_head: &str) -> ProbeOutcome {
        *self.review_posted_head_seen.borrow_mut() = Some(reviewed_head.to_string());
        self.review_posted.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn review_hold(&self, _cwd: &Path, _pr: u64) -> ProbeOutcome {
        self.review_hold.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn base_lineage(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.lineage.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn merge_result(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.merge_result.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn ci_base(&self, _cwd: &Path, _facts: &PrFacts, merge_clean: bool) -> ProbeOutcome {
        *self.ci_base_calls.borrow_mut() += 1;
        if merge_clean {
            return ProbeOutcome::Clear;
        }
        self.ci_base.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn require_fresh_ci(&self, _cwd: &Path) -> bool {
        self.fresh_ci.unwrap_or(true)
    }
    fn live_lanes(&self, _cwd: &Path) -> usize {
        self.live_lanes
    }
    fn slot_holder(&self, _cwd: &Path, _base_ref: &str) -> Result<Option<u64>, String> {
        if self.slot_holder_err {
            return Err("merge slot claim corrupted".to_string());
        }
        Ok(*self.slot.borrow())
    }
    fn take_slot(&self, _cwd: &Path, _base_ref: &str, pr: u64) -> Result<(), String> {
        self.take_slot_calls.borrow_mut().push(pr);
        if self.take_slot_err {
            return Err("merge slot already held by pr:99".to_string());
        }
        *self.slot.borrow_mut() = Some(pr);
        *self.slot_retested.borrow_mut() = false;
        Ok(())
    }
    fn release_slot(&self, _cwd: &Path, _base_ref: &str, pr: u64) {
        self.release_slot_calls.borrow_mut().push(pr);
        let mut held = self.slot.borrow_mut();
        if *held == Some(pr) {
            *held = None;
            *self.slot_retested.borrow_mut() = false;
        }
    }
    fn look_request(&self, _cwd: &Path, facts: &PrFacts, detail: &str) -> bool {
        self.look_requests
            .borrow_mut()
            .push((facts.number, detail.to_string()));
        true
    }
    fn slot_retested(&self, _cwd: &Path, _base_ref: &str, pr: u64) -> bool {
        *self.slot.borrow() == Some(pr) && *self.slot_retested.borrow()
    }
    fn mark_slot_retested(&self, _cwd: &Path, _base_ref: &str, _pr: u64) {
        *self.slot_retested.borrow_mut() = true;
    }
    fn checks_read(&self, _cwd: &Path, pr: u64) -> ChecksRead {
        let mine = self.facts.as_ref().map(|f| f.number) == Some(pr);
        let verdict = if mine {
            self.late_checks
                .clone()
                .filter(|_| self.architect_finished.get())
                .or_else(|| self.checks.clone())
                .unwrap_or_else(|| "green".to_string())
        } else {
            self.other_checks
                .borrow()
                .get(&pr)
                .cloned()
                .unwrap_or_else(|| "green".to_string())
        };
        ChecksRead {
            verdict,
            github_block: if mine {
                self.github_block.clone()
            } else {
                None
            },
            optional_unresolved: if mine {
                self.optional_unresolved
            } else {
                Some(Some(0))
            },
            rerun_recovered: if mine { self.rerun_recovered } else { None },
            rerun_failures: None,
            evals_gate_row: if mine {
                self.evals_gate_row.clone()
            } else {
                None
            },
            unmeasured: None,
        }
    }

    fn evals_gate_expected(&self, _cwd: &Path, _facts: &PrFacts) -> bool {
        self.evals_gate_expected.unwrap_or(false)
    }
    fn covered_head(&self, _cwd: &Path, _pr: u64, _pr_head: &str) -> Option<String> {
        self.covered_head.clone()
    }
    fn auto_merge_enabled(&self, _cwd: &Path) -> bool {
        self.enabled && !(self.revoke_merge && self.architect_finished.get())
    }
    fn posture_floor_block(&self, _cwd: &Path) -> Option<String> {
        self.floor.clone()
    }
    fn strategy(&self, _cwd: &Path) -> String {
        "squash".to_string()
    }
    fn main_ci_token(&self, _cwd: &Path) -> Result<Value, String> {
        self.main_ci
            .clone()
            .unwrap_or_else(|| Ok(Value::String("pending".to_string())))
    }
    fn main_repair_hold(&self, _cwd: &Path, _facts: &PrFacts) -> Option<String> {
        self.main_repair.clone().flatten()
    }
    fn merge_queue(
        &self,
        _cwd: &Path,
        _facts: &PrFacts,
        _head: &str,
    ) -> Option<Result<Option<String>, String>> {
        self.merge_queue.clone()
    }
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String), String> {
        self.gh_calls.borrow_mut().push(args.to_vec());
        if args.first().map(String::as_str) == Some("api") {
            if let Some(ok) = self.gh_recovery_ok {
                return Ok((ok, String::new()));
            }
        }
        let joined = args.join(" ");
        if let Some((_, out)) = self
            .gh_routes
            .iter()
            .find(|(k, _)| joined.contains(k.as_str()))
        {
            return Ok((true, out.clone()));
        }
        Ok((self.gh_ok, self.gh_output.clone()))
    }
    fn fno_shell(
        &self,
        _cwd: &Path,
        args: &[String],
    ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
        self.fno_calls.borrow_mut().push(args.to_vec());
        // The head-grant ask: `backlog decisions <subject> --lane law
        // --state live --json`.
        if args.len() >= 2 && args[0] == "backlog" && args[1] == "decisions" {
            return Ok((
                self.decisions_exit.or(Some(0)),
                self.decisions_stdout.clone().unwrap_or_default(),
                Vec::new(),
            ));
        }
        // The plan-fidelity gate computes in process now (its Python leg
        // retired), so no fidelity ask reaches this shell any more. Whatever
        // else arrives is a done probe: answer clean.
        Ok((Some(0), br#"{}"#.to_vec(), Vec::new()))
    }

    fn coverage_verdict(
        &self,
        _cwd: &Path,
        _pr: u64,
    ) -> crate::loopcheck::coverage_gate::GateVerdict {
        use crate::loopcheck::coverage_gate::{GateState, GateVerdict};
        let (state, refusal, note) = match self.coverage_exit {
            None | Some(0) => (GateState::Covered, "", ""),
            Some(3) => (
                GateState::Refused,
                "no review coverage row at this head",
                "",
            ),
            _ => (GateState::Unanswered, "", "coverage read unavailable"),
        };
        GateVerdict {
            state,
            refusal: refusal.to_string(),
            covered_head: String::new(),
            note: note.to_string(),
        }
    }
}

fn request(effect: Effect) -> Request {
    Request {
        cwd: PathBuf::from("/tmp"),
        pr: Some(7),
        effect,
        approved: Some(true),
        auto_merge_source: Some("config".to_string()),
        require_checks: false,
        covered_head: None,
        decide_only: false,
        authority: None,
        accept_flake: false,
        supplied_verdict: None,
        supplied_counts: None,
        supplied_rerun_recovered: None,
        supplied_optional_unresolved: None,
        supplied_github_blockers: None,
        supplied_dispatch_hold: None,
        supplied_review_hold: None,
        supplied_facts: None,
        supplied_evals_gate: None,
        supplied_unmeasured: None,
    }
}

fn granted_decisions() -> Vec<u8> {
    br#"{"decisions":[{"authority_source":"operator","ts":"2026-11-01T00:00:00Z","decision":"merge authorized for this head"}]}"#
        .to_vec()
}

#[test]
fn evals_gate_facts_warn_without_holding_merges_arms_or_previews() {
    for (state, expected, code) in [
        (Some("FAILURE"), false, Some("evals_gate_red")),
        (Some("ERROR"), false, Some("evals_gate_red")),
        (Some("PENDING"), false, Some("evals_gate_pending")),
        (Some("SUCCESS"), true, None),
        (None, true, Some("evals_gate_missing")),
        (None, false, None),
    ] {
        for require_checks in [false, true] {
            for effect in [Effect::Merge, Effect::Arm, Effect::Preview] {
                let fake = Fake {
                    evals_gate_row: state.map(|state| EvalsGateRead {
                        state: state.into(),
                        description: "gate evidence from the daemon".into(),
                    }),
                    evals_gate_expected: Some(expected),
                    ..clean()
                };
                let req = Request {
                    require_checks,
                    ..request(effect)
                };
                let outcome = run(&fake, &req);
                let word = match effect {
                    Effect::Merge => "merged",
                    Effect::Arm => "armed",
                    Effect::Preview => "authorized",
                };
                assert_eq!(
                    outcome.word(),
                    word,
                    "state={state:?}, expected={expected}, effect={effect:?}: {outcome:?}"
                );
                let codes = outcome
                    .warnings()
                    .iter()
                    .map(|w| w.code.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(codes, code.into_iter().collect::<Vec<_>>());
                let receipt = outcome.to_json();
                for warning in outcome.warnings() {
                    assert!(receipt
                        .get("warnings")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&json!({"code": warning.code, "detail": warning.detail})));
                    if state.is_some() {
                        assert!(warning.detail.contains("gate evidence from the daemon"));
                    }
                }
                let preview = preview_walk(
                    &fake,
                    &Request {
                        effect: Effect::Preview,
                        ..req
                    },
                    &open_facts(),
                );
                assert!(matches!(preview, PreviewVerdict::Go { .. }), "{preview:?}");
                assert_eq!(preview.warnings(), outcome.warnings());
                assert_eq!(
                    preview.to_json("abc123").get("warnings"),
                    receipt.get("warnings")
                );
            }
        }
    }
    for (ci, authority, expected) in [
        ("red", None, "failed"),
        ("red", Some("durable_grant"), "held"),
    ] {
        let fake = Fake {
            checks: Some(ci.into()),
            evals_gate_row: Some(EvalsGateRead {
                state: "PENDING".into(),
                description: "gate evidence beside a retained CI gate".into(),
            }),
            ..clean()
        };
        let req = Request {
            require_checks: true,
            authority: authority.map(str::to_string),
            ..request(Effect::Merge)
        };
        let mut observed_head = None;
        let outcome = run_observed(&fake, &req, &mut observed_head);
        assert_eq!(outcome.word(), expected, "{outcome:?}");
        assert_eq!(observed_head.as_deref(), Some("abc123"));
        assert!(outcome.detail().contains(&format!("checks are {ci}")));
        assert_eq!(
            outcome.warnings().len(),
            1,
            "the retained CI gate lost evaluated evidence: {outcome:?}"
        );
        assert_eq!(
            outcome.to_json().pointer("/warnings/0/code").unwrap(),
            "evals_gate_pending"
        );
        assert_eq!(
            decide(&fake, &req).unwrap_err().warnings(),
            outcome.warnings()
        );
        let preview = run(
            &fake,
            &Request {
                effect: Effect::Preview,
                ..req
            },
        );
        assert_eq!(preview.word(), "held");
        assert_eq!(preview.warnings(), outcome.warnings());
        assert!(!fake
            .gh_calls
            .borrow()
            .iter()
            .any(|args| args.get(1).map(String::as_str) == Some("merge")));
    }
}

/// PR 247 merged with clippy still running because the status door, its cache
/// lock busy, answered `unknown` while its counts still said 22 checks were
/// unsettled. A degraded read that names running checks is a pending read; a
/// degraded read that names none (no CI at all) keeps its fall-through.
#[test]
fn a_degraded_unknown_read_with_running_checks_holds_every_door() {
    let degraded = parse_checks_read(
        br#"{"verdict": "unknown", "ready_blockers": ["status_lock_busy"], "checks": {"pending": 22, "unsettled": 22}}"#,
    );
    assert_eq!(degraded.verdict, "pending");
    let fake = Fake {
        checks: Some(degraded.verdict),
        ..clean()
    };
    for effect in [Effect::Merge, Effect::Arm] {
        let mut req = request(effect);
        req.require_checks = true;
        let outcome = run(&fake, &req);
        assert_eq!(outcome.word(), "held", "{effect:?}: {outcome:?}");
    }

    // PR 251: clippy failed and 33 siblings were cancelled when it merged.
    let failed = parse_checks_read(
        br#"{"verdict": "unknown", "ready_blockers": ["status_stale"], "checks": {"fail": 1, "pending": 33, "unsettled": 33}}"#,
    );
    assert_eq!(failed.verdict, "red");
    let fake = Fake {
        checks: Some(failed.verdict),
        ..clean()
    };
    for effect in [Effect::Merge, Effect::Arm] {
        for require_checks in [true, false] {
            let outcome = run(
                &fake,
                &Request {
                    require_checks,
                    ..request(effect)
                },
            );
            assert!(
                !matches!(outcome.word(), "merged" | "armed"),
                "{effect:?}: {outcome:?}"
            );
        }
    }

    let no_ci =
        parse_checks_read(br#"{"verdict": "unknown", "checks": {"pending": 0, "unsettled": 0}}"#);
    assert_eq!(no_ci.verdict, "unknown");
}

// ── the head-scoped operator merge grant ──────────────────────────────

#[test]
fn a_head_grant_clears_the_per_run_no_merge_and_the_receipt_names_it() {
    // AC2-HP.
    let mut req = request(Effect::Merge);
    req.approved = Some(false);
    let fake = Fake {
        decisions_stdout: Some(granted_decisions()),
        ..clean()
    };
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "merged", "{}", outcome.detail());
    match &outcome {
        Outcome::Merged { merge_grant, .. } => {
            assert_eq!(merge_grant.as_deref(), Some("user head grant abc123"))
        }
        other => panic!("expected a merged receipt, got {other:?}"),
    }
    assert!(fake
        .fno_calls
        .borrow()
        .iter()
        .any(|args| args.get(2).map(String::as_str) == Some("merge-grant:o/r#7@abc123")));
    let missing_repo = Fake {
        facts: Some(PrFacts {
            url: String::new(),
            ..open_facts()
        }),
        decisions_stdout: Some(granted_decisions()),
        ..clean()
    };
    let outcome = run(&missing_repo, &req);
    assert_eq!(outcome.word(), "refused");
    assert!(outcome
        .detail()
        .contains("repository identity is unreadable"));
    assert!(missing_repo.fno_calls.borrow().is_empty());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".fno")).unwrap();
    let spaces = tempfile::tempdir().unwrap();
    let _env_lock = crate::pr_status::cache_env_lock();
    crate::paths::pin_test_claims_root(spaces.path());
    struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("FNO_SPACES_DIR", value),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.1.take() {
                Some(value) => std::env::set_var("FNO_HOME", value),
                None => std::env::remove_var("FNO_HOME"),
            }
        }
    }
    let _env = RestoreEnv(
        std::env::var_os("FNO_SPACES_DIR"),
        std::env::var_os("FNO_HOME"),
    );
    std::env::set_var("FNO_SPACES_DIR", spaces.path());
    std::env::set_var("FNO_HOME", tmp.path());
    crate::graph_store::seed_rows(
        &crate::org_board::scope::graph_json_path(tmp.path()),
        &[json!({
            "id": "x-visual",
            "status": "in_review",
            "pr_number": 7,
            "pr_url": "https://github.com/o/r/pull/7"
        })],
    )
    .unwrap();

    let head = "a".repeat(40);
    let mut fake = clean();
    fake.facts = Some(PrFacts {
        head_sha: head.clone(),
        body: Some("Docs: fno-web#1\n".to_string()),
        ..open_facts()
    });
    fake.covered_head = Some(head.clone());
    fake.decisions_stdout = Some(
        json!({
            "decisions": [{
                "subject": "x-visual",
                "authority_source": "role",
                "lifecycle": "unscoped",
                "decision": format!("visual approval PR 7 at {head}: approved"),
                "rationale": "User tapped Approve on the Visual Log (viewer u_test)"
            }]
        })
        .to_string()
        .into_bytes(),
    );
    assert!(!crate::merge_gates::visual_tap_clears_pr(
        &fake,
        tmp.path(),
        7,
        &"b".repeat(40),
        "x-visual"
    ));
    let req = Request {
        cwd: tmp.path().to_path_buf(),
        approved: Some(false),
        covered_head: Some(head.clone()),
        ..request(Effect::Merge)
    };

    let visual_outcome = run(&fake, &req);
    assert_eq!(
        visual_outcome.word(),
        "merged",
        "{}",
        visual_outcome.detail()
    );
    let expected_grant = format!(
        "user head grant {}",
        head.chars().take(8).collect::<String>()
    );
    match visual_outcome {
        Outcome::Merged { merge_grant, .. } => {
            assert_eq!(merge_grant.as_deref(), Some(expected_grant.as_str()))
        }
        other => panic!("expected a merged receipt, got {other:?}"),
    }
}

#[test]
fn a_holder_with_fresh_ci_clears_and_releases_the_slot_on_merge_while_a_racer_waits() {
    // AC2-HP.
    let mut fake = Fake {
        slot: RefCell::new(Some(7)),
        facts: Some(PrFacts {
            number: 9,
            head_sha: "nine".to_string(),
            ..open_facts()
        }),
        covered_head: Some("nine".to_string()),
        ..clean()
    };
    let req9 = Request {
        require_checks: true,
        pr: Some(9),
        ..request(Effect::Merge)
    };
    let outcome9 = run(&fake, &req9);
    assert_eq!(outcome9.word(), "held");
    let detail9 = outcome9.detail();
    assert!(detail9.contains("merge_slot_held"));
    assert!(detail9.contains("PR 7"));
    assert_eq!(
        *fake.slot.borrow(),
        Some(7),
        "the waiting PR must not take the slot"
    );

    fake.facts = Some(PrFacts {
        number: 7,
        head_sha: "abc123".to_string(),
        ..open_facts()
    });
    fake.covered_head = Some("abc123".to_string());
    let req7 = Request {
        require_checks: true,
        pr: Some(7),
        ..request(Effect::Merge)
    };
    let outcome7 = run(&fake, &req7);
    assert_eq!(
        outcome7.word(),
        "merged",
        "the slot holder with fresh CI clears and merges"
    );
    assert_eq!(
        *fake.slot.borrow(),
        None,
        "the slot releases once the Merged outcome lands"
    );
}

#[test]
fn a_self_held_slot_retests_the_base_at_most_once_then_merges_on_the_next_settle() {
    // The observed bug (2026-10-17): on a base busy enough that a full CI
    // run outlasts the next landing, retesting on every settle restarts
    // CI forever and the slot never converges. One retest per hold; the
    // settle after that merges on the real `gh pr merge` call instead.
    let fake = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;

    let first = run(&fake, &req);
    assert_eq!(first.word(), "held");
    assert!(first.detail().contains("merged main into PR 7's branch"));
    assert_eq!(fake.gh_calls.borrow().len(), 1);

    // Base keeps moving (still ci_base_stale) and the retested head still
    // reads green - the exact shape that looped forever before this fix.
    fake.gh_calls.borrow_mut().clear();
    let second = run(&fake, &req);
    assert_eq!(second.word(), "merged", "{second:?}");
    assert!(
        fake.gh_calls.borrow().iter().all(|c| c[0] != "api"),
        "a second settle must not retest the base again: {:?}",
        fake.gh_calls.borrow()
    );
}

#[test]
fn an_unreadable_body_or_graph_refuses_to_assume_bound() {
    // AC4-ERR: a missing body key is an out-of-date deployed fno, never a
    // bound PR; the remedy names the update.
    let entries = vec![json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno"})];
    let facts = PrFacts {
        body: None,
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Inconclusive(reason) = outcome else {
        unreachable!("a missing body is Inconclusive, never a verdict")
    };
    assert!(reason.contains("fno doctor update"));

    // A repo-scoped PR with no binding key at all is the refusal, with
    // the bind remedy.
    let facts = PrFacts {
        head_ref: "docs/team-succeed-faq".to_string(),
        body: Some(String::new()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Refused(reason) = outcome else {
        unreachable!("three missing keys refuse")
    };
    assert!(reason.contains("closure-trailer"));
    assert!(reason.contains("no flag bypasses this gate"));

    // No comparable url leaves the backref key unevaluable: Unknown, not
    // a refusal on a PR that may be bound through the back-pointer.
    let facts = PrFacts {
        head_ref: "docs/team-succeed-faq".to_string(),
        url: String::new(),
        body: Some(String::new()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Inconclusive(reason) = outcome else {
        unreachable!("an unscopeable backref key is Inconclusive")
    };
    assert!(reason.contains("could not be scoped"));

    // AC4-ERR, retarget: the body hands the branch's node to another
    // node, but the graph still points the old node at this PR; the
    // refusal names both fno backlog update commands.
    let url = "https://github.com/o/r/pull/7";
    let body = "Fixes x-bbbb\nRetarget x-aaaa x-bbbb msg-447f8f".to_string();
    let unmoved = vec![
        json!({"id": "x-aaaa", "cwd": "/this/repo", "project": "fno",
               "pr_number": 7, "pr_url": url}),
        json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno"}),
    ];
    let facts = PrFacts {
        head_ref: "feature/x-aaaa".to_string(),
        body: Some(body.clone()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &unmoved, &facts);
    let ProbeOutcome::Refused(reason) = outcome else {
        unreachable!("an unmoved retarget binding refuses")
    };
    assert!(reason.contains("msg-447f8f"), "{reason}");
    assert!(
        reason.contains("fno backlog update x-aaaa pr_number=null pr_url=null"),
        "{reason}"
    );
    assert!(
        reason.contains("fno backlog update x-bbbb pr_number=7 pr_url"),
        "{reason}"
    );

    // AC4-HP: the graph moved (the old node holds no ref, the new one
    // carries this PR), so the probe is Clear.
    let moved = vec![
        json!({"id": "x-aaaa", "cwd": "/this/repo", "project": "fno"}),
        json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno",
               "pr_number": 7, "pr_url": url}),
    ];
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &moved, &facts);
    assert_eq!(outcome, ProbeOutcome::Clear);
}

#[test]
fn process_conditions_warn_on_merge_arm_and_preview_pending_holds() {
    for code in [
        "review_in_flight",
        "review_coverage_uncovered",
        "posture_floor",
        "ci_pending",
        "ci_error",
        "ci_unknown",
        "architect_unavailable",
        "architect_not_run",
        "architect_granted",
    ] {
        let mut fake = clean();
        match code {
            "review_in_flight" => {
                fake.review_hold = Some(ProbeOutcome::Inconclusive("unreadable review".into()))
            }
            "review_coverage_uncovered" => fake.coverage_exit = Some(3),
            "posture_floor" => fake.floor = Some("below review floor".into()),
            code if code.starts_with("architect_") => {
                fake.architect = Some(Blocker::unknown(code, "reader evidence"))
            }
            _ => fake.checks = Some(code.trim_start_matches("ci_").into()),
        }
        for (effect, word) in [
            (Effect::Merge, "merged"),
            (Effect::Arm, "armed"),
            (Effect::Preview, "authorized"),
        ] {
            let req = Request {
                require_checks: true,
                ..request(effect)
            };
            let outcome = run(&fake, &req);
            if code == "ci_pending" && matches!(effect, Effect::Merge | Effect::Arm) {
                // The cells the warning does not cover: a pending read holds
                // the merge door until the newest attempt settles, and holds
                // the arm because `--auto` waits only on required checks.
                // The 2026-11-10 incident: pending fell through as advisory
                // and main went red on the merged head.
                assert_eq!(outcome.word(), "held", "{code}: {outcome:?}");
                assert!(
                    outcome.detail().contains("checks are pending; "),
                    "{outcome:?}"
                );
                if effect == Effect::Arm {
                    let off = run(
                        &fake,
                        &Request {
                            require_checks: false,
                            ..request(Effect::Arm)
                        },
                    );
                    assert_eq!(off.word(), "held", "{code}: {off:?}");
                    assert!(
                        off.detail().contains("arming auto-merge waits only"),
                        "{off:?}"
                    );
                }
                continue;
            }
            assert_eq!(outcome.word(), word, "{code}: {outcome:?}");
            assert!(outcome.warnings().iter().any(|w| w.code == code));
        }
        let preview = preview_walk(&fake, &preview_request(7), &open_facts());
        assert!(matches!(&preview, PreviewVerdict::Go { .. }), "{preview:?}");
        assert!(preview.warnings().iter().any(|w| w.code == code));
    }
}

#[test]
fn missing_coverage_keeps_the_head_pin_and_unreadable_pins_refuse() {
    for (effect, expected) in [(Effect::Merge, "merged"), (Effect::Arm, "armed")] {
        let fake = Fake {
            covered_head: None,
            coverage_exit: Some(3),
            ..clean()
        };
        let outcome = run(&fake, &request(effect));
        assert_eq!(outcome.word(), expected, "{outcome:?}");
        assert!(outcome
            .warnings()
            .iter()
            .any(|w| w.code == "review_coverage_uncovered"));
        assert!(fake.gh_calls.borrow().iter().any(|args| args
            .windows(2)
            .any(|pair| matches!(pair, [flag, pin] if flag == "--match-head-commit" && pin == "abc123"))));
        for (head, pin) in [("", None), ("abc123", Some(String::new()))] {
            let fake = Fake {
                covered_head: None,
                facts: Some(PrFacts {
                    head_sha: head.into(),
                    ..open_facts()
                }),
                ..clean()
            };
            let req = Request {
                covered_head: pin,
                ..request(effect)
            };
            let outcome = run(&fake, &req);
            assert_eq!(outcome.word(), "unknown", "{outcome:?}");
            assert!(outcome.detail().contains("unpinned"));
            assert!(fake.gh_calls.borrow().is_empty());
        }
    }
}

#[test]
fn an_immediate_merge_never_passes_auto() {
    let journal = crate::merge_provenance::TestJournal::opt_in();
    let fake = clean();
    let outcome = run(&fake, &request(Effect::Merge));
    assert_eq!(
        outcome,
        Outcome::Merged {
            head: "abc123".to_string(),
            note: None,
            cleanup_failure: None,
            merge_grant: None,
            warnings: Vec::new(),
        }
    );
    let calls = fake.gh_calls.borrow();
    assert!(!calls[0].contains(&"--auto".to_string()));
    assert!(calls[0].contains(&"--match-head-commit".to_string()));
    // The owner's record: one merge_landed span for the run, naming the
    // command lane and the merged PR. The global-journal mirror doubles the
    // row under the test journal target (both writes fold into one file),
    // so the doubled count proves the mirror fired.
    let spans: Vec<_> = journal
        .rows()
        .into_iter()
        .filter(|r| r["data"]["span_kind"] == "merge_landed")
        .collect();
    assert_eq!(spans.len(), 2);
    for span in &spans {
        assert_eq!(span.pointer("/data/path"), Some(&json!("pr_merge")));
        assert_eq!(span.pointer("/data/pr"), Some(&json!(7)));
        assert_eq!(span.pointer("/data/repo"), Some(&json!("o/r")));
        assert_eq!(span.pointer("/data/head"), Some(&json!("abc123")));
    }
}

#[test]
fn a_gh_failure_over_a_merge_that_landed_reads_as_merged() {
    // The local post-merge step can fail after the server-side merge landed.
    let fake = Fake {
        gh_ok: false,
        gh_output: "failed to delete local branch".to_string(),
        facts: Some(PrFacts {
            state: "MERGED".to_string(),
            ..open_facts()
        }),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
        warnings: Vec::new(),
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "merged");
    // The cleanup failure rides its OWN field. A bare success would lose
    // it, and folding it into `note` would report the merge as partial.
    let Outcome::Merged {
        note,
        cleanup_failure,
        ..
    } = outcome
    else {
        unreachable!()
    };
    assert_eq!(note.as_deref(), Some("merged server-side"));
    assert!(cleanup_failure
        .expect("a cleanup failure")
        .contains("failed to delete local branch"));
}

#[test]
fn a_gh_proxy_admit_banner_ahead_of_a_real_refusal_reads_the_refusal_not_the_banner() {
    // Specimen 2026-10-19: the gh proxy prints its admit banner to stderr
    // right before os.execve hands off to the real gh, so both land in one
    // combined stream and the banner is line one. Three merge attempts on a
    // green, mergeable PR all surfaced the generic fallback reason with the
    // banner instead of gh's own refusal.
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh proxy: draft guard unavailable; admitting\n\
                    gh: GraphQL: Pull request is not in a mergeable state"
            .to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
        warnings: Vec::new(),
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "failed");
    let detail = outcome.detail();
    assert!(
        detail.contains("Pull request is not in a mergeable state"),
        "{detail}"
    );
    assert!(!detail.contains("draft guard unavailable"), "{detail}");
    // The same failed-merge fixture also routes the channel refusals: two
    // shapes name the graphql channel, not the merge - GitHub's primary
    // exhaustion and fno's own budget hold. Both retry through the REST PUT
    // with the same head pin (a tight channel routes to another instead of
    // refusing). The withhold row: a secondary rate limit is token-wide, so
    // the REST retry would trip it too - it holds and fires no retry.
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh: GraphQL: API rate limit exceeded for user ID 1".to_string(),
        gh_recovery_ok: Some(true),
        ..clean()
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    let Outcome::Merged { note, .. } = outcome else {
        unreachable!()
    };
    assert_eq!(note.as_deref(), Some("merged server-side (rest fallback)"));
    let calls = fake.gh_calls.borrow();
    let put = calls
        .iter()
        .find(|c| c.first().map(String::as_str) == Some("api"))
        .expect("the REST retry fires");
    assert!(put.contains(&"--method".to_string()) && put.contains(&"PUT".to_string()));
    assert!(put.iter().any(|a| a.contains("pulls/7/merge")));
    assert!(put.contains(&"sha=abc123".to_string()));
    drop(calls);
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh budget: GitHub rate limit held locally (user token: backoff 900s left); \
                    this command did not reach GitHub and no other channel serves it (App token: \
                    none for this call). Retry after 900s. Ledger: fno-agent fleet-incident \
                    gh-budget status"
            .to_string(),
        gh_recovery_ok: Some(true),
        ..clean()
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    let Outcome::Merged { note, .. } = outcome else {
        unreachable!()
    };
    assert_eq!(note.as_deref(), Some("merged server-side (rest fallback)"));
    // The withhold: a secondary limit holds and fires no api retry.
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh: You have exceeded a secondary rate limit".to_string(),
        ..clean()
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "held", "{outcome:?}");
    assert!(
        fake.gh_calls
            .borrow()
            .iter()
            .all(|c| c.first().map(String::as_str) != Some("api")),
        "a secondary limit fires no REST retry"
    );
}

#[test]
fn hold_probe_fails_closed_on_every_non_success() {
    assert_eq!(
        classify_hold_probe(true, b"unheld", b""),
        ProbeOutcome::Clear
    );
    assert_eq!(
        classify_hold_probe(false, b"", b"dispatch-hold:x-owner"),
        ProbeOutcome::Refused("dispatch-hold:x-owner".to_string())
    );
    assert_eq!(
        classify_hold_probe(false, b"", b""),
        ProbeOutcome::Refused(
            "dispatch hold state unreadable; refusing to assume unheld".to_string()
        )
    );
    // The blocked_by hold rides the same polarity: an open blocker holds the
    // PR, a closed or missing one lets it merge.
    let waits = vec![("x-1".to_string(), "x-2".to_string())];
    let open = vec![json!({"id": "x-2", "status": "in_progress"})];
    let closed =
        vec![json!({"id": "x-2", "status": "done", "completed_at": "2026-11-24T00:00:00Z"})];
    match blocked_by_verdict(9, &waits, &open) {
        ProbeOutcome::Refused(reason) => assert!(reason.contains("x-1 waits on x-2"), "{reason}"),
        other => panic!("an open blocker must hold: {other:?}"),
    }
    assert_eq!(blocked_by_verdict(9, &waits, &closed), ProbeOutcome::Clear);
    assert_eq!(blocked_by_verdict(9, &waits, &[]), ProbeOutcome::Clear);
}

#[test]
fn the_command_refuses_a_payload_that_names_no_effect() {
    assert!(parse_request(&json!({"cwd": "/tmp"})).is_err());
    assert!(parse_request(&json!({"effect": "arm"})).is_err());
    assert!(parse_request(&json!({"cwd": "/tmp", "effect": "sideways"})).is_err());
    let ok = parse_request(&json!({"cwd": "/tmp", "effect": "arm", "pr": 7}))
        .expect("a well-formed payload parses");
    assert_eq!(ok.effect, Effect::Arm);
    assert_eq!(ok.pr, Some(7));
}

fn preview_request(pr: u64) -> Request {
    Request {
        pr: Some(pr),
        effect: Effect::Preview,
        require_checks: true,
        ..request(Effect::Preview)
    }
}

fn preview_blockers(fake: &Fake, req: &Request) -> Vec<Blocker> {
    let facts = fake.facts.clone().unwrap_or_else(open_facts);
    match preview_walk(fake, req, &facts) {
        PreviewVerdict::Go { .. } => Vec::new(),
        PreviewVerdict::Blocked { blockers, .. } => blockers,
    }
}

#[test]
fn merge_and_preview_name_the_same_first_blocker_for_every_gate() {
    // AC2: for each gate made to refuse in turn, the Merge outcome's
    // detail and the Preview's first blocker are the SAME answer - one
    // decision, two collectors.
    let base = || Fake { ..clean() };
    let scenarios: Vec<(&str, Fake)> = vec![
        (
            "pr_terminal",
            Fake {
                facts: Some(PrFacts {
                    state: "CLOSED".to_string(),
                    ..open_facts()
                }),
                ..base()
            },
        ),
        (
            "node_unbound",
            Fake {
                node_binding: Some(ProbeOutcome::Refused(
                    "unbound: no node names this PR".to_string(),
                )),
                ..base()
            },
        ),
        (
            "main_red",
            Fake {
                main_ci: Some(Ok(red_main())),
                main_repair: Some(Some("the declared repair lane is x-fix".to_string())),
                ..base()
            },
        ),
        (
            "dispatch_hold",
            Fake {
                dispatch_hold: Some(ProbeOutcome::Refused(
                    "dispatch_hold: held by tgt-x".to_string(),
                )),
                ..base()
            },
        ),
        (
            "outside_pr",
            Fake {
                outside_pr: Some(ProbeOutcome::Refused(
                    "outside_pr: PR #7 is from stranger/footnote; fleet automation never \
                     merges, heals, binds or reviews an outside pull request"
                        .to_string(),
                )),
                ..base()
            },
        ),
        (
            "pr_hold",
            Fake {
                pr_hold: Some(ProbeOutcome::Refused(
                    "pr_hold: the owner holds PR #7 (d-1); release it to clear".to_string(),
                )),
                ..base()
            },
        ),
        (
            "merge_freeze",
            Fake {
                merge_freeze: Some(ProbeOutcome::Refused(
                    "merge_freeze: PR #7 is off the pin merge-freeze:o/r@prs:6".to_string(),
                )),
                ..base()
            },
        ),
        (
            "rule",
            Fake {
                rules: Some(ProbeOutcome::Refused(
                    "Refused by rule: r-release-freeze: the release freeze is on".to_string(),
                )),
                ..base()
            },
        ),
        (
            "red_merge_result",
            Fake {
                merge_result: Some(ProbeOutcome::Refused("F821 in the merged tree".to_string())),
                ..base()
            },
        ),
        (
            "architect_misses_plan",
            Fake {
                architect: Some(Blocker::held("architect_misses_plan", "AC2: unmet")),
                ..base()
            },
        ),
        (
            "architect_off_vision",
            Fake {
                architect: Some(Blocker::held(
                    "architect_off_vision",
                    "principle 4: conflict",
                )),
                ..base()
            },
        ),
        (
            "stacked_base",
            Fake {
                lineage: Some(ProbeOutcome::Refused(
                    "base no longer reaches the default branch".to_string(),
                )),
                ..base()
            },
        ),
        (
            "github_blocked",
            Fake {
                github_block: Some("smoke".to_string()),
                ..base()
            },
        ),
    ];
    for (code, fake) in scenarios {
        let req_merge = Request {
            require_checks: true,
            ..request(Effect::Merge)
        };
        let outcome = run(&fake, &req_merge);
        let blockers = preview_blockers(&fake, &preview_request(7));
        assert!(
            !blockers.is_empty(),
            "{code}: preview cleared while merge refused ({outcome:?})"
        );
        let first = blockers[0].code.clone();
        assert_eq!(
            outcome.detail(),
            blockers[0].detail,
            "{code}: merge detail and the preview's first blocker ({first}) disagree"
        );
    }

    // The two admission gates answer the Arm effect identically: finalize's
    // auto-merge arm passes through decide, so the same gate holds it.
    let outside = Fake {
        outside_pr: Some(ProbeOutcome::Refused(
            "outside_pr: PR #7 is from stranger/footnote".to_string(),
        )),
        ..base()
    };
    let held = Fake {
        pr_hold: Some(ProbeOutcome::Refused(
            "pr_hold: the owner holds PR #7".to_string(),
        )),
        ..base()
    };
    let frozen = Fake {
        merge_freeze: Some(ProbeOutcome::Refused(
            "merge_freeze: PR #7 is off the pin".to_string(),
        )),
        ..base()
    };
    // An unreadable freeze fails closed on both effects.
    let unreadable = Fake {
        merge_freeze: Some(ProbeOutcome::Inconclusive(
            "merge_freeze: the freeze is unreadable".to_string(),
        )),
        ..base()
    };
    for (code, fake, word) in [
        ("outside_pr", outside, "refused"),
        ("pr_hold", held, "held"),
        ("merge_freeze", frozen, "held"),
        ("merge_freeze_unreadable", unreadable, "held"),
    ] {
        for effect in [Effect::Arm, Effect::Merge] {
            let req = Request {
                require_checks: true,
                ..request(effect)
            };
            let outcome = run(&fake, &req);
            assert_eq!(
                outcome.word(),
                word,
                "{code}: the {effect:?} did not answer {word}: {outcome:?}"
            );
        }
    }
}

/// The lead check-in's red token, stubbed: the run name and the sha it
/// failed on. A stub verdict, never a live merge.
fn red_main() -> Value {
    serde_json::json!({
        "verdict": "red",
        "workflow": "cli-ci",
        "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    })
}

#[test]
fn the_main_red_gate_refuses_red_and_exempts_only_the_declared_repair() {
    // Reproduction of 2026-09-30 (four merges landed on a red main): the
    // merge preview walks a stubbed red main verdict, never a live merge.
    let lane = "no node is declared the repair lane; tag the repair node \
                (fno backlog update <node> tags+=main-repair)";
    let refusing = Fake {
        main_ci: Some(Ok(red_main())),
        main_repair: Some(Some(lane.to_string())),
        ..clean()
    };
    let codes: Vec<String> = preview_blockers(&refusing, &preview_request(7))
        .into_iter()
        .map(|b| b.code.to_string())
        .collect();
    assert!(
        codes.contains(&"main_red".to_string()),
        "the preview walked a red main with no main_red blocker: {codes:?}"
    );
    let err = decide(&refusing, &request(Effect::Merge)).unwrap_err();
    assert_eq!(err.word(), "refused", "{}", err.detail());
    let detail = err.detail();
    assert!(detail.contains("cli-ci"), "{detail}");
    assert!(detail.contains("deadbeef"), "{detail}");
    assert!(detail.contains("repair lane"), "{detail}");

    // The exemption: a PR whose node carries the tag is the declared
    // repair, and it lands through the red.
    let repair = Fake {
        main_ci: Some(Ok(red_main())),
        main_repair: Some(None),
        ..clean()
    };
    assert!(decide(&repair, &request(Effect::Merge)).is_ok());

    // A pending main is not red, and an unreadable verdict never
    // manufactures one; the gate names main, not every base.
    for clearing in [
        Fake {
            main_ci: Some(Ok(Value::String("pending".into()))),
            main_repair: Some(Some(lane.to_string())),
            ..clean()
        },
        Fake {
            main_ci: Some(Err("gh api failed: rate limited".to_string())),
            main_repair: Some(Some(lane.to_string())),
            ..clean()
        },
        Fake {
            facts: Some(PrFacts {
                base_ref: "release".to_string(),
                ..open_facts()
            }),
            main_ci: Some(Ok(red_main())),
            ..clean()
        },
    ] {
        assert!(
            decide(&clearing, &request(Effect::Merge)).is_ok(),
            "{}",
            err_words(&clearing)
        );
    }

    // The probe's truth under the wiring: the exemption reads the tagged
    // node from a real store. The live refusal (2026-11-21) had the slice's
    // light projection precede the full carrier rows, so the tag check read
    // the tag-less twin and refused the very lane it named. The store shape
    // is the incident's: tags on the node, pr_number set, primary_pr null,
    // and the Fixes line as the last body line. Each store phase holds the
    // env pin inside its own block: the pin carries the shared env lock,
    // and a std Mutex never relocks on one thread.
    {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".fno")).unwrap();
        let (_env_lock, _env) = pin_graph_env(tmp.path());
        crate::graph_store::seed_rows(
            &crate::org_board::scope::graph_json_path(tmp.path()),
            &[json!({
                "id": "x-9999",
                "status": "in_progress",
                "project": "fno",
                "tags": [MAIN_REPAIR_TAG],
                "pr_number": 497,
                "pr_url": "https://github.com/o/r/pull/497",
                "primary_pr": null
            })],
        )
        .unwrap();
        let incident = PrFacts {
            number: 497,
            head_ref: "fix/pr-read-e0308".to_string(),
            url: "https://github.com/o/r/pull/497".to_string(),
            body: Some("Fixes x-9999\n".to_string()),
            ..open_facts()
        };
        assert_eq!(main_repair_hold_probe(tmp.path(), &incident), None);
    }

    // The withhold arm: the exemption is for the PR whose own node is the
    // repair. An untagged PR bound beside a tagged lane still holds, and the
    // sentence names the lane.
    {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".fno")).unwrap();
        let (_env_lock, _env) = pin_graph_env(tmp.path());
        crate::graph_store::seed_rows(
            &crate::org_board::scope::graph_json_path(tmp.path()),
            &[
                json!({
                    "id": "x-1111",
                    "status": "in_progress",
                    "project": "fno",
                    "pr_number": 7,
                    "pr_url": "https://github.com/o/r/pull/7"
                }),
                json!({
                    "id": "x-8888",
                    "status": "in_progress",
                    "project": "fno",
                    "tags": [MAIN_REPAIR_TAG]
                }),
            ],
        )
        .unwrap();
        let untagged = PrFacts {
            number: 7,
            head_ref: "feature/x-1111".to_string(),
            url: "https://github.com/o/r/pull/7".to_string(),
            body: Some("Fixes x-1111\n".to_string()),
            ..open_facts()
        };
        let hold = main_repair_hold_probe(tmp.path(), &untagged);
        assert!(
            hold.as_deref().is_some_and(|h| h.contains("x-8888")),
            "{hold:?}"
        );
    }
}

/// The refused word of a decide on `fake`, for a failure message.
fn err_words(fake: &Fake) -> String {
    match decide(fake, &request(Effect::Merge)) {
        Ok(_) => "authorized".to_string(),
        Err(outcome) => format!("{}: {}", outcome.word(), outcome.detail()),
    }
}

#[test]
fn an_old_name_url_binds_after_a_repo_transfer_when_the_alias_names_it() {
    let rows = [
        json!({"id": "x-old", "pr_number": 4327,
               "pr_url": "https://github.com/bllshttng/fno/pull/4327"}),
        json!({"id": "x-extra", "additional_prs": [
               {"number": 4411, "url": "https://github.com/bllshttng/fno/pull/4411"}]}),
    ];
    let mut aliases = BTreeMap::new();
    aliases.insert("bllshttng/fno".to_string(), "fnodotdev/fno".to_string());
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 4327, Some("fnodotdev/fno"), &aliases),
        Some(0)
    );
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 4411, Some("fnodotdev/fno"), &aliases),
        Some(1)
    );
    // No alias, no bind: the cross-repo guard still holds.
    assert_eq!(
        crate::authorized_merge::pr_bound_index(
            &rows,
            4327,
            Some("fnodotdev/fno"),
            &BTreeMap::new()
        ),
        None
    );
}

#[test]
fn a_url_less_row_never_steals_a_bind_and_bare_ties_refuse() {
    // Rows recorded before the url rules carried a bare pr_number; after the
    // cut the new repo reuses those numbers, so a bare row binds only while
    // no located ref answers.
    let rows = [
        json!({"id": "x-bare", "pr_number": 16}),
        json!({"id": "x-new", "pr_number": 16,
               "pr_url": "https://github.com/fnodotdev/fno/pull/16"}),
    ];
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 16, Some("fnodotdev/fno"), &BTreeMap::new()),
        Some(1),
        "the located ref outranks the bare number"
    );
    // Two bare rows naming one number refuse the way the dispatch-hold probe
    // does: several nodes bind this PR number with no discriminating url.
    let bare_tie = [
        json!({"id": "x-a", "pr_number": 16}),
        json!({"id": "x-b", "pr_number": 16}),
    ];
    assert_eq!(
        crate::authorized_merge::pr_bound_index(
            &bare_tie,
            16,
            Some("fnodotdev/fno"),
            &BTreeMap::new()
        ),
        None
    );
    // A bare tie at the primary rank refuses the whole bind: it never falls
    // through to bind an additional row by number alone.
    let tie_then_extra = [
        json!({"id": "x-a", "pr_number": 16}),
        json!({"id": "x-b", "pr_number": 16}),
        json!({"id": "x-c", "additional_prs": [{"number": 16}]}),
    ];
    assert_eq!(
        crate::authorized_merge::pr_bound_index(
            &tie_then_extra,
            16,
            Some("fnodotdev/fno"),
            &BTreeMap::new()
        ),
        None
    );
    // A lone bare row keeps the legacy number bind.
    assert_eq!(
        crate::authorized_merge::pr_bound_index(
            &bare_tie[..1],
            16,
            Some("fnodotdev/fno"),
            &BTreeMap::new()
        ),
        Some(0)
    );
}

#[test]
fn a_paint_pr_merges_on_a_missing_look_and_holds_on_a_declined_page() {
    let tmp = std::env::temp_dir().join(format!("xc129-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".fno")).unwrap();
    // questions_dir falls back to the space dir, and the hermetic guard
    // refuses a HOME-derived one under test; point it at a tempdir. The
    // graph store reads the state root too (the bound-node lookup), so
    // FNO_HOME is declared as well.
    struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.1.take() {
                Some(v) => std::env::set_var("FNO_HOME", v),
                None => std::env::remove_var("FNO_HOME"),
            }
        }
    }
    let spaces = std::env::temp_dir().join(format!("xc129-spaces-{}", std::process::id()));
    std::fs::create_dir_all(&spaces).unwrap();
    let _env_lock = crate::pr_status::cache_env_lock();
    // The bound-node lookup transitively resolves the global claims root.
    crate::paths::pin_test_claims_root(&spaces);
    let _env = {
        let prior = std::env::var_os("FNO_SPACES_DIR");
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let prior_home = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", &tmp);
        RestoreEnv(prior, prior_home)
    };
    std::fs::write(
        tmp.join(".fno/config.toml"),
        "merge.visual_paint_paths = [\"crates/fno/src/ui/client/**\"]\n",
    )
    .unwrap();
    let mut fake = clean();
    fake.gh_output = "crates/fno/src/ui/client/theme.rs\nREADME.md\n".to_string();
    let head: String = "a".repeat(40);
    let held = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).expect("held");
    assert_eq!(held.code, "visual_approval");
    assert!(
        held.detail.contains("crates/fno/src/ui/client/theme.rs"),
        "{}",
        held.detail
    );
    // The verdict is warning evidence: no look record anywhere, so the
    // merge lands and the warning routes to the lead as a look request.
    {
        fake.facts = Some(PrFacts {
            head_sha: head.clone(),
            body: Some("Docs: fno-web#1\n".to_string()),
            ..open_facts()
        });
        let req = Request {
            cwd: tmp.clone(),
            covered_head: Some(head.clone()),
            ..request(Effect::Merge)
        };
        let outcome = run(&fake, &req);
        assert_eq!(outcome.word(), "merged", "{outcome:?}");
        assert!(outcome
            .warnings()
            .iter()
            .any(|w| w.code == "visual_approval" && w.detail.contains("no recorded look")));
        let requests = fake.look_requests.borrow();
        assert_eq!(requests.len(), 1, "{requests:?}");
        drop(requests);
        // Reset the stage: the declined-page section below asserts that no
        // merge and no look request happen while a decline names the head.
        fake.look_requests.borrow_mut().clear();
        fake.gh_calls.borrow_mut().clear();
        fake.facts = None;
    }
    // An OPEN page naming the PR never clears the hold.
    let qdir = crate::escalation::questions_dir(&tmp);
    std::fs::create_dir_all(&qdir).unwrap();
    let page = qdir.join("q-xc129test.md");
    std::fs::write(&page, format!(
        "---\nquestion_id: q-xc129\nstatus: answered\nask: May PR 7 merge as painted? (node at {})\nanswer: Hold for changes\n---\n",
        head.chars().take(10).collect::<String>()
    )).unwrap();
    let declined = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).unwrap();
    assert_eq!(declined.code, "visual_declined");
    fake.facts = Some(PrFacts {
        head_sha: head.clone(),
        ..open_facts()
    });
    let mut req = Request {
        cwd: tmp.clone(),
        covered_head: Some(head.clone()),
        ..request(Effect::Merge)
    };
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "held");
    assert!(outcome.detail().contains("the user declined"));
    std::fs::remove_file(&page).unwrap();
    fake.late_decline = Some(page.clone());
    let late = run(&fake, &req);
    assert_eq!(late.word(), "held");
    assert!(late.detail().contains("the user declined"));
    assert!(!fake
        .gh_calls
        .borrow()
        .iter()
        .any(|args| args.get(1).map(String::as_str) == Some("merge")));
    fake.late_decline = None;
    let next_head = "b".repeat(40);
    let evidence = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &next_head).unwrap();
    assert_eq!(evidence.code, "visual_approval");
    assert!(evidence.detail.contains("earlier declined look evidence"));
    fake.facts = Some(PrFacts {
        head_sha: next_head.clone(),
        body: Some("Docs: fno-web#1\n".to_string()),
        ..open_facts()
    });
    req.covered_head = Some(next_head);
    // The declined look was withdrawn with the page and no look names the
    // new head, so the outcome rides the paint warning and merges; the
    // warning routes to the lead as exactly one look request. The body
    // names a doc PR so D2707's docs hold stays out of the way: this leg
    // pins the paint contract, not the docs gate.
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "merged", "{outcome:?}");
    assert!(outcome
        .warnings()
        .iter()
        .any(|w| w.code == "visual_approval" && w.detail.contains("no recorded look")));
    let requests = fake.look_requests.borrow();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let (requested_pr, requested_detail) = requests.first().expect("one look request");
    assert_eq!(*requested_pr, 7);
    assert!(
        requested_detail.contains("no recorded look"),
        "{requested_detail}"
    );
    drop(requests);
    fake.facts = None;
    // A look ask names the PR in its title and stays open: the user was
    // asked and has not answered, so the merge holds instead of warning.
    std::fs::write(
        &page,
        "---\nquestion_id: q-xc129\nstatus: open\ntitle: May PR 7 merge?\n---\nbody\n",
    )
    .unwrap();
    // A question with no shots is no look ask: it warns like a missing look.
    let no_shots = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).unwrap();
    assert_eq!(no_shots.code, "visual_approval");
    std::fs::write(
        &page,
        "---\nquestion_id: q-xc129\nstatus: open\ntitle: May PR 7 merge?\n\
         aliases:\n- May PR 7 merge?\n- D3505\n---\n## Screens\n\n![[q-xc129-1.png]]\n",
    )
    .unwrap();
    let pending = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).unwrap();
    assert_eq!(pending.code, "visual_look_pending");
    assert!(pending.detail.contains("D3505"), "{}", pending.detail);
    fake.facts = Some(PrFacts {
        head_sha: head.clone(),
        body: Some("Docs: fno-web#1\n".to_string()),
        ..open_facts()
    });
    fake.look_requests.borrow_mut().clear();
    let at_head = Request {
        cwd: tmp.clone(),
        covered_head: Some(head.clone()),
        ..request(Effect::Merge)
    };
    let waits = run(&fake, &at_head);
    assert_eq!(waits.word(), "held", "{waits:?}");
    assert!(waits.detail().contains("is open and unanswered"));
    assert!(fake.look_requests.borrow().is_empty());
    // A sibling PR named only in an open page's body prose is no ask.
    std::fs::write(
        &page,
        "---\nquestion_id: q-xc129\nstatus: open\ntitle: May PR 8 merge?\n---\n## Screens\n\nships before PR 7\n",
    )
    .unwrap();
    let warns = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).unwrap();
    assert_eq!(warns.code, "visual_approval");
    fake.facts = None;
    // An answer that rejects, or picks no option, keeps the hold.
    for answer in [
        "2. Hold for changes",
        "Hold for changes",
        "none of these",
        "''",
    ] {
        std::fs::write(
            &page,
            format!(
                "---\nquestion_id: q-xc129\nstatus: answered\ntitle: May PR 7 merge?\n\
                 answer: {answer}\n---\nbody\n"
            ),
        )
        .unwrap();
        assert!(
            crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some(),
            "answer {answer:?} must not clear"
        );
    }
    // An approve answer on a page that names no head leaves the carry's
    // evidence stale: an approval exists, but no commit it names is
    // reachable. A stale look warns and never holds, so the gate clears and
    // the outcome carries the warning.
    let approved = |body: &str| {
        format!(
            "---\nquestion_id: q-xc129\nstatus: answered\ntitle: May PR 7 merge?\n\
             answer: 1. Merge as painted\n---\n{body}\n"
        )
    };
    std::fs::write(&page, approved("body")).unwrap();
    assert!(
        crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none(),
        "a stale approve record warns and never holds"
    );
    // The page answers with the approve option at this head, the hold clears.
    std::fs::write(&page, approved(&format!("PR 7 at head {head}"))).unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    // A push after the user looked: the approval names the old head, and the
    // commit list answers garbage here, so no walked commit matches and the
    // carry warns instead of holding.
    let pushed: String = "b".repeat(40);
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_none());
    // With the commit list readable, the carry reads the paint files changed
    // between the approved commit and the pushed one, then renders frames.
    // The temp vault is no git repo, so the render cannot run: every carry
    // here ends at the unprovable warning that never holds. Which verdict
    // the carry answers (Clear, Ask, Warn) is the mapping test's to prove.
    let routes = |now: &str| {
        vec![
            ("/commits".to_string(), format!("{head}\n{pushed}\n")),
            (format!("compare/{head}...{pushed}"), now.to_string()),
        ]
    };
    fake.gh_routes = routes("crates/fno/src/ui/client/theme.rs\ncli/tests/test_x.py\n");
    assert!(
        crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_none(),
        "the look carries to the new head"
    );
    // A paint file the approval never showed reaches the same unrenderable
    // frames path here, so it warns the same way; the ask verdict is the
    // mapping test's to prove.
    fake.gh_routes = routes("crates/fno/src/ui/client/theme.rs\ncrates/fno/src/client/splash.rs\n");
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_none());
    // An approval that spells the approved head as a short hex prefix
    // carries the same way.
    fake.gh_routes = routes("crates/fno/src/ui/client/theme.rs\n");
    std::fs::write(&page, approved(&format!("PR 7 at {}", &head[..10]))).unwrap();
    assert!(
        crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_none(),
        "a short-sha approval carries to the new head"
    );
    std::fs::write(&page, approved(&format!("PR 7 at head {head}"))).unwrap();
    fake.gh_routes.clear();
    // The page archives into done/; the approval must not lapse.
    let donedir = qdir.join("done");
    std::fs::create_dir_all(&donedir).unwrap();
    std::fs::rename(&page, donedir.join("q-xc129test.md")).unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    // No paint paths in any candidate config: the gate is disarmed and
    // spends no gh read.
    let bare = std::env::temp_dir().join(format!("xc129-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bare);
    std::fs::create_dir_all(&bare).unwrap();
    let mut off = clean();
    off.gh_output = "crates/fno/src/ui/client/theme.rs\n".to_string();
    assert!(crate::merge_gates::visual_approval_blocker(&off, &bare, 7, &head).is_none());
    assert!(off.gh_calls.borrow().is_empty());
    // A checkout that carries the mux client arms the built-in paint list
    // with no config key: a stock footnote tree holds its own UI PRs, and a
    // config list cannot opt a client tree out.
    let stock = std::env::temp_dir().join(format!("xc129-stock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stock);
    std::fs::create_dir_all(stock.join("crates/fno/src/ui/client")).unwrap();
    std::fs::create_dir_all(stock.join(".fno")).unwrap();
    let mut armed = clean();
    armed.gh_output = "crates/fno/src/ui/client/org_board.rs\n".to_string();
    let held = crate::merge_gates::visual_approval_blocker(&armed, &stock, 7, &head)
        .expect("a stock footnote tree holds a mux client PR");
    assert_eq!(held.code, "visual_approval");
    std::fs::write(
        stock.join(".fno/config.toml"),
        "[merge]\nvisual_paint_paths = []\n",
    )
    .unwrap();
    // A machine config cannot disarm or narrow the list in a client tree.
    let held = crate::merge_gates::visual_approval_blocker(&armed, &stock, 7, &head)
        .expect("config does not change what holds in a footnote tree");
    assert_eq!(held.code, "visual_approval");
    let mut keymap_only = clean();
    keymap_only.gh_output = "crates/fno/src/ui/digest_overlay.rs\n".to_string();
    assert!(
        crate::merge_gates::visual_approval_blocker(&keymap_only, &stock, 7, &head).is_none(),
        "digest_overlay.rs renders nothing, so it holds no PR"
    );
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::remove_dir_all(&bare).ok();
    std::fs::remove_dir_all(&stock).ok();
}

#[test]
fn a_role_chat_approval_clears_the_paint_hold_only_at_its_head() {
    let tmp = std::env::temp_dir().join(format!("xc39f-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".fno")).unwrap();
    struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.1.take() {
                Some(v) => std::env::set_var("FNO_HOME", v),
                None => std::env::remove_var("FNO_HOME"),
            }
        }
    }
    let spaces = std::env::temp_dir().join(format!("xc39f-spaces-{}", std::process::id()));
    std::fs::create_dir_all(&spaces).unwrap();
    let _env_lock = crate::pr_status::cache_env_lock();
    // The bound-node lookup transitively resolves the global claims root.
    crate::paths::pin_test_claims_root(&spaces);
    let _env = {
        let prior = std::env::var_os("FNO_SPACES_DIR");
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let prior_home = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", &tmp);
        RestoreEnv(prior, prior_home)
    };
    std::fs::write(
        tmp.join(".fno/config.toml"),
        "merge.visual_paint_paths = [\"crates/fno/src/ui/client/**\"]\n",
    )
    .unwrap();
    let head: String = "a".repeat(40);
    let pushed: String = "b".repeat(40);
    let row_at = |subject: &str,
                  decision: &str,
                  rationale: &str,
                  authority: &str,
                  lifecycle: &str| {
        format!(
            r#"{{"decisions":[{{"decision_id":"d-xc39f","subject":"{subject}","authority_source":"{authority}","lifecycle":"{lifecycle}","decision":"{decision}","rationale":"{rationale}"}}]}}"#
        )
    };
    let row = |decision: &str, rationale: &str, authority: &str, lifecycle: &str| {
        row_at("x-paint", decision, rationale, authority, lifecycle)
    };
    let mut fake = clean();
    fake.gh_output = "crates/fno/src/ui/client/theme.rs\n".to_string();
    let clears = |fake: &Fake, head: &str| {
        crate::merge_gates::role_chat_clears_pr(fake, &tmp, 7, head, "x-paint")
    };
    // The lead's transcription at the PR's node, head-scoped: clears.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "superuser in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(clears(&fake, &head));
    // A head push invalidates the approval.
    assert!(
        !clears(&fake, &pushed),
        "an approval recorded for head a..a must not clear head b..b"
    );
    // With no node bound to the PR, even that row holds: the subject rule
    // has no node to match.
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some());
    // A role row on another subject never clears: the door scopes the role
    // to the lead whose territory covers the subject, so only the PR's own
    // node carries that scope.
    fake.decisions_stdout = Some(
        row_at(
            "release-cadence",
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // No chat attestation in the rationale: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "lead judges it good",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // The reader's row for an Approve tap on the Visual Log clears at its head.
    fake.decisions_stdout = Some(
        row(
            "visual approval PR 7 at <head>: approved",
            "User tapped Approve on the Visual Log (viewer u_test)",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(clears(&fake, &head));
    assert!(!clears(&fake, &pushed));
    // The row's authority is not role: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "agent",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // A retracted approval never clears: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "retracted",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // A board answer: the rationale cites the answered page instead of chat.
    // The page names the PR and no head; the row carries the head.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user answered q-board7 on the board",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    let qdir = crate::escalation::questions_dir(&tmp);
    std::fs::create_dir_all(&qdir).unwrap();
    let page = |status: &str, answer: &str| {
        format!(
            "---\nquestion_id: q-board7\nstatus: {status}\ntitle: May PR 7 merge?\n\
             answer: {answer}\n---\nbody\n"
        )
    };
    // These generic approvals do not supply a look at this head.
    assert!(!clears(&fake, &head));
    for (status, answer) in [("open", "''"), ("answered", "2. Hold for changes")] {
        std::fs::write(qdir.join("q-board7.md"), page(status, answer)).unwrap();
        assert!(!clears(&fake, &head), "{status} {answer} must hold");
    }
    // The approving answer clears, at the row's head only.
    std::fs::write(
        qdir.join("q-board7.md"),
        page("answered", "Approve as shown"),
    )
    .unwrap();
    assert!(clears(&fake, &head));
    assert!(!clears(&fake, &pushed));
    std::fs::remove_file(qdir.join("q-board7.md")).unwrap();
    fake.decisions_stdout = None;
    let held = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).expect("warning");
    assert!(held.detail.contains("no recorded look"), "{}", held.detail);
    // A split: the node holds PR 6 and lists PR 7 in additional_prs. PR 7
    // binds to it, so the approval recorded on that node clears PR 7. A row
    // whose own pr_number is the PR still wins over a row that only lists it.
    crate::graph_store::seed_rows(
        &crate::org_board::scope::graph_json_path(&tmp),
        &[
            json!({"id": "x-lists", "status": "in_review", "additional_prs": [{"number": 6}]}),
            json!({"id": "x-paint", "status": "in_review", "pr_number": 6,
                   "additional_prs": [{"number": 7}]}),
        ],
    )
    .unwrap();
    let bound = |pr| {
        crate::authorized_merge::pr_bound_entry(&tmp, pr)
            .and_then(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
    };
    assert_eq!(bound(7).as_deref(), Some("x-paint"));
    assert_eq!(bound(6).as_deref(), Some("x-paint"));
    assert_eq!(bound(8), None);
    // PR numbers collide across repositories: a ref whose URL names another
    // repository never binds, primary or additional; a ref with no URL does.
    let rows = [
        json!({"id": "x-foreign", "pr_number": 7,
               "pr_url": "https://github.com/o/other/pull/7"}),
        json!({"id": "x-also-foreign", "additional_prs": [
               {"number": 7, "url": "https://github.com/o/other/pull/7"}]}),
        json!({"id": "x-here", "additional_prs": [
               {"number": 7, "url": "https://github.com/O/Here/pull/7"}]}),
    ];
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 7, Some("o/here"), &BTreeMap::new()),
        Some(2)
    );
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows[..2], 7, Some("o/here"), &BTreeMap::new()),
        None
    );
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 7, None, &BTreeMap::new()),
        Some(0)
    );
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn docs_line_missing_holds_merge_and_passes_with_a_docs_line() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join("crates/fno/src/ui/client")).unwrap();
    let _pins = [claims::EnvVarGuard::set(
        "FNO_CONFIG",
        temp.path().join("missing-config.toml").to_str().unwrap(),
    )];
    let route = (
        "/files".to_string(),
        "skills/example/SKILL.md\n".to_string(),
    );
    let fake_with_body = |body: &str| {
        let mut fake = clean();
        fake.facts = Some(PrFacts {
            body: Some(body.to_string()),
            ..open_facts()
        });
        fake.gh_routes = vec![route.clone()];
        fake
    };
    // Hold on the merge door: user-surface diff, no Docs: line.
    let held = run(
        &fake_with_body("## Summary\n"),
        &Request {
            cwd: repo.clone(),
            ..request(Effect::Merge)
        },
    );
    assert!(
        matches!(&held, Outcome::Held { reason, .. } if reason.contains("no `Docs:` line")),
        "{held:?}"
    );
    // The same facts pass with a Docs: line naming the doc PR.
    let merged = run(
        &fake_with_body("## Summary\nDocs: fno-web#9\n"),
        &Request {
            cwd: repo.clone(),
            ..request(Effect::Merge)
        },
    );
    assert!(matches!(merged, Outcome::Merged { .. }), "{merged:?}");
    // Preview keeps the warning: pr status is a read receipt.
    let preview_fake = fake_with_body("## Summary\n");
    let preview_facts = preview_fake.facts.clone().unwrap();
    let preview = preview_walk(
        &preview_fake,
        &Request {
            cwd: repo.clone(),
            ..request(Effect::Preview)
        },
        &preview_facts,
    );
    match preview {
        PreviewVerdict::Go { warnings, .. } => {
            assert!(warnings.iter().any(|w| w.code == "docs_line_missing"));
        }
        other => panic!("preview held an undocumented surface: {other:?}"),
    }
    // An empty changed-file answer lists no user surface, so the merge
    // lands with no docs warning at all.
    let unknown = run(
        &clean(),
        &Request {
            cwd: repo,
            ..request(Effect::Merge)
        },
    );
    assert!(matches!(unknown, Outcome::Merged { .. }), "{unknown:?}");
    assert!(
        !unknown
            .warnings()
            .iter()
            .any(|w| w.code.starts_with("docs_")),
        "{unknown:?}"
    );
}

/// A graph-seeding test's hermetic env. The graph anchor ignores cwd and
/// resolves through FNO_HOME, so an unpinned store would land in the real
/// home; pin it (and the claims root the bound-node lookup transitively
/// resolves) for the life of the test, under the shared env lock so
/// parallel tests cannot race the mutation.
struct GraphEnvRestore(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
impl Drop for GraphEnvRestore {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
        match self.1.take() {
            Some(v) => std::env::set_var("FNO_HOME", v),
            None => std::env::remove_var("FNO_HOME"),
        }
    }
}

fn pin_graph_env(tmp: &Path) -> (std::sync::MutexGuard<'static, ()>, GraphEnvRestore) {
    let lock = crate::pr_status::cache_env_lock();
    let spaces = std::env::temp_dir().join(format!("x17ee-spaces-{}", std::process::id()));
    std::fs::create_dir_all(&spaces).unwrap();
    crate::paths::pin_test_claims_root(&spaces);
    let prior_spaces = std::env::var_os("FNO_SPACES_DIR");
    std::env::set_var("FNO_SPACES_DIR", &spaces);
    let prior_home = std::env::var_os("FNO_HOME");
    std::env::set_var("FNO_HOME", tmp);
    (lock, GraphEnvRestore(prior_spaces, prior_home))
}
