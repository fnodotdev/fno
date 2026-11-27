//! The propose-and-prove loop: a week's eval failures draft one candidate
//! change per cluster, the bank proves it against a seeded held-out half of
//! the golden tier, and only a real gain opens a PR. One PR per cycle at
//! most, one commit, with the proof in the body; every cluster lands a row
//! in `<state>/evals/propose/ledger.jsonl`, shipped or not.
//!
//! Facts this module owns: the seeded train/held-out split, the
//! answer-key strip on the proposer's input, the reply tripwires, the gain
//! rule, the count-identity ledger and probation. It calls
//! `evals_compare::failure_fold` and `evals_compare::fold` rather than
//! copying them. Candidate surfaces are repo text only (D2977): hooks,
//! routing and fnh changes wait on their own nodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::reader_route::ReaderRoute;

use crate::evals::bank::TaskSpec;
use crate::evals::failure_types::{prove, unproved_tasks, Catalog};
use crate::evals_power::SEED;
use crate::evals_trend::Row;

/// Clusters one cycle drafts. One change per cluster keeps the per-change
/// verdict honest: two merged changes in one probation window read
/// `confounded` (second pass, change 5).
pub(crate) const MAX_CLUSTERS: usize = 2;

/// Files per reply cap (change 2). A reply that rewrites the whole repo is
/// not a candidate.
pub(crate) const MAX_EDITS: usize = 3;

/// The default cycle cap when config is silent (change 3).
pub(crate) const DEFAULT_CYCLE_USD: f64 = 10.0;

/// Typed failures of one propose cycle (R6: no new string error type in
/// this crate). `Display` carries the one-line cause the ledger row reads.
#[derive(Debug)]
pub(crate) enum ProposeError {
    Bank(String),
    Fold(String),
    Reply(String),
    Budget(String),
    Ledger(String),
    Pr(String),
    Io(String),
}

impl fmt::Display for ProposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, cause) = match self {
            ProposeError::Bank(c) => ("bank", c),
            ProposeError::Fold(c) => ("fold", c),
            ProposeError::Reply(c) => ("reply", c),
            ProposeError::Budget(c) => ("budget", c),
            ProposeError::Ledger(c) => ("ledger", c),
            ProposeError::Pr(c) => ("pr", c),
            ProposeError::Io(c) => ("io", c),
        };
        write!(f, "evals propose {kind}: {cause}")
    }
}

impl std::error::Error for ProposeError {}

impl From<std::io::Error> for ProposeError {
    fn from(e: std::io::Error) -> Self {
        ProposeError::Io(e.to_string())
    }
}

/// Which half of the golden tier a task id lands in. The split is a
/// preregistered rule (D2982): the proposer reads only train
/// goldens, and a change ships only on held-out proof with the whole tier
/// unregressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Train,
    HeldOut,
}

/// splitmix64's finalizer, the mix the power block's seed rides through.
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The split: fold the id's bytes through `mix64` seeded with the power
/// block's seed; an even final word is train, odd is held-out. Stable as
/// goldens are added, and printed in the receipt.
pub(crate) fn split(id: &str) -> Role {
    let mut h = SEED;
    for b in id.as_bytes() {
        h ^= u64::from(*b);
        h = mix64(h);
    }
    if h % 2 == 0 {
        Role::Train
    } else {
        Role::HeldOut
    }
}

fn split_roles(ids: &[String]) -> (Vec<String>, Vec<String>) {
    let mut train = Vec::new();
    let mut held_out = Vec::new();
    for id in ids {
        match split(id) {
            Role::Train => train.push(id.clone()),
            Role::HeldOut => held_out.push(id.clone()),
        }
    }
    (train, held_out)
}

/// One failing proved failure type, with its failing goldens' split roles.
#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    pub(crate) kind: String,
    pub(crate) description: String,
    pub(crate) cost: String,
    /// Goldens of this type whose newest grades read failing or flaky.
    pub(crate) failing_tasks: Vec<String>,
    pub(crate) train: Vec<String>,
    pub(crate) held_out: Vec<String>,
}

/// The typed read of `evals_compare::failure_fold`'s output (R1: parse at
/// the boundary; no string-key reads past it).
#[derive(Deserialize)]
struct FailureFold {
    #[serde(default)]
    types: Vec<FailureTypeFold>,
}

#[derive(Deserialize)]
struct FailureTypeFold {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    cost: String,
    #[serde(default)]
    failing_tasks: Vec<String>,
    #[serde(default)]
    proved: bool,
}

/// The clusters one cycle drafts: proved types with a failing golden, in
/// the fold's cost order, capped at `MAX_CLUSTERS`. An unproved type never
/// picks (AC1-EDGE): an unproved grader cannot prove a gain.
pub(crate) fn clusters(
    rows: &[Row],
    tasks: &[TaskSpec],
    bank_dir: &std::path::Path,
    repeats: usize,
) -> Result<Vec<Cluster>, ProposeError> {
    let catalog = Catalog::load(bank_dir).map_err(ProposeError::Bank)?;
    let proofs = prove(&catalog, tasks, bank_dir);
    let fold = crate::evals_compare::failure_fold(rows, tasks, &catalog, &proofs, repeats);
    let parsed: FailureFold = serde_json::from_value(fold)
        .map_err(|e| ProposeError::Fold(format!("failure fold unreadable: {e}")))?;
    Ok(parsed
        .types
        .into_iter()
        .filter(|t| t.proved && !t.failing_tasks.is_empty())
        .take(MAX_CLUSTERS)
        .map(|t| {
            let (train, held_out) = split_roles(&t.failing_tasks);
            Cluster {
                kind: t.kind,
                description: t.description,
                cost: t.cost,
                failing_tasks: t.failing_tasks,
                train,
                held_out,
            }
        })
        .collect())
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn contains_norm(flat: &str, s: &str) -> bool {
    let n = norm(s);
    !n.is_empty() && flat.contains(&n)
}

/// True when the built text holds no held-out golden's prompt line, no
/// grade command and no expected answer (change 1). Quotes and rules are
/// failure evidence, not answer keys; the scan reads only what must never
/// reach the proposer. A hit means the built text leaked, and the cycle
/// refuses rather than ships a stripped flag that lies.
pub(crate) fn answer_keys_stripped(text: &str, tasks: &[TaskSpec]) -> bool {
    let flat = norm(text);
    for task in tasks {
        if split(&task.id) != Role::HeldOut {
            continue;
        }
        if let Some(p) = &task.prompt {
            if contains_norm(&flat, p) {
                return false;
            }
        }
        let Some(g) = task.golden.as_ref() else {
            continue;
        };
        if contains_norm(&flat, &g.expected) {
            return false;
        }
        for check in &task.grade {
            if let Some(cmd) = &check.command {
                if contains_norm(&flat, cmd) {
                    return false;
                }
            }
            if let Some(pat) = &check.pattern {
                if contains_norm(&flat, pat) {
                    return false;
                }
            }
        }
    }
    true
}

/// The proposer's input for one cluster: the type's id, description and
/// cost, then the train goldens' rule and quote. No prompt, no grade, no
/// expected answer, nothing from `evals/fixtures/metrics` (change 1). A
/// cluster with no train golden reads the description and the case quotes
/// only (the plan's uncovered case); a mixed cluster carries no held-out
/// field at all (AC1-HP).
pub(crate) fn proposer_input(cluster: &Cluster, tasks: &[TaskSpec]) -> String {
    let mut text = format!(
        "failure_type: {}\ndescription: {}\ncost: {}\n",
        cluster.kind, cluster.description, cluster.cost
    );
    if cluster.train.is_empty() {
        text.push_str(
            "\nNo train golden of this type reads today. Draft from the \
             description and the case quotes only.\n",
        );
        for id in &cluster.held_out {
            if let Some(quote) = tasks
                .iter()
                .find(|t| &t.id == id)
                .and_then(|t| t.golden.as_ref())
                .map(|g| g.quote.as_str())
            {
                text.push_str(&format!("\ncase quote ({id}): {quote}\n"));
            }
        }
        return text;
    }
    for id in &cluster.train {
        if let Some((rule, quote)) = tasks
            .iter()
            .find(|t| &t.id == id)
            .and_then(|t| t.golden.as_ref())
            .map(|g| (g.rule.as_str(), g.quote.as_str()))
        {
            text.push_str(&format!(
                "\ntrain golden {id}:\nrule: {rule}\nquote: {quote}\n"
            ));
        }
    }
    text
}

/// The proposer's reply, parsed once at the boundary (R1). `verdict` reads
/// `change` or `keep_base`; `kind` reads `procedure` or `knowledge`; each
/// edit is one full-file replacement.
#[derive(Deserialize)]
pub(crate) struct Reply {
    pub(crate) verdict: String,
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) edits: Vec<Edit>,
    #[serde(default)]
    pub(crate) reason: String,
}

#[derive(Deserialize)]
pub(crate) struct Edit {
    pub(crate) path: String,
    pub(crate) content: String,
}

/// The first JSON object in a model reply.
pub(crate) fn parse_reply(out: &str) -> Result<Reply, ProposeError> {
    let (a, b) = (out.find('{'), out.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        return Err(ProposeError::Reply("no JSON object in the reply".into()));
    };
    serde_json::from_str(&out[a..=b])
        .map_err(|e| ProposeError::Reply(format!("reply is not the JSON asked for: {e}")))
}

/// The 8-word runs (windows) of `s`, normalized. Fewer than 8 words
/// yields none.
fn eight_grams(s: &str) -> Vec<String> {
    let words: Vec<&str> = s.split_whitespace().collect();
    words
        .windows(8)
        .map(|w| w.join(" ").to_lowercase())
        .collect()
}

/// The reply tripwires (change 2): the path allowlist, at most
/// MAX_EDITS files per reply, and the 8-word answer-key run. A refusal
/// names the reason and, for a quoted key, the golden id (AC2-HP).
pub(crate) fn validate_reply(reply: &Reply, tasks: &[TaskSpec]) -> Result<(), ProposeError> {
    match reply.verdict.as_str() {
        "keep_base" => return Ok(()),
        "change" => {}
        other => {
            return Err(ProposeError::Reply(format!(
                "verdict must be change or keep_base, got {other:?}"
            )))
        }
    }
    if !matches!(reply.kind.as_str(), "procedure" | "knowledge") {
        return Err(ProposeError::Reply(format!(
            "kind must be procedure or knowledge, got {:?}",
            reply.kind
        )));
    }
    if reply.edits.is_empty() {
        return Err(ProposeError::Reply("change reply carries no edits".into()));
    }
    if reply.edits.len() > MAX_EDITS {
        return Err(ProposeError::Reply(format!(
            "at most {MAX_EDITS} files per reply, got {}",
            reply.edits.len()
        )));
    }
    for edit in &reply.edits {
        if !allowed_path(&edit.path) {
            return Err(ProposeError::Reply(format!(
                "edit path {:?} is outside AGENTS.md, docs/, skills/*/SKILL.md and skills/*/references/",
                edit.path
            )));
        }
    }
    // The 8-word run: per golden, so a refusal names the id (AC2-HP).
    for (id, keys) in golden_key_grams(tasks) {
        for edit in &reply.edits {
            if eight_grams(&edit.content).iter().any(|g| keys.contains(g)) {
                return Err(ProposeError::Reply(format!(
                    "answer_key_quote: an edit quotes 8 consecutive words of golden {id}"
                )));
            }
        }
    }
    Ok(())
}

/// Each golden's answer-key 8-grams: its prompt, expected answer and grade
/// command or pattern words.
fn golden_key_grams(tasks: &[TaskSpec]) -> Vec<(String, BTreeSet<String>)> {
    tasks
        .iter()
        .filter_map(|t| {
            let g = t.golden.as_ref()?;
            let mut keys = BTreeSet::new();
            if let Some(p) = &t.prompt {
                keys.extend(eight_grams(p));
            }
            keys.extend(eight_grams(&g.expected));
            for c in &t.grade {
                if let Some(cmd) = &c.command {
                    keys.extend(eight_grams(cmd));
                }
                if let Some(pat) = &c.pattern {
                    keys.extend(eight_grams(pat));
                }
            }
            (!keys.is_empty()).then_some((t.id.clone(), keys))
        })
        .collect()
}

/// Candidate surfaces (change 2): repo text only.
fn allowed_path(p: &str) -> bool {
    p == "AGENTS.md"
        || p.starts_with("docs/")
        || (p.starts_with("skills/") && p.ends_with("/SKILL.md"))
        || (p.starts_with("skills/") && p.contains("/references/"))
}

/// The proposer's prompt: the loop's contract and the reply shape.
/// keep_base is a valid answer when the evidence does not argue for a
/// change (second pass: the selector never becomes the reward).
pub(crate) fn propose_prompt(cluster: &Cluster, input: &str) -> String {
    format!(
        "You draft one candidate change for one failure type of this repository. Read AGENTS.md, docs/evals.md section Propose and prove, and the files the evidence names.\n\
Rules: candidates are repo text only (AGENTS.md, docs/, a SKILL.md, or a skill reference page); one full-file replacement per edit; keep_base is a valid answer whenever the evidence does not argue for a change; never restate an eval prompt, its expected answer or its grader.\n\
Failure type: {kind}\n\
Evidence:\n{input}\n\
Reply with one JSON object and nothing else.\n\
A change: {{\"verdict\":\"change\",\"kind\":\"procedure\",\"edits\":[{{\"path\":\"AGENTS.md\",\"content\":\"<the whole new file>\"}}],\"reason\":\"<one sentence>\"}}\n\
No change: {{\"verdict\":\"keep_base\",\"kind\":\"procedure\",\"edits\":[],\"reason\":\"<one sentence>\"}}\n\
Do not invent a rule you cannot cite from a repo file.",
        kind = cluster.kind,
    )
}

/// The proposer's one-shot: reader tools only, no settings, routed through
/// the reader route profile `evals-propose`, model sonnet, 600 s (the
/// golden drafter's spawn shape).
pub(crate) fn spawn_reader(
    name: &str,
    prompt: &str,
    cwd: &std::path::Path,
    timeout: u64,
    route: &ReaderRoute,
) -> Result<(i32, String, String), ProposeError> {
    let argv = crate::reader_route::reader_tools_argv(name, prompt, cwd, timeout, route, &[])
        .map_err(|e| ProposeError::Reply(format!("reader argv: {e}")))?;
    crate::blueprint_judge::headless_spawn(&argv, timeout)
        .map_err(|e| ProposeError::Reply(format!("spawn: {e}")))
}

/// The cycle's gain verdict (change 3): ship only on a positive held-out
/// power block over an unregressed tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gain {
    Ship,
    Regression,
    Underpowered,
    InsideNoise,
}

/// The typed read of `evals_compare::fold`'s view (R1: parse at the
/// boundary).
#[derive(Deserialize)]
struct FoldView {
    #[serde(default)]
    breaking: bool,
    #[serde(default)]
    regressions: Vec<Value>,
    #[serde(default)]
    power: Option<PowerBlock>,
}

#[derive(Deserialize)]
struct PowerBlock {
    #[serde(default)]
    verdict: String,
    paired: Option<PairedBlock>,
}

#[derive(Deserialize)]
struct PairedBlock {
    #[serde(default)]
    mean_delta_points: Option<f64>,
}

/// The gain rule (AC3-HP, AC3-EDGE): a tier break or a regression entry
/// reads regression whatever the held-out block says; a negative
/// beats_noise never ships (the power verdict ignores the sign, so the
/// mean's sign decides); underpowered and inside_noise ship nothing.
pub(crate) fn gain(held: &Value, all: &Value) -> Result<Gain, ProposeError> {
    let h: FoldView = serde_json::from_value(held.clone())
        .map_err(|e| ProposeError::Fold(format!("held-out view unreadable: {e}")))?;
    let a: FoldView = serde_json::from_value(all.clone())
        .map_err(|e| ProposeError::Fold(format!("all-golden view unreadable: {e}")))?;
    if a.breaking || !a.regressions.is_empty() {
        return Ok(Gain::Regression);
    }
    let Some(power) = h.power else {
        return Err(ProposeError::Fold("view carries no power block".into()));
    };
    if power.verdict == "underpowered" {
        return Ok(Gain::Underpowered);
    }
    if power.verdict == "beats_noise" {
        return Ok(match power.paired.and_then(|p| p.mean_delta_points) {
            Some(m) if m > 0.0 => Gain::Ship,
            Some(_) => Gain::Regression,
            None => Gain::Underpowered,
        });
    }
    Ok(Gain::InsideNoise)
}

/// The cycle's planned attempt counts, priced before launch (change 3's
/// budget line).
pub(crate) struct CyclePlan {
    pub(crate) base_attempts: usize,
    pub(crate) per_candidate_attempts: usize,
    pub(crate) candidates: usize,
    pub(crate) proposer_calls: usize,
    pub(crate) estimate_usd: f64,
}

/// The budget line (change 3): base attempts = goldens x repeats, each
/// candidate cohort = goldens x repeats. Over `cycle_usd` the plan drops
/// candidates one at a time; base-only over cap leaves candidates at zero
/// and the estimate over cap, which the caller refuses on.
pub(crate) fn plan_budget(
    goldens: usize,
    repeats: usize,
    candidates: usize,
    cap_usd: f64,
    attempt_usd: f64,
) -> CyclePlan {
    let base_attempts = goldens * repeats;
    let per_candidate_attempts = goldens * repeats;
    let est = |cands: usize| (base_attempts + cands * per_candidate_attempts) as f64 * attempt_usd;
    let mut candidates = candidates;
    while candidates > 0 && est(candidates) > cap_usd {
        candidates -= 1;
    }
    CyclePlan {
        base_attempts,
        per_candidate_attempts,
        candidates,
        proposer_calls: 0,
        estimate_usd: est(candidates),
    }
}

/// The per-cycle cap (change 3): `evals.propose.cycle_usd`, default 10.0,
/// read like the gate's daily_usd (float, integer, or numeric string).
pub(crate) fn cycle_usd(repo: &std::path::Path) -> f64 {
    match crate::agents_config::config_value_deep(repo, &["evals", "propose", "cycle_usd"]) {
        Some(toml::Value::Float(f)) => f,
        Some(toml::Value::Integer(i)) => i as f64,
        Some(toml::Value::String(v)) => v.trim().parse().unwrap_or(DEFAULT_CYCLE_USD),
        _ => DEFAULT_CYCLE_USD,
    }
}

/// One history row, typed (R1): the fields the count assert reads.
#[derive(Deserialize)]
struct HistoryRow {
    #[serde(default)]
    attempt_id: Option<String>,
    #[serde(default)]
    experiment_id: Option<String>,
}

/// A ledger row's cycle mark: every cycle that gets past the cluster pick
/// writes one, cohort or not, so the leg's week reads from the ledger.
#[derive(Deserialize)]
struct CycleMark {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    ts: Option<String>,
}

/// The count-identity ledger assert (change 3, AC3-LEDGER): for each cycle
/// cohort the history holds exactly the planned attempt count, and no
/// attempt id repeats. A mismatch ships nothing.
pub(crate) fn assert_ledger(
    rows: &[Value],
    base_cohort: &str,
    candidate_cohorts: &[String],
    planned: &CyclePlan,
) -> Result<(), ProposeError> {
    let mut wanted: Vec<(String, usize)> = vec![(base_cohort.to_string(), planned.base_attempts)];
    for c in candidate_cohorts {
        wanted.push((c.clone(), planned.per_candidate_attempts));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        let r: HistoryRow = serde_json::from_value(row.clone())
            .map_err(|e| ProposeError::Ledger(format!("history row unreadable: {e}")))?;
        let Some(cohort) = r.experiment_id.as_deref() else {
            continue;
        };
        if !wanted.iter().any(|(name, _)| name == cohort) {
            continue;
        }
        let id = r.attempt_id.clone().unwrap_or_default();
        if id.is_empty() {
            return Err(ProposeError::Ledger(format!(
                "count_mismatch: a row in {cohort} carries no attempt id"
            )));
        }
        if !seen.insert(id.clone()) {
            return Err(ProposeError::Ledger(format!(
                "count_mismatch: duplicate attempt id {id:?}"
            )));
        }
        *counts.entry(cohort.to_string()).or_default() += 1;
    }
    for (name, want) in &wanted {
        let got = counts.get(name).copied().unwrap_or(0);
        if got != *want {
            return Err(ProposeError::Ledger(format!(
                "count_mismatch: cohort {name} holds {got} rows, planned {want}"
            )));
        }
    }
    Ok(())
}

/// The weekly propose leg, mirrored from evals_capability: once per
/// `evals.schedule_days`, one cycle in flight under claim
/// `evals:propose-run`, holds on loops_pause and the RAM gate.
pub(crate) const CLAIM_KEY: &str = "evals:propose-run";
pub(crate) const COHORT_PREFIX: &str = "propose-";

/// The leg's inputs are the capability leg's: one shape, one owner.
pub(crate) use crate::evals_capability::Leg;

/// The leg's verdict: fresh (not due), budget (cap refuses the plan),
/// gated (machine holds), launched.
#[derive(Debug, PartialEq)]
pub(crate) enum LegVerdict {
    Fresh,
    Budget,
    Launch,
}

/// The leg's due+budget decision, the capability leg's shape: due when the
/// ledger's newest cycle mark is older than the window; budget when the
/// cap fits no candidate beside the base.
pub(crate) fn decide(
    ledger: &[Value],
    now: chrono::DateTime<chrono::Utc>,
    window_days: i64,
    goldens: usize,
    cap_usd: f64,
    attempt_usd: f64,
) -> LegVerdict {
    let newest = ledger
        .iter()
        .filter_map(|r| serde_json::from_value::<CycleMark>(r.clone()).ok())
        .filter(|m| m.kind == "cycle")
        .filter_map(|m| chrono::DateTime::parse_from_rfc3339(m.ts.as_deref()?).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .max();
    if let Some(ts) = newest {
        if (now - ts).num_days() < window_days {
            return LegVerdict::Fresh;
        }
    }
    // The plan prices the whole cycle: base plus MAX_CLUSTERS candidates.
    let plan = plan_budget(goldens, 3, MAX_CLUSTERS, cap_usd, attempt_usd);
    if plan.candidates == 0 {
        return LegVerdict::Budget;
    }
    LegVerdict::Launch
}

/// Release the cycle claim; a failed release only logs, because the claim's
/// TTL frees it anyway.
fn release_claim(holder: &str, root: Option<&std::path::Path>) {
    if let Err(e) = crate::claims::release(CLAIM_KEY, holder, root, None) {
        eprintln!("evals propose: release {CLAIM_KEY}: {e}");
    }
}

/// The run marker beside the history: a launch that fails before it writes
/// a row waits a day instead of relaunching every tick.
fn attempt_path(history: &std::path::Path) -> std::path::PathBuf {
    history.with_file_name("evals-propose-attempt.txt")
}

fn tried_within_a_day(history: &std::path::Path, now: chrono::DateTime<chrono::Utc>) -> bool {
    std::fs::read_to_string(attempt_path(history))
        .ok()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t.trim()).ok())
        .is_some_and(|t| (now - t.with_timezone(&chrono::Utc)).num_hours() < 24)
}

/// Tick mode: launch the detached cycle when due. The outcome word is for
/// the caller's log and the tests; a skip is silent.
pub(crate) fn tick(
    leg: &Leg,
    now: chrono::DateTime<chrono::Utc>,
    can_launch: bool,
    argv: &[String],
    spawner: &dyn Fn(&[String]) -> Option<u32>,
) -> &'static str {
    let root = leg
        .claims_root
        .map(std::path::Path::to_path_buf)
        .or_else(crate::claims::global_claims_root);
    match crate::claims::status(CLAIM_KEY, root.as_deref()) {
        (crate::claims::ClaimState::Live, _) => return "in_flight",
        (crate::claims::ClaimState::Corrupted, _) => return "claim_corrupted",
        (crate::claims::ClaimState::Suspect | crate::claims::ClaimState::Stale, Some(rec)) => {
            release_claim(&rec.holder, root.as_deref());
        }
        _ => {}
    }
    let ledger =
        crate::evals_gate::history_rows(&ledger_path(&crate::evals_candidates::state_root()));
    let daily = cycle_usd(leg.repo);
    let lane = crate::evals_gate::config_lane(leg.repo);
    let lane_price = crate::evals_gate::lane_attempt_usd(leg.repo, &lane);
    let goldens = crate::evals::bank::discover_bank(&leg.repo.join("evals").join("bank"))
        .map(|tasks| {
            tasks
                .iter()
                .filter(|t| t.tags.iter().any(|g| g == "golden") && !t.is_demoted())
                .count()
        })
        .unwrap_or(0);
    match decide(&ledger, now, leg.window_days, goldens, daily, lane_price) {
        LegVerdict::Fresh => return "fresh",
        LegVerdict::Budget => return "budget",
        LegVerdict::Launch => {}
    }
    if !can_launch {
        return "gated";
    }
    if tried_within_a_day(leg.history, now) {
        return "retry_wait";
    }
    if let Err(e) = std::fs::write(attempt_path(leg.history), now.to_rfc3339()) {
        eprintln!("evals propose: attempt marker: {e}");
    }
    let Some(pid) = spawner(argv) else {
        return "spawn_failed";
    };
    let opts = crate::claims::AcquireOpts {
        host: None,
        pid: Some(pid),
        pid_unavailable: false,
        ttl_ms: Some((leg.run_timeout_s as i64 + 600) * 1000),
        reason: Some("evals propose cycle".into()),
        metadata: None,
        pid_provenance: None,
        identity: None,
        root: root.clone(),
        events_dir: None,
    };
    let _ = crate::claims::acquire(CLAIM_KEY, &pid.to_string(), opts);
    "launched"
}

/// bank_pr::open's shape.
pub(crate) type Opener<'a> =
    &'a dyn Fn(&str, &str, &str, &[(std::path::PathBuf, Vec<u8>)]) -> Result<String, ProposeError>;

/// The state-side ledger path: `<state>/db/evals/propose/ledger.jsonl`.
pub(crate) fn ledger_path(state: &std::path::Path) -> std::path::PathBuf {
    crate::evals::state_evals_dir(state)
        .join("propose")
        .join("ledger.jsonl")
}

/// One ledger row per cluster, shipped or not (change 4). Appends one
/// line; a failed write names the path and moves on.
fn ledger_append(state: &std::path::Path, row: &Value) -> Result<(), ProposeError> {
    let path = ledger_path(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| ProposeError::Io(format!("{}: {e}", path.display())))?;
    use std::io::Write as _;
    writeln!(f, "{row}").map_err(|e| ProposeError::Io(format!("{}: {e}", path.display())))?;
    Ok(())
}

/// The candidate's commit: the reply's full-file edits as one commit on a
/// local ref `refs/fno/propose/<tag>` off `base_sha`, never pushed. The
/// variant axis scores a git ref of this repo (docs/evals.md Variants), and
/// a local ref resolves the same as a pushed one. A failed write or commit
/// is an error, never a silent base-tree candidate that spends a cohort.
fn candidate_ref(
    repo: &std::path::Path,
    base_sha: &str,
    tag: &str,
    edits: &[(std::path::PathBuf, Vec<u8>)],
) -> Result<String, ProposeError> {
    let dir = std::env::temp_dir().join(format!("fno-propose-ref-{}", tag.replace('/', "-")));
    let dir_s = dir.to_string_lossy().into_owned();
    // A stale tree from a crashed cycle; none is the usual case.
    drop(git_out(repo, &["worktree", "remove", "--force", &dir_s]));
    git_out(repo, &["worktree", "add", "--detach", &dir_s, base_sha])?;
    let made = (|| -> Result<String, ProposeError> {
        let mut rels = Vec::new();
        for (rel, bytes) in edits {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, bytes)?;
            rels.push(rel.to_string_lossy().into_owned());
        }
        let mut add = vec!["add", "--"];
        add.extend(rels.iter().map(String::as_str));
        git_out(&dir, &add)?;
        git_out(&dir, &["commit", "-q", "-m", "evals: propose candidate"])?;
        git_out(&dir, &["rev-parse", "HEAD"])
    })();
    if let Err(e) = git_out(repo, &["worktree", "remove", "--force", &dir_s]) {
        eprintln!("evals propose: candidate tree {dir_s}: {e}");
    }
    let head = made?;
    git_out(
        repo,
        &["update-ref", &format!("refs/fno/propose/{tag}"), &head],
    )?;
    Ok(head)
}

/// One bounded command in `dir` through the spawn seam; trimmed stdout on
/// success, the command and its stderr on failure (R6: typed error).
fn run_in(
    dir: &std::path::Path,
    bin: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<String, ProposeError> {
    match crate::pr_push::run_labeled("evals-propose", bin, args, dir, timeout)
        .map_err(ProposeError::Io)?
    {
        (true, out, _) => Ok(out.trim().to_string()),
        (false, _, err) => Err(ProposeError::Io(format!(
            "{bin} {}: {}",
            args.join(" "),
            err.trim()
        ))),
    }
}

const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// A golden cohort at 3 repeats and 2 workers runs well under this.
const COHORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4 * 3600);

fn git_out(dir: &std::path::Path, args: &[&str]) -> Result<String, ProposeError> {
    run_in(dir, "git", args, GIT_TIMEOUT)
}

/// The ship outcome for the ledger row (change 4).
#[derive(Debug, PartialEq)]
enum ShipOutcome {
    Opened(String),
    AlreadyOpen,
}

/// One PR per real gain: branch `evals/propose-<date>-<type>`, one commit
/// of the reply's edits, the proof in the body. An open PR on the branch
/// reads already_open and opens nothing (AC4-EDGE).
fn ship_candidate(
    day: &str,
    kind: &str,
    edits: &[(std::path::PathBuf, Vec<u8>)],
    body: &str,
    doors: &Doors,
) -> Result<ShipOutcome, ProposeError> {
    let branch = format!("evals/propose-{day}-{}", kind.replace('_', "-"));
    if (doors.is_open)(&branch)? {
        return Ok(ShipOutcome::AlreadyOpen);
    }
    let title = format!("evals: a proved {kind} fix on the golden tier");
    let url = (doors.open)(&branch, &title, body, edits)?;
    Ok(ShipOutcome::Opened(url))
}

/// The cycle's side effects, injected so a whole cycle runs under test
/// with no model, no bank run, no git and no GitHub.
pub(crate) struct Doors<'a> {
    /// The sealed proposer call: (cluster type, prompt) to (exit, stdout, stderr).
    pub(crate) propose: &'a dyn Fn(&str, &str) -> Result<(i32, String, String), ProposeError>,
    /// One golden cohort run: (cohort, variant, sha).
    pub(crate) run: &'a dyn Fn(&str, &str, &str) -> Result<(), ProposeError>,
    /// Commit edits on a local ref off the base: (tag, edits) to sha.
    pub(crate) commit:
        &'a dyn Fn(&str, &[(std::path::PathBuf, Vec<u8>)]) -> Result<String, ProposeError>,
    pub(crate) open: Opener<'a>,
    pub(crate) is_open: &'a dyn Fn(&str) -> Result<bool, ProposeError>,
}

/// One cycle's fixed inputs.
pub(crate) struct Cycle<'a> {
    pub(crate) bank_dir: &'a std::path::Path,
    pub(crate) history: &'a std::path::Path,
    pub(crate) state: &'a std::path::Path,
    /// origin/main's sha, read once: the base cohort and every candidate
    /// ref start from the same tree.
    pub(crate) base_sha: &'a str,
    pub(crate) cap_usd: f64,
    pub(crate) attempt_usd: f64,
    pub(crate) day: &'a str,
}

/// The typed read of the fold view the PR body prints.
#[derive(Deserialize)]
struct ProofLine {
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    paired: Option<ProofPaired>,
    #[serde(default)]
    noise_floor_points: Option<f64>,
    #[serde(default)]
    noise_source: Value,
}

#[derive(Deserialize)]
struct ProofPaired {
    #[serde(default)]
    mean_delta_points: Option<f64>,
    #[serde(default)]
    ci95_points: Option<[f64; 2]>,
}

/// Set one key on a JSON object row; a non-object row is left as it is.
fn put(row: &mut Value, key: &str, value: Value) {
    if let Some(map) = row.as_object_mut() {
        map.insert(key.to_string(), value);
    }
}

/// The fold view's power block, or null.
fn power_of(view: &Value) -> Value {
    view.pointer("/power").cloned().unwrap_or_default()
}

/// The task ids of one fold list (`fixed`, `regressions`, `errors`).
fn list_tasks(view: &Value, list: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Entry {
        task: String,
    }
    serde_json::from_value::<Vec<Entry>>(
        view.pointer(&format!("/{list}"))
            .cloned()
            .unwrap_or_default(),
    )
    .map(|v| v.into_iter().map(|e| e.task).collect())
    .unwrap_or_default()
}

/// The PR body: cluster, cohorts, the held-out power line, the all-golden
/// fixed and regression lists, and the per-change attribution.
fn proof_body(
    kind: &str,
    base: &str,
    cand: &str,
    held: &Value,
    all: &Value,
    files: usize,
) -> String {
    let p: Option<ProofLine> = serde_json::from_value(power_of(held)).ok();
    let num = |v: Option<f64>| v.map_or_else(|| "unmeasured".to_string(), |m| format!("{m:.1}"));
    let (verdict, mean, ci, floor, source) = match &p {
        Some(p) => (
            p.verdict.clone(),
            num(p.paired.as_ref().and_then(|x| x.mean_delta_points)),
            p.paired.as_ref().and_then(|x| x.ci95_points).map_or_else(
                || "unmeasured".to_string(),
                |[lo, hi]| format!("[{lo:.1}, {hi:.1}]"),
            ),
            num(p.noise_floor_points),
            p.noise_source.as_str().unwrap_or("none").to_string(),
        ),
        None => (
            "unmeasured".into(),
            "unmeasured".into(),
            "unmeasured".into(),
            "unmeasured".into(),
            "none".into(),
        ),
    };
    let fixed = list_tasks(all, "fixed");
    let regressions = list_tasks(all, "regressions");
    let attribution = if files == 1 { "helped" } else { "confounded" };
    format!(
        "The propose-and-prove loop drafted this change for failure type `{kind}` and proved it on the golden tier.\n\n\
         - base cohort: `{base}`\n- candidate cohort: `{cand}`\n\
         - held-out power: {verdict}, paired delta {mean} points, CI95 {ci}, noise floor {floor} points ({source})\n\
         - all goldens fixed: {fixed}\n- all goldens regressed: {reg}\n\
         - attribution: {attribution} ({files} file(s) changed)\n\n\
         A person reads the change before merge. Probation keeps measuring it after merge and opens a revert when it stops paying.",
        fixed = if fixed.is_empty() { "none".into() } else { fixed.join(", ") },
        reg = if regressions.is_empty() { "none".into() } else { regressions.join(", ") },
    )
}

/// One full-file edit set: (repo-relative path, bytes).
type Edits = Vec<(std::path::PathBuf, Vec<u8>)>;

/// The proposer's answer for one cluster, after the tripwires.
enum Proposed {
    KeepBase(String),
    Change(Edits),
}

/// One cluster's proposal: strip check, one sealed call, parse, tripwires.
/// It spends no bank run, so the cycle asks every cluster before the base.
fn propose_cluster(
    doors: &Doors,
    cluster: &Cluster,
    tasks: &[TaskSpec],
) -> Result<Proposed, ProposeError> {
    let input = proposer_input(cluster, tasks);
    if !answer_keys_stripped(&input, tasks) {
        return Err(ProposeError::Reply(
            "answer_key_leak: the proposer input carries a held-out key".into(),
        ));
    }
    let (code, out, err) = (doors.propose)(&cluster.kind, &propose_prompt(cluster, &input))?;
    if code != 0 {
        return Err(ProposeError::Reply(format!(
            "proposer exit {code}: {}",
            err.trim()
        )));
    }
    let reply = parse_reply(&out)?;
    validate_reply(&reply, tasks)?;
    if reply.verdict == "keep_base" {
        return Ok(Proposed::KeepBase(reply.reason));
    }
    Ok(Proposed::Change(
        reply
            .edits
            .into_iter()
            .map(|e| (std::path::PathBuf::from(e.path), e.content.into_bytes()))
            .collect(),
    ))
}

/// One candidate's proof: commit, run, assert the counts, fold, gain,
/// ship. Every outcome is one ledger row; an error becomes a `refused` row
/// in the caller.
#[allow(clippy::too_many_arguments)]
fn prove_candidate(
    c: &Cycle,
    doors: &Doors,
    cluster: &Cluster,
    edits: &Edits,
    base_cohort: &str,
    variant: &str,
    unproved: &BTreeSet<String>,
    plan: &CyclePlan,
) -> Result<Value, ProposeError> {
    let sha = (doors.commit)(&format!("{}/{}", c.day, cluster.kind), edits)?;
    let cand = format!(
        "{COHORT_PREFIX}{}-{}",
        c.day,
        cluster.kind.replace('_', "-")
    );
    (doors.run)(&cand, variant, &sha)?;
    let raw = crate::evals_gate::history_rows(c.history);
    assert_ledger(&raw, base_cohort, std::slice::from_ref(&cand), plan)?;
    let rows = crate::evals_trend::read_rows(&c.history.to_string_lossy(), None, None);
    let held_rows: Vec<Row> = rows
        .iter()
        .filter(|r| split(&r.task_id) == Role::HeldOut)
        .cloned()
        .collect();
    let fold = |rows: &[Row]| {
        crate::evals_compare::fold(
            rows,
            base_cohort,
            &cand,
            crate::evals_gate::REPEATS,
            unproved,
        )
        .map_err(ProposeError::Fold)
    };
    let held = fold(&held_rows)?;
    let all = fold(&rows)?;
    let mut row = serde_json::json!({
        "base": base_cohort, "candidate": cand, "sha": sha,
        "goldens": cluster.failing_tasks, "power": power_of(&held),
        "files": edits.len(),
    });
    let verdict = match gain(&held, &all)? {
        Gain::Ship => {
            let body = proof_body(&cluster.kind, base_cohort, &cand, &held, &all, edits.len());
            match ship_candidate(c.day, &cluster.kind, edits, &body, doors)? {
                ShipOutcome::Opened(url) => {
                    put(
                        &mut row,
                        "learning",
                        Value::from(record_learning(c.state, &url, &held)),
                    );
                    put(&mut row, "pr", Value::String(url));
                    "shipped"
                }
                ShipOutcome::AlreadyOpen => "already_open",
            }
        }
        Gain::Regression => "regression",
        Gain::Underpowered => "underpowered",
        Gain::InsideNoise => "inside_noise",
    };
    put(&mut row, "verdict", Value::from(verdict));
    Ok(row)
}

/// The shipped change's proof row in the learnings ledger (`kind: pr`):
/// the held-out paired delta is the score, so `evals report --learnings`
/// lists the propose loop's gains beside the go-node and persona rows.
/// Returns the ledger's answer for the cluster row; a refusal never undoes
/// the PR.
fn record_learning(state: &std::path::Path, url: &str, held: &Value) -> String {
    let mean = serde_json::from_value::<ProofLine>(power_of(held))
        .ok()
        .and_then(|p| p.paired)
        .and_then(|p| p.mean_delta_points);
    let row = crate::evals_ledger::Learning {
        change: url.to_string(),
        kind: crate::evals_ledger::ChangeKind::Pr,
        trace_ids: Vec::new(),
        golden: None,
        go_node: None,
        score_before: mean.map(|_| 0.0),
        pr: Some(url.to_string()),
        score_after: mean,
        score: Some("held-out paired delta in points against the fresh base cohort".into()),
        outcome: crate::evals_ledger::Outcome::Improved,
        recorded_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    match crate::evals_ledger::append(state, row) {
        Ok(true) => "recorded".into(),
        Ok(false) => "already_recorded".into(),
        Err(e) => format!("refused: {e}"),
    }
}

/// The `--dry-run` receipt: each cluster with its split and the strip flag
/// on the input the proposer would read. It spends nothing.
fn dry_receipt(list: &[Cluster], tasks: &[TaskSpec], split_rule: &str) -> Value {
    let items: Vec<Value> = list
        .iter()
        .map(|cl| {
            let input = proposer_input(cl, tasks);
            serde_json::json!({
                "type": cl.kind,
                "verdict": "dry_run",
                "failing": cl.failing_tasks,
                "train": cl.train,
                "held_out": cl.held_out,
                "proposer_input": {
                    "answer_keys_stripped": answer_keys_stripped(&input, tasks),
                    "chars": input.len(),
                },
            })
        })
        .collect();
    serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "split": split_rule,
        "clusters": items,
    })
}

/// Ask every cluster the cap admits before any cohort runs: a final row
/// (`Ok`) for keep_base, a refusal or a cap drop, or the edits (`Err`) of a
/// candidate still to prove.
fn ask_proposers(
    list: &[Cluster],
    candidates: usize,
    doors: &Doors,
    tasks: &[TaskSpec],
) -> Vec<Result<Value, Edits>> {
    list.iter()
        .enumerate()
        .map(|(i, cluster)| {
            if i >= candidates {
                return Ok(serde_json::json!({"verdict": "dropped_by_cap"}));
            }
            match propose_cluster(doors, cluster, tasks) {
                Ok(Proposed::KeepBase(reason)) => {
                    Ok(serde_json::json!({"verdict": "keep_base", "reason": reason}))
                }
                Ok(Proposed::Change(edits)) => Err(edits),
                Err(e) => Ok(serde_json::json!({"verdict": "refused", "refused": e.to_string()})),
            }
        })
        .collect()
}

/// One propose cycle (changes 1-4): cluster, budget line, fresh base
/// cohort, then per cluster propose, prove, gain and ship. Every cluster
/// lands one ledger row, shipped or not. `dry_run` stops after change 1.
pub(crate) fn run_cycle(c: &Cycle, dry_run: bool, doors: &Doors) -> Result<Value, ProposeError> {
    let tasks =
        crate::evals::bank::discover_bank(c.bank_dir).map_err(|e| ProposeError::Bank(e.0))?;
    let rows = crate::evals_trend::read_rows(&c.history.to_string_lossy(), None, None);
    let list = clusters(&rows, &tasks, c.bank_dir, crate::evals_gate::REPEATS)?;
    let split_rule =
        format!("splitmix64 over the id bytes, seed {SEED}; even is train, odd is held-out");
    if dry_run {
        return Ok(dry_receipt(&list, &tasks, &split_rule));
    }
    let mark = serde_json::json!({
        "kind": "cycle", "cycle": c.day, "ts": chrono::Utc::now().to_rfc3339(),
        "clusters": list.len(),
    });
    ledger_append(c.state, &mark)?;
    if list.is_empty() {
        return Ok(serde_json::json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "clusters": 0, "results": [],
        }));
    }
    let goldens = tasks
        .iter()
        .filter(|t| t.tags.iter().any(|g| g == "golden") && !t.is_demoted())
        .count();
    let mut plan = plan_budget(
        goldens,
        crate::evals_gate::REPEATS,
        list.len(),
        c.cap_usd,
        c.attempt_usd,
    );
    plan.proposer_calls = plan.candidates;
    if plan.candidates == 0 {
        return Err(ProposeError::Budget(format!(
            "cycle cap {:.2} USD leaves no candidate (base plus one prices {:.2})",
            c.cap_usd,
            (plan.base_attempts + plan.per_candidate_attempts) as f64 * c.attempt_usd
        )));
    }
    let budget = serde_json::json!({
        "cycle": c.day, "kind": "budget",
        "base_attempts": plan.base_attempts,
        "candidate_attempts": plan.per_candidate_attempts * plan.candidates,
        "proposer_calls": plan.proposer_calls,
        "estimate_usd": plan.estimate_usd, "cap_usd": c.cap_usd,
    });
    ledger_append(c.state, &budget)?;
    // Ask every cluster first: a cycle whose proposers all keep the base
    // or refuse spends no cohort.
    let slots = ask_proposers(&list, plan.candidates, doors, &tasks);
    let base_cohort = format!("{COHORT_PREFIX}{}-base", c.day);
    let any_candidate = slots.iter().any(Result::is_err);
    if any_candidate {
        (doors.run)(&base_cohort, "v1", c.base_sha)?;
    }
    let unproved = unproved_tasks(c.bank_dir).map_err(ProposeError::Bank)?;
    let mut results: Vec<Value> = Vec::new();
    for (i, (cluster, slot)) in list.iter().zip(slots).enumerate() {
        let mut row = match slot {
            Ok(row) => row,
            Err(edits) => prove_candidate(
                c,
                doors,
                cluster,
                &edits,
                &base_cohort,
                &format!("v{}", i + 2),
                &unproved,
                &plan,
            )
            .unwrap_or_else(
                |e| serde_json::json!({"verdict": "refused", "refused": e.to_string()}),
            ),
        };
        put(&mut row, "cycle", Value::String(c.day.to_string()));
        put(&mut row, "type", Value::String(cluster.kind.clone()));
        ledger_append(c.state, &row)?;
        results.push(row);
    }
    Ok(serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "split": split_rule,
        "base_cohort": base_cohort,
        "base_ran": any_candidate,
        "budget": budget,
        "clusters": results.len(),
        "results": results,
    }))
}

/// Post-merge `gate-main-` cohorts that close probation (change 5).
pub(crate) const PROBATION_COHORTS: usize = 7;
/// Cohorts after which inside_noise retires an unconfounded change.
pub(crate) const PROBATION_MAX: usize = 21;
const GATE_MAIN: &str = "gate-main-";

/// A worker-seat main cohort: `gate-main-<date>-<hash>`. The lead seat runs
/// other tasks, and a fallback cohort runs another lane, so neither pools.
fn worker_main_cohort(cohort: &str) -> bool {
    cohort.starts_with(GATE_MAIN) && !cohort.ends_with("-lead") && !cohort.ends_with("-fallback")
}

/// What probation reads from GitHub and git, injected for tests.
pub(crate) struct ProbationDoors<'a> {
    /// The merge commit of a merged PR, `None` while it is unmerged.
    pub(crate) merged: &'a dyn Fn(&str) -> Result<Option<String>, ProposeError>,
    /// True when tree `rev` contains commit `merge`.
    pub(crate) contains: &'a dyn Fn(&str, &str) -> bool,
    /// Open the revert PR: (branch, title, body, merge sha) to url.
    pub(crate) revert: &'a dyn Fn(&str, &str, &str, &str) -> Result<String, ProposeError>,
}

/// One ledger row, typed (R1): the fields probation reads.
#[derive(Deserialize, Default)]
struct LedgerRow {
    #[serde(default)]
    verdict: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    pr: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    goldens: Vec<String>,
    #[serde(default)]
    probation_of: Option<String>,
}

fn ledger_rows(state: &std::path::Path) -> Vec<LedgerRow> {
    std::fs::read_to_string(ledger_path(state))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The proof's base rows and the pooled post-merge rows, restricted to the
/// goldens graded on both sides: a golden added or removed since the proof
/// is not a regression of this change.
fn pool_rows(rows: &[Row], base: &str, post: &[&Row], pooled: &str) -> Vec<Row> {
    let base_rows: Vec<&Row> = rows
        .iter()
        .filter(|x| x.experiment_id.as_deref() == Some(base))
        .collect();
    let on_base: BTreeSet<&str> = base_rows.iter().map(|x| x.task_id.as_str()).collect();
    let on_post: BTreeSet<&str> = post.iter().map(|x| x.task_id.as_str()).collect();
    let mut fold_rows: Vec<Row> = base_rows
        .iter()
        .filter(|x| on_post.contains(x.task_id.as_str()))
        .map(|x| (*x).clone())
        .collect();
    fold_rows.extend(
        post.iter()
            .filter(|x| on_base.contains(x.task_id.as_str()))
            .map(|x| {
                let mut y = (*x).clone();
                y.experiment_id = Some(pooled.to_string());
                y
            }),
    );
    fold_rows
}

/// Probation's two views (all goldens, held-out goldens) and the goldens
/// with no live grade. A dead or ungraded golden measures the machine, not
/// the change, so both views exclude it; on a cluster golden the caller
/// suspends instead.
fn probation_views(
    fold_rows: &[Row],
    base: &str,
    pooled: &str,
    unproved: &BTreeSet<String>,
) -> Result<(Value, Value, BTreeSet<String>), ProposeError> {
    let fold = |rows: &[Row], excluded: &BTreeSet<String>| {
        crate::evals_compare::fold(rows, base, pooled, crate::evals_gate::REPEATS, excluded)
            .map_err(ProposeError::Fold)
    };
    let first = fold(fold_rows, unproved)?;
    let dead = serde_json::from_value::<BTreeMap<String, usize>>(
        first.pointer("/b/dead_runs").cloned().unwrap_or_default(),
    )
    .unwrap_or_default();
    let errored: BTreeSet<String> = list_tasks(&first, "errors")
        .into_iter()
        .chain(dead.into_keys())
        .collect();
    let mut excluded = unproved.clone();
    excluded.extend(errored.iter().cloned());
    let all = fold(fold_rows, &excluded)?;
    let held_rows: Vec<Row> = fold_rows
        .iter()
        .filter(|x| split(&x.task_id) == Role::HeldOut)
        .cloned()
        .collect();
    let held = fold(&held_rows, &excluded)?;
    Ok((all, held, errored))
}

/// One probation read's verdict: `None` keeps waiting. A suspended change
/// never reads a gain; a regression or a negative move retires; a positive
/// move keeps; past `PROBATION_MAX` cohorts inside_noise retires, unless
/// the window is confounded.
fn probation_verdict(
    suspended: bool,
    held: &Value,
    all: &Value,
    cohorts: usize,
    confounded: bool,
) -> Result<Option<&'static str>, ProposeError> {
    if suspended {
        return Ok(Some("suspended"));
    }
    Ok(match gain(held, all)? {
        Gain::Regression => Some("retired"),
        Gain::Ship => Some("kept"),
        Gain::InsideNoise | Gain::Underpowered if cohorts >= PROBATION_MAX => {
            Some(if confounded { "confounded" } else { "retired" })
        }
        Gain::InsideNoise | Gain::Underpowered => None,
    })
}

/// The revert PR for a retired change (`git revert -m 1 <merge>`).
fn open_revert(
    doors: &ProbationDoors,
    kind: &str,
    pr: &str,
    merge: &str,
    cohorts: usize,
) -> Result<String, ProposeError> {
    let slug = kind.replace('_', "-");
    let branch = format!(
        "evals/propose-revert-{slug}-{}",
        merge.get(..8).unwrap_or(merge)
    );
    let title = format!("evals: revert the {kind} fix, probation retired it");
    let body = format!(
        "Probation retired {pr} after {cohorts} post-merge `gate-main-` cohorts: the held-out goldens no longer show the gain its proof claimed, or a golden regressed. This reverts merge {merge}."
    );
    (doors.revert)(&branch, &title, &body, merge)
}

/// Probation (change 5): each shipped and merged change is measured on the
/// held-out goldens against its proof's base cohort, pooled over the
/// `gate-main-` cohorts whose trees contain the merge. kept, retired and
/// confounded close it; suspended holds it while a cluster golden reads
/// unproved or errors. Returns the rows it appended.
pub(crate) fn probation(
    bank_dir: &std::path::Path,
    history: &std::path::Path,
    state: &std::path::Path,
    doors: &ProbationDoors,
) -> Result<Vec<Value>, ProposeError> {
    let ledger = ledger_rows(state);
    let mut last: BTreeMap<&str, &str> = BTreeMap::new();
    for r in &ledger {
        if let Some(pr) = r.probation_of.as_deref() {
            last.insert(pr, r.verdict.as_str());
        }
    }
    let closed = |pr: &str| matches!(last.get(pr), Some(&("kept" | "retired" | "confounded")));
    let shipped: Vec<&LedgerRow> = ledger
        .iter()
        .filter(|r| r.verdict == "shipped" && r.probation_of.is_none())
        .filter(|r| r.pr.is_some() && r.base.is_some())
        .collect();
    // Merge shas of every shipped PR, read once.
    let mut merges: BTreeMap<&str, String> = BTreeMap::new();
    for r in &shipped {
        let pr = r.pr.as_deref().unwrap_or_default();
        if let Some(sha) = (doors.merged)(pr)? {
            merges.insert(pr, sha);
        }
    }
    let unproved = unproved_tasks(bank_dir).map_err(ProposeError::Bank)?;
    let rows = crate::evals_trend::read_rows(&history.to_string_lossy(), None, None);
    let mut out = Vec::new();
    for r in &shipped {
        let (Some(pr), Some(base)) = (r.pr.as_deref(), r.base.as_deref()) else {
            continue;
        };
        let Some(merge) = merges.get(pr) else {
            continue;
        };
        if closed(pr) {
            continue;
        }
        let post: Vec<&Row> = rows
            .iter()
            .filter(|x| x.experiment_id.as_deref().is_some_and(worker_main_cohort))
            .filter(|x| {
                x.bank_rev
                    .as_deref()
                    .is_some_and(|rev| (doors.contains)(rev, merge))
            })
            .collect();
        let cohorts: BTreeSet<&str> = post
            .iter()
            .filter_map(|x| x.experiment_id.as_deref())
            .collect();
        if cohorts.len() < PROBATION_COHORTS {
            continue;
        }
        let pooled = format!("probation-{}", merge.get(..8).unwrap_or(merge.as_str()));
        let fold_rows = pool_rows(&rows, base, &post, &pooled);
        let (all, held, errored) = probation_views(&fold_rows, base, &pooled, &unproved)?;
        let newest = post
            .iter()
            .filter_map(|x| x.bank_rev.as_deref())
            .last()
            .unwrap_or(merge.as_str());
        let confounded = merges.iter().any(|(other_pr, other)| {
            *other_pr != pr
                && ((doors.contains)(newest, other) && !(doors.contains)(merge, other)
                    || (!closed(*other_pr) && (doors.contains)(merge, other)))
        });
        let suspended = r
            .goldens
            .iter()
            .any(|g| unproved.contains(g) || errored.contains(g));
        let Some(verdict) = probation_verdict(suspended, &held, &all, cohorts.len(), confounded)?
        else {
            continue;
        };
        if verdict == "suspended" && last.get(pr) == Some(&"suspended") {
            continue;
        }
        let mut row = serde_json::json!({
            "probation_of": pr, "type": r.kind, "verdict": verdict,
            "merge": merge, "cohorts": cohorts.len(), "confounded": confounded,
            "power": power_of(&held),
        });
        if verdict == "retired" {
            let url = open_revert(doors, &r.kind, pr, merge, cohorts.len())?;
            put(&mut row, "revert", Value::String(url));
        }
        ledger_append(state, &row)?;
        out.push(row);
    }
    Ok(out)
}

/// Probation over `repo` with the real doors: gh for the merge commit, git
/// for ancestry, and bank_pr for the revert PR.
fn real_probation(
    repo: &std::path::Path,
    bank_dir: &std::path::Path,
    history: &std::path::Path,
    state: &std::path::Path,
) -> Result<Vec<Value>, ProposeError> {
    let merged = |pr: &str| -> Result<Option<String>, ProposeError> {
        let jq = "select(.state == \"MERGED\") | .mergeCommit.oid";
        let args = ["pr", "view", pr, "--json", "state,mergeCommit", "--jq", jq];
        let sha =
            run_in(repo, "gh", &args, GIT_TIMEOUT).map_err(|e| ProposeError::Pr(e.to_string()))?;
        Ok((!sha.is_empty()).then_some(sha))
    };
    let contains = |rev: &str, merge: &str| {
        git_out(repo, &["merge-base", "--is-ancestor", merge, rev]).is_ok()
    };
    let revert = |branch: &str, title: &str, body: &str, merge: &str| {
        crate::bank_pr::open_with(repo, branch, title, body, |tree| {
            git_out(tree, &["revert", "-m", "1", "--no-edit", merge])
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .map_err(ProposeError::Pr)
    };
    let pdoors = ProbationDoors {
        merged: &merged,
        contains: &contains,
        revert: &revert,
    };
    probation(bank_dir, history, state, &pdoors)
}

/// The real doors over `repo`: the sealed reader call, `fno-agent evals
/// run` through `run_cmd`, local refs, bank_pr and gh.
fn real_cycle<'a>(
    repo: &'a std::path::Path,
    run_cmd: &'a dyn Fn(&[String]) -> Result<(), ProposeError>,
    lane: &'a str,
    base_sha: &'a str,
    dry_run: bool,
) -> Result<Value, ProposeError> {
    let state = crate::evals_candidates::state_root();
    let history = crate::evals::history_path();
    let bank_dir = repo.join("evals").join("bank");
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let propose = |kind: &str, prompt: &str| -> Result<(i32, String, String), ProposeError> {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let base = crate::evals::runner::sealed_base(&std::env::temp_dir(), &home)
            .map_err(ProposeError::Io)?;
        let (tree, _) = crate::evals::runner::make_sealed_tree(
            repo,
            base_sha,
            None,
            None,
            "evals-propose",
            &base,
        )
        .map_err(ProposeError::Io)?;
        let route = crate::reader_route::resolve(repo, "evals-propose", None, "sonnet");
        let out = spawn_reader(&format!("evals-propose-{kind}"), prompt, &tree, 600, &route);
        if let Err(e) = std::fs::remove_dir_all(&tree) {
            eprintln!("evals propose: sealed tree {}: {e}", tree.display());
        }
        out
    };
    let run = |cohort: &str, variant: &str, sha: &str| {
        let argv: Vec<String> = [
            "run",
            "--tag",
            "golden",
            "--repeat",
            "3",
            "--workers",
            "2",
            "--cohort",
            cohort,
            "--lane",
            lane,
            "--variant",
            variant,
            "--ref",
            sha,
            "-y",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        run_cmd(&argv)
    };
    let commit = |tag: &str, edits: &[(std::path::PathBuf, Vec<u8>)]| {
        candidate_ref(repo, base_sha, tag, edits)
    };
    let open = |branch: &str, title: &str, body: &str, edits: &[(std::path::PathBuf, Vec<u8>)]| {
        crate::bank_pr::open(repo, branch, title, body, edits).map_err(ProposeError::Pr)
    };
    let is_open = |branch: &str| {
        crate::bank_pr::existing_pr(repo, branch)
            .map(|p| p.is_some())
            .map_err(ProposeError::Pr)
    };
    let doors = Doors {
        propose: &propose,
        run: &run,
        commit: &commit,
        open: &open,
        is_open: &is_open,
    };
    let cycle = Cycle {
        bank_dir: &bank_dir,
        history: &history,
        state: &state,
        base_sha,
        cap_usd: cycle_usd(repo),
        attempt_usd: crate::evals_gate::lane_attempt_usd(repo, lane),
        day: &day,
    };
    if dry_run {
        return run_cycle(&cycle, true, &doors);
    }
    let probation_rows = real_probation(repo, &bank_dir, &history, &state)?;
    let mut receipt = run_cycle(&cycle, false, &doors)?;
    put(&mut receipt, "probation", Value::Array(probation_rows));
    Ok(receipt)
}

/// origin/main's sha in `repo`, fetched first so the base is today's main.
fn origin_main(repo: &std::path::Path) -> Result<String, ProposeError> {
    if let Err(e) = git_out(repo, &["fetch", "-q", "origin", "main"]) {
        eprintln!("evals propose: fetch failed, using the local origin/main: {e}");
    }
    git_out(repo, &["rev-parse", "origin/main"])
}

/// One `fno-agent evals ...` call: the cohort run, refused on a nonzero exit.
fn evals_cmd<'a>(
    exe: &'a std::path::Path,
    repo: &'a std::path::Path,
) -> impl Fn(&[String]) -> Result<(), ProposeError> + 'a {
    move |argv: &[String]| {
        let mut args: Vec<&str> = vec!["evals"];
        args.extend(argv.iter().map(String::as_str));
        run_in(repo, &exe.to_string_lossy(), &args, COHORT_TIMEOUT)
            .map(|_| ())
            .map_err(|e| ProposeError::Io(format!("cohort run refused: {e}")))
    }
}

/// The detached cycle child's inputs (the capability leg's Run shape).
pub(crate) struct Run<'a> {
    pub(crate) leg: Leg<'a>,
}

/// Run mode: the detached child runs probation and one cycle, then
/// releases the claim. The ledger, not an event, is the cycle's journal.
pub(crate) fn run_leg(r: &Run) -> i32 {
    let repo = r.leg.repo;
    let lane = crate::evals_gate::config_lane(repo);
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("fno-agent"));
    let cmd = evals_cmd(&exe, repo);
    let outcome = origin_main(repo).and_then(|sha| real_cycle(repo, &cmd, &lane, &sha, false));
    release_claim(&std::process::id().to_string(), r.leg.claims_root);
    match outcome {
        Ok(receipt) => {
            eprintln!("evals propose: {receipt}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// The manual door: `fno-agent evals drafts changes [--dry-run] [--json]`
/// runs probation and one cycle now; `--dry-run` stops after the cluster
/// pick and spends nothing.
pub fn changes_cli(args: &[String]) -> i32 {
    if let Some(bad) = args
        .iter()
        .find(|a| a.as_str() != "--dry-run" && !crate::json_output::is_flag(a))
    {
        eprintln!(
            "evals drafts changes: unknown argument {bad:?}; usage: changes [--dry-run] [--json|-J]"
        );
        return 2;
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let json_out = crate::json_output::requested(args);
    let Some(repo) = crate::golden_drafter::bank_repo() else {
        eprintln!("evals drafts changes: no bank checkout found");
        return 2;
    };
    let lane = crate::evals_gate::config_lane(&repo);
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("fno-agent"));
    let cmd = evals_cmd(&exe, &repo);
    let root = crate::claims::global_claims_root();
    let holder = format!("drafts-changes:{}", std::process::id());
    if !dry_run {
        if let (crate::claims::ClaimState::Live, _) =
            crate::claims::status(CLAIM_KEY, root.as_deref())
        {
            eprintln!("evals drafts changes: a propose cycle is in flight (claim {CLAIM_KEY}); wait for it to settle");
            return 3;
        }
        let opts = crate::claims::AcquireOpts {
            host: None,
            pid: Some(std::process::id()),
            pid_unavailable: false,
            ttl_ms: Some(6 * 3600 * 1000),
            reason: Some("evals propose cycle (manual)".into()),
            metadata: None,
            pid_provenance: None,
            identity: None,
            root: root.clone(),
            events_dir: None,
        };
        let _ = crate::claims::acquire(CLAIM_KEY, &holder, opts);
    }
    let base = if dry_run {
        Ok(String::new())
    } else {
        origin_main(&repo)
    };
    let outcome = base.and_then(|sha| real_cycle(&repo, &cmd, &lane, &sha, dry_run));
    if !dry_run {
        release_claim(&holder, root.as_deref());
    }
    match outcome {
        Ok(receipt) => {
            let text = if json_out {
                serde_json::to_string(&receipt)
            } else {
                serde_json::to_string_pretty(&receipt)
            };
            println!("{}", text.unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("evals drafts changes: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One field by JSON pointer, null when absent.
    fn at<'a>(v: &'a Value, ptr: &str) -> &'a Value {
        v.pointer(ptr).unwrap_or(&Value::Null)
    }
    use crate::evals::bank::load_task;

    fn view(verdict: &str, mean: Option<f64>, breaking: bool, regressions: usize) -> Value {
        serde_json::json!({
            "breaking": breaking,
            "regressions": vec![serde_json::json!({}); regressions],
            "power": {"verdict": verdict, "paired": {"mean_delta_points": mean}},
        })
    }

    fn row(id: &str, cohort: &str) -> Value {
        serde_json::json!({"attempt_id": id, "experiment_id": cohort})
    }

    fn gain_reads_the_rule() {
        let clean = view("inside_noise", Some(0.0), false, 0);
        let negative = view("beats_noise", Some(-30.0), false, 0);
        assert_eq!(gain(&negative, &clean).unwrap(), Gain::Regression);
        let positive = view("beats_noise", Some(25.0), false, 0);
        assert_eq!(gain(&positive, &clean).unwrap(), Gain::Ship);
        let broken = view("beats_noise", Some(0.0), true, 1);
        assert_eq!(gain(&positive, &broken).unwrap(), Gain::Regression);
        let thin = view("underpowered", None, false, 0);
        assert_eq!(gain(&thin, &clean).unwrap(), Gain::Underpowered);
        let quiet = view("inside_noise", Some(2.0), false, 0);
        assert_eq!(gain(&quiet, &clean).unwrap(), Gain::InsideNoise);
    }

    fn the_ledger_refuses_a_count_mismatch_and_a_duplicate() {
        let planned = CyclePlan {
            base_attempts: 2,
            per_candidate_attempts: 2,
            candidates: 1,
            proposer_calls: 0,
            estimate_usd: 0.0,
        };
        let cands = vec!["cand".to_string()];
        let ok_rows = vec![
            row("a1", "base"),
            row("a2", "base"),
            row("c1", "cand"),
            row("c2", "cand"),
        ];
        assert_ledger(&ok_rows, "base", &cands, &planned).unwrap();
        let extra = vec![
            row("a1", "base"),
            row("a2", "base"),
            row("a3", "base"),
            row("c1", "cand"),
            row("c2", "cand"),
        ];
        let err = assert_ledger(&extra, "base", &cands, &planned).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("count_mismatch"), "{msg}");
        assert!(msg.contains("holds 3 rows, planned 2"), "{msg}");
        let dup_rows = vec![
            row("a1", "base"),
            row("a1", "base"),
            row("c1", "cand"),
            row("c2", "cand"),
        ];
        let err = assert_ledger(&dup_rows, "base", &cands, &planned).unwrap_err();
        assert!(format!("{err}").contains("duplicate"), "{err}");
        let none = vec![
            row("a1", "base"),
            serde_json::json!({"experiment_id": "base"}),
            row("c1", "cand"),
            row("c2", "cand"),
        ];
        let err = assert_ledger(&none, "base", &cands, &planned).unwrap_err();
        assert!(format!("{err}").contains("no attempt id"), "{err}");
    }

    fn the_cap_drops_candidates_before_the_base_refuses() {
        let p = plan_budget(5, 3, 2, 1.0, 0.04);
        assert_eq!(p.candidates, 0);
        assert!((p.estimate_usd - 0.6).abs() < 1e-9);
        let keep = plan_budget(5, 3, 2, 2.0, 0.04);
        assert_eq!(keep.candidates, 2);
        assert!((keep.estimate_usd - 1.8).abs() < 1e-9);
    }

    const BANK_A: &str = "id: golden-answer-the-question\ntier: capability\nprompt: p-a\ngrade:\n  - kind: file-exists\n    path: reply.md\ntimeout_minutes: 5\ntags: [golden]\ngolden:\n  source: {kind: correction, ref: \"c:1\"}\n  quote: quote-a\n  expected: expected-a\n  rule: rule-a\n  failure_type: t1\n  cost: trust\n  outcome: failed\n";
    const BANK_B: &str = "id: golden-agent-decide-no-operator-authority\ntier: capability\nprompt: prompt-b words here\ngrade:\n  - kind: exit\n    command: \"grep -q zb reply.md\"\n  - kind: grep\n    path: reply.md\n    pattern: zb-marker\ntimeout_minutes: 5\ntags: [golden]\ngolden:\n  source: {kind: correction, ref: \"c:2\"}\n  quote: quote-b\n  expected: expected-b\n  rule: rule-b\n  failure_type: t1\n  cost: trust\n  outcome: failed\n";
    const FIXTURE_OK: &str = "cases:\n  - {id: g1, label: good, files: {\"reply.md\": \"yes\\n\"}}\n  - {id: g2, label: good, files: {\"reply.md\": \"yes 2\\n\"}}\n  - {id: g3, label: good, files: {\"reply.md\": \"yes 3\\n\"}}\n  - {id: b1, label: bad, files: {}}\n  - {id: b2, label: bad, files: {}}\n  - {id: b3, label: bad, files: {}}\n";
    const FIXTURE_B: &str = "cases:\n  - {id: g1, label: good, files: {\"reply.md\": \"zb-marker\\n\"}}\n  - {id: g2, label: good, files: {\"reply.md\": \"zb-marker 2\\n\"}}\n  - {id: g3, label: good, files: {\"reply.md\": \"zb-marker 3\\n\"}}\n  - {id: b1, label: bad, files: {}}\n  - {id: b2, label: bad, files: {\"reply.md\": \"no\\n\"}}\n  - {id: b3, label: bad, files: {}}\n";
    const CATALOG: &str =
        "cost_order: [trust]\ntypes:\n  - {id: t1, cost: trust, description: type one}\n";

    /// The split is preregistered (D2982): these two roles were
    /// measured 2026-11-20 at SEED 20_261_102 and must never move.
    fn the_split_is_preregistered() {
        assert_eq!(split("golden-answer-the-question"), Role::Train);
        assert_eq!(
            split("golden-agent-decide-no-operator-authority"),
            Role::HeldOut
        );
    }

    struct BankFixture {
        dir: tempfile::TempDir,
        tasks: Vec<TaskSpec>,
        rows: Vec<Row>,
    }

    fn write_bank(extra_unproved: bool) -> BankFixture {
        let dir = tempfile::tempdir().unwrap();
        let bank = dir.path().join("evals").join("bank");
        let metrics = dir.path().join("evals").join("fixtures").join("metrics");
        std::fs::create_dir_all(&bank).unwrap();
        std::fs::create_dir_all(&metrics).unwrap();
        std::fs::write(bank.join("failure-types.yaml"), CATALOG).unwrap();
        for (name, body, fixture) in [
            ("golden-answer-the-question.yaml", BANK_A, FIXTURE_OK),
            (
                "golden-agent-decide-no-operator-authority.yaml",
                BANK_B,
                FIXTURE_B,
            ),
        ] {
            std::fs::write(bank.join(name), body).unwrap();
            let id = name.trim_end_matches(".yaml");
            std::fs::write(metrics.join(format!("{id}.yaml")), fixture).unwrap();
        }
        if extra_unproved {
            let body = BANK_B
                .replace(
                    "golden-agent-decide-no-operator-authority",
                    "golden-unproved-type",
                )
                .replace("failure_type: t1", "failure_type: t2");
            std::fs::write(bank.join("golden-unproved-type.yaml"), body).unwrap();
            let catalog = format!("{CATALOG}  - {{id: t2, cost: trust, description: type two}}\n");
            std::fs::write(bank.join("failure-types.yaml"), catalog).unwrap();
            // No metrics fixture: t2 never proves, so it never picks.
        }
        let mut tasks = Vec::new();
        for entry in std::fs::read_dir(&bank).unwrap().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "yaml")
                && p.file_name().is_some_and(|n| n != "failure-types.yaml")
            {
                tasks.push(load_task(&p).unwrap());
            }
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        let mut history = String::new();
        for task in &tasks {
            for n in 0..3 {
                history.push_str(&format!(
                    "{{\"task_id\":\"{}\",\"tier\":\"capability\",\"pass\":false,\"status\":\"graded\",\"ts\":\"2026-11-1{}T00:00:00Z\",\"experiment_id\":\"c1\",\"attempt_id\":\"{}-{n}\"}}\n",
                    task.id,
                    n + 8,
                    task.id
                ));
            }
        }
        let history_path = dir.path().join("history.jsonl");
        std::fs::write(&history_path, &history).unwrap();
        let rows = crate::evals_trend::read_rows(history_path.to_str().unwrap(), None, None);
        BankFixture { dir, tasks, rows }
    }

    /// AC1-HP: the input carries the train golden's rule and quote and no
    /// field of the held-out golden, and the strip flag reads true. The
    /// held-out golden here leaks on purpose-shaped keys (prompt words,
    /// grade command, pattern, expected) that the scan must catch.
    fn proposer_input_carries_train_only_and_strips_answer_keys() {
        let f = write_bank(false);
        let bank_dir = f.dir.path().join("evals").join("bank");
        let list = clusters(&f.rows, &f.tasks, &bank_dir, 3).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        let c = list.first().unwrap();
        assert_eq!(c.kind, "t1");
        assert_eq!(c.train, vec!["golden-answer-the-question".to_string()]);
        assert_eq!(
            c.held_out,
            vec!["golden-agent-decide-no-operator-authority".to_string()]
        );
        let text = proposer_input(c, &f.tasks);
        assert!(text.contains("rule-a"), "{text}");
        assert!(text.contains("quote-a"), "{text}");
        assert!(!text.contains("rule-b"), "{text}");
        assert!(!text.contains("expected-b"), "{text}");
        assert!(!text.contains("prompt-b"), "{text}");
        assert!(!text.contains("zb-marker"), "{text}");
        assert!(answer_keys_stripped(&text, &f.tasks));
        // The tripwire's own arm: the same text with a held-out key pasted
        // in flips the flag, so a true strip is never vacuous.
        let leaked = format!("{text}\nexpected-b\n");
        assert!(!answer_keys_stripped(&leaked, &f.tasks));
    }

    /// AC1-EDGE: a type whose golden is unproved never picks.
    fn an_unproved_type_never_picks() {
        let f = write_bank(true);
        let bank_dir = f.dir.path().join("evals").join("bank");
        let list = clusters(&f.rows, &f.tasks, &bank_dir, 3).unwrap();
        assert!(
            list.iter().all(|c| c.kind != "t2"),
            "unproved t2 picked: {list:?}"
        );
        assert_eq!(list.len(), 1, "{list:?}");
    }

    /// A cluster whose goldens all drew held-out reads the description and
    /// the case quotes, never a rule or an expected answer.
    fn a_held_out_only_cluster_reads_quotes_only() {
        let f = write_bank(false);
        let c = Cluster {
            kind: "t1".into(),
            description: "type one".into(),
            cost: "trust".into(),
            failing_tasks: vec!["golden-agent-decide-no-operator-authority".into()],
            train: Vec::new(),
            held_out: vec!["golden-agent-decide-no-operator-authority".into()],
        };
        let text = proposer_input(&c, &f.tasks);
        assert!(text.contains("case quote"), "{text}");
        assert!(text.contains("quote-b"), "{text}");
        assert!(!text.contains("rule-b"), "{text}");
        assert!(!text.contains("expected-b"), "{text}");
        assert!(answer_keys_stripped(&text, &f.tasks));
    }

    fn key_task() -> TaskSpec {
        TaskSpec {
            id: "golden-key-holder".into(),
            tier: "capability".into(),
            grade: vec![crate::evals::bank::GradeCheck {
                kind: "exit".into(),
                command: Some("grep -q yes the reply file inside the tree".into()),
                expect: 0,
                path: None,
                pattern: None,
                max_tool_calls: None,
                tests: None,
                rubric: None,
            }],
            prompt: Some("one two three four five six seven eight nine ten".into()),
            repo_fixture: "HEAD".into(),
            timeout_minutes: 5,
            tags: vec!["golden".into()],
            covers: Vec::new(),
            golden: Some(crate::evals::bank::Golden {
                source_kind: "correction".into(),
                source_ref: "c:9".into(),
                quote: "the reply was wrong".into(),
                expected: "run the doctor command".into(),
                rule: "cite the rule".into(),
                failure_type: "t1".into(),
                cost: None,
                outcome: "failed".into(),
            }),
            source_path: std::path::PathBuf::new(),
        }
    }

    fn reply_with_edits(edits: &[(&str, &str)]) -> Reply {
        let edits: Vec<serde_json::Value> = edits
            .iter()
            .map(|(p, c)| serde_json::json!({"path": p, "content": c}))
            .collect();
        let body = serde_json::json!({
            "verdict": "change",
            "kind": "procedure",
            "edits": edits,
            "reason": "the rule moved",
        });
        parse_reply(&body.to_string()).unwrap()
    }

    fn a_change_reply_validates_and_keep_base_passes() {
        let tasks = vec![key_task()];
        let good = reply_with_edits(&[("AGENTS.md", "the new full file text")]);
        assert_eq!(good.verdict, "change");
        assert!(validate_reply(&good, &tasks).is_ok());
        let keep = parse_reply(
            &serde_json::json!({
                "verdict": "keep_base",
                "kind": "procedure",
                "edits": [],
                "reason": "no argued change",
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(keep.verdict, "keep_base");
        assert!(validate_reply(&keep, &tasks).is_ok());
        let garbage = parse_reply("not json at all");
        assert!(garbage.is_err());
    }

    /// AC2-HP: an edit quoting 8 consecutive words of a golden prompt
    /// refuses with answer_key_quote and names the golden id.
    fn an_edit_quoting_an_answer_key_refuses_and_names_the_golden() {
        let tasks = vec![key_task()];
        let quoted = reply_with_edits(&[(
            "AGENTS.md",
            "lead in words one two three four five six seven eight nine ten and a tail",
        )]);
        let err = validate_reply(&quoted, &tasks).unwrap_err();
        assert!(format!("{err}").contains("answer_key_quote"), "{err}");
        assert!(format!("{err}").contains("golden-key-holder"), "{err}");
    }

    fn edit_paths_outside_the_allowlist_refuse() {
        let tasks = vec![key_task()];
        for path in [
            "evals/bank/golden-x.yaml",
            "skills/foo/evals/case.yaml",
            "crates/x/src/lib.rs",
            "CLAUDE.md",
        ] {
            let bad = reply_with_edits(&[(path, "harmless new text")]);
            assert!(validate_reply(&bad, &tasks).is_err(), "{path}");
        }
        for path in [
            "AGENTS.md",
            "docs/x.md",
            "docs/sub/deep.md",
            "skills/foo/SKILL.md",
            "skills/foo/references/page.md",
        ] {
            let ok = reply_with_edits(&[(path, "harmless new text")]);
            assert!(validate_reply(&ok, &tasks).is_ok(), "{path}");
        }
    }

    fn more_than_three_files_refuse() {
        let tasks = vec![key_task()];
        let bad = reply_with_edits(&[
            ("docs/a.md", "t"),
            ("docs/b.md", "t"),
            ("docs/c.md", "t"),
            ("docs/d.md", "t"),
        ]);
        assert!(validate_reply(&bad, &tasks).is_err());
    }

    /// Held-out task ids for synthetic cohort rows: the fold reads rows,
    /// not bank files, so the power block sees as many tasks as the test
    /// writes.
    fn held_ids(n: usize) -> Vec<String> {
        (0..)
            .map(|k| format!("h-{k}"))
            .filter(|id| split(id) == Role::HeldOut)
            .take(n)
            .collect()
    }

    fn history_line(task: &str, cohort: &str, pass: bool, rev: &str) -> String {
        format!(
            "{{\"task_id\":\"{task}\",\"tier\":\"capability\",\"pass\":{pass},\"status\":\"graded\",\"ts\":\"2026-11-20T00:00:00Z\",\"experiment_id\":\"{cohort}\",\"attempt_id\":\"{cohort}-{task}\",\"bank_rev\":\"{rev}\"}}\n"
        )
    }

    fn append(path: &std::path::Path, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// A bank with two proved failing types (t1, t3) and three goldens, so
    /// each cohort holds 9 attempts.
    fn two_type_bank() -> BankFixture {
        let f = write_bank(false);
        let bank = f.dir.path().join("evals").join("bank");
        let metrics = f.dir.path().join("evals").join("fixtures").join("metrics");
        let third = BANK_A
            .replace("golden-answer-the-question", "golden-third-type")
            .replace("failure_type: t1", "failure_type: t3");
        std::fs::write(bank.join("golden-third-type.yaml"), third).unwrap();
        std::fs::write(metrics.join("golden-third-type.yaml"), FIXTURE_OK).unwrap();
        let catalog = format!("{CATALOG}  - {{id: t3, cost: trust, description: type three}}\n");
        std::fs::write(bank.join("failure-types.yaml"), catalog).unwrap();
        let history = f.dir.path().join("history.jsonl");
        for n in 0..3 {
            append(
                &history,
                &history_line("golden-third-type", &format!("c1-{n}"), false, "r0"),
            );
        }
        f
    }

    struct CycleOut {
        receipt: Value,
        runs: Vec<String>,
        prs: usize,
        ledger: Vec<Value>,
    }

    /// One cycle over `two_type_bank`: the base fails every task, t1's
    /// candidate passes every task, t3's reply is `t3_reply`.
    fn cycle_with(t3_reply: &str) -> CycleOut {
        let f = two_type_bank();
        let root = f.dir.path();
        let history = root.join("history.jsonl");
        let state = tempfile::tempdir().unwrap();
        let ids = held_ids(9);
        let runs = std::cell::RefCell::new(Vec::new());
        let prs = std::cell::Cell::new(0usize);
        let change = r#"{"verdict":"change","kind":"procedure","edits":[{"path":"docs/x.md","content":"a change"}],"reason":"r"}"#;
        let propose = |kind: &str, _: &str| -> Result<(i32, String, String), ProposeError> {
            let out = if kind == "t3" { t3_reply } else { change };
            Ok((0, out.to_string(), String::new()))
        };
        let run = |cohort: &str, _: &str, _: &str| -> Result<(), ProposeError> {
            runs.borrow_mut().push(cohort.to_string());
            let pass = cohort.ends_with("-t1");
            let text: String = ids
                .iter()
                .map(|id| history_line(id, cohort, pass, "r1"))
                .collect();
            append(&history, &text);
            Ok(())
        };
        let commit = |tag: &str,
                      _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { Ok(format!("sha-{tag}")) };
        let open = |_: &str,
                    _: &str,
                    _: &str,
                    _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> {
            prs.set(prs.get() + 1);
            Ok("url-1".into())
        };
        let is_open = |_: &str| -> Result<bool, ProposeError> { Ok(false) };
        let doors = Doors {
            propose: &propose,
            run: &run,
            commit: &commit,
            open: &open,
            is_open: &is_open,
        };
        let bank_dir = root.join("evals").join("bank");
        let cycle = Cycle {
            bank_dir: &bank_dir,
            history: &history,
            state: state.path(),
            base_sha: "base",
            cap_usd: 10.0,
            attempt_usd: 0.04,
            day: "2026-11-20",
        };
        let receipt = run_cycle(&cycle, false, &doors).unwrap();
        let ledger = std::fs::read_to_string(ledger_path(state.path()))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        CycleOut {
            receipt,
            runs: runs.into_inner(),
            prs: prs.get(),
            ledger,
        }
    }

    fn verdict_of<'a>(ledger: &'a [Value], kind: &str) -> &'a str {
        ledger
            .iter()
            .find(|r| at(r, "/type") == kind)
            .and_then(|r| at(r, "/verdict").as_str())
            .unwrap_or("missing")
    }

    /// AC4-HP: one cluster proves a gain and one does not; exactly one PR
    /// opens and each cluster lands one ledger row beside the budget line.
    /// The non-ship cluster reads underpowered: a 9-attempt cohort cannot
    /// read inside_noise without a regression, and both ship nothing.
    fn a_cycle_opens_one_pr_and_lands_a_row_per_cluster() {
        let change = r#"{"verdict":"change","kind":"knowledge","edits":[{"path":"AGENTS.md","content":"b"}],"reason":"r"}"#;
        let out = cycle_with(change);
        assert_eq!(out.prs, 1, "{}", out.receipt);
        assert_eq!(verdict_of(&out.ledger, "t1"), "shipped");
        let t1 = out.ledger.iter().find(|r| at(r, "/type") == "t1").unwrap();
        assert_eq!(at(t1, "/learning"), "recorded");
        assert_eq!(verdict_of(&out.ledger, "t3"), "underpowered");
        let cluster_rows = out
            .ledger
            .iter()
            .filter(|r| at(r, "/type").is_string())
            .count();
        assert_eq!(cluster_rows, 2);
        let budget = out
            .ledger
            .iter()
            .find(|r| at(r, "/kind") == "budget")
            .unwrap();
        assert_eq!(at(budget, "/base_attempts"), 9);
        assert_eq!(
            out.runs.first().map(String::as_str),
            Some("propose-2026-11-20-base")
        );
    }

    /// AC2-EDGE: a keep_base reply records keep_base and runs no candidate
    /// cohort.
    fn keep_base_runs_no_candidate_cohort() {
        let keep =
            r#"{"verdict":"keep_base","kind":"procedure","edits":[],"reason":"no change argued"}"#;
        let out = cycle_with(keep);
        assert_eq!(verdict_of(&out.ledger, "t3"), "keep_base");
        assert!(
            !out.runs.iter().any(|c| c.ends_with("-t3")),
            "{:?}",
            out.runs
        );
        assert_eq!(out.runs.len(), 2, "{:?}", out.runs);
    }

    struct ProbationOut {
        rows: Vec<Value>,
        reverts: usize,
        again: Vec<Value>,
    }

    /// Probation over one shipped change whose cluster golden is `golden`:
    /// the proof's base passes every held-out task, the seven post-merge
    /// gate-main cohorts fail every one.
    fn probation_with(bank: &BankFixture, golden: &str) -> ProbationOut {
        let root = bank.dir.path();
        let history = root.join("history.jsonl");
        let state = tempfile::tempdir().unwrap();
        let shipped = serde_json::json!({
            "cycle": "2026-11-01", "type": "t1", "verdict": "shipped", "pr": "url-1",
            "base": "propose-2026-11-01-base", "goldens": [golden],
        });
        ledger_append(state.path(), &shipped).unwrap();
        let ids = held_ids(9);
        let mut text: String = ids
            .iter()
            .map(|id| history_line(id, "propose-2026-11-01-base", true, "r0"))
            .collect();
        for k in 1..=PROBATION_COHORTS {
            let cohort = format!("{GATE_MAIN}2026-11-0{k}");
            text.extend(
                ids.iter()
                    .map(|id| history_line(id, &cohort, false, &format!("rev-{k}"))),
            );
        }
        append(&history, &text);
        let reverts = std::cell::Cell::new(0usize);
        let merged = |_: &str| -> Result<Option<String>, ProposeError> { Ok(Some("m1".into())) };
        let contains = |_: &str, _: &str| true;
        let revert = |_: &str, _: &str, _: &str, _: &str| -> Result<String, ProposeError> {
            reverts.set(reverts.get() + 1);
            Ok("revert-url".into())
        };
        let doors = ProbationDoors {
            merged: &merged,
            contains: &contains,
            revert: &revert,
        };
        let bank_dir = root.join("evals").join("bank");
        let rows = probation(&bank_dir, &history, state.path(), &doors).unwrap();
        let again = probation(&bank_dir, &history, state.path(), &doors).unwrap();
        ProbationOut {
            rows,
            reverts: reverts.get(),
            again,
        }
    }

    /// AC6-HP: seven post-merge cohorts that lost the gain open one revert
    /// PR and record retired; a second pass opens nothing more.
    fn probation_retires_a_change_that_stopped_paying() {
        let f = write_bank(false);
        let out = probation_with(&f, "golden-answer-the-question");
        assert_eq!(out.reverts, 1);
        assert_eq!(out.rows.len(), 1);
        let row = out.rows.first().unwrap();
        assert_eq!(at(row, "/verdict"), "retired");
        assert_eq!(at(row, "/revert"), "revert-url");
        assert!(out.again.is_empty(), "{:?}", out.again);
    }

    /// AC6-EDGE: a cluster golden that turned unproved holds the change as
    /// suspended and opens nothing.
    fn probation_suspends_on_an_unproved_golden() {
        let f = write_bank(true);
        let out = probation_with(&f, "golden-unproved-type");
        assert_eq!(out.reverts, 0);
        assert_eq!(at(out.rows.first().unwrap(), "/verdict"), "suspended");
        assert!(
            out.again.is_empty(),
            "a repeat suspended row is not appended twice"
        );
    }

    fn ship_reads_already_open_and_opens_one_pr_otherwise() {
        let edits: Vec<(std::path::PathBuf, Vec<u8>)> =
            vec![(std::path::PathBuf::from("docs/x.md"), b"x".to_vec())];
        let fail_propose =
            |_: &str, _: &str| -> Result<(i32, String, String), ProposeError> { unreachable!() };
        let fail_run = |_: &str, _: &str, _: &str| -> Result<(), ProposeError> { unreachable!() };
        let fail_commit = |_: &str,
                           _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { unreachable!() };
        let opener = |_: &str,
                      _: &str,
                      _: &str,
                      _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { Ok("url-1".into()) };
        let closed = |_: &str| -> Result<bool, ProposeError> { Ok(false) };
        let doors = Doors {
            propose: &fail_propose,
            run: &fail_run,
            commit: &fail_commit,
            open: &opener,
            is_open: &closed,
        };
        let out = ship_candidate("2026-11-20", "t1", &edits, "body", &doors).unwrap();
        assert_eq!(out, ShipOutcome::Opened("url-1".into()));
        let never = |_: &str,
                     _: &str,
                     _: &str,
                     _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { panic!("opener must not fire") };
        let open = |_: &str| -> Result<bool, ProposeError> { Ok(true) };
        let doors = Doors {
            propose: &fail_propose,
            run: &fail_run,
            commit: &fail_commit,
            open: &never,
            is_open: &open,
        };
        let out = ship_candidate("2026-11-20", "t2", &edits, "body", &doors).unwrap();
        assert_eq!(out, ShipOutcome::AlreadyOpen);
    }

    /// The dry-run receipt (the done_probes contract): clusters with roles
    /// and the strip flag, no model call and no cohort run.
    fn dry_run_prints_clusters_and_calls_no_model() {
        let f = write_bank(false);
        let repo = f.dir.path();
        let history = repo.join("history.jsonl");
        let state = tempfile::tempdir().unwrap();
        let propose = |_: &str, _: &str| -> Result<(i32, String, String), ProposeError> {
            panic!("no model call in a dry run")
        };
        let run = |_: &str, _: &str, _: &str| -> Result<(), ProposeError> {
            panic!("no cohort run in a dry run")
        };
        let commit = |_: &str,
                      _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { panic!("no commit in a dry run") };
        let open = |_: &str,
                    _: &str,
                    _: &str,
                    _: &[(std::path::PathBuf, Vec<u8>)]|
         -> Result<String, ProposeError> { panic!("no PR in a dry run") };
        let is_open = |_: &str| -> Result<bool, ProposeError> { panic!("no PR read in a dry run") };
        let doors = Doors {
            propose: &propose,
            run: &run,
            commit: &commit,
            open: &open,
            is_open: &is_open,
        };
        let bank_dir = repo.join("evals").join("bank");
        let cycle = Cycle {
            bank_dir: &bank_dir,
            history: &history,
            state: state.path(),
            base_sha: "",
            cap_usd: 10.0,
            attempt_usd: 0.04,
            day: "2026-11-20",
        };
        let receipt = run_cycle(&cycle, true, &doors).unwrap();
        let c = at(&receipt, "/clusters/0");
        assert_eq!(at(c, "/type"), "t1");
        assert_eq!(at(c, "/verdict"), "dry_run");
        assert_eq!(at(c, "/proposer_input/answer_keys_stripped"), true);
        assert!(at(&receipt, "/generated_at").as_str().is_some());
    }

    /// One declaration for the module's contracts. Each helper runs under
    /// `catch_unwind`, so one red run names every broken contract at once.
    #[test]
    fn all_contracts_in_one_declaration() {
        let checks: [(&str, fn()); 17] = [
            ("the_split_is_preregistered", the_split_is_preregistered),
            (
                "proposer_input_carries_train_only_and_strips_answer_keys",
                proposer_input_carries_train_only_and_strips_answer_keys,
            ),
            ("an_unproved_type_never_picks", an_unproved_type_never_picks),
            (
                "a_held_out_only_cluster_reads_quotes_only",
                a_held_out_only_cluster_reads_quotes_only,
            ),
            (
                "dry_run_prints_clusters_and_calls_no_model",
                dry_run_prints_clusters_and_calls_no_model,
            ),
            (
                "a_change_reply_validates_and_keep_base_passes",
                a_change_reply_validates_and_keep_base_passes,
            ),
            (
                "an_edit_quoting_an_answer_key_refuses_and_names_the_golden",
                an_edit_quoting_an_answer_key_refuses_and_names_the_golden,
            ),
            (
                "edit_paths_outside_the_allowlist_refuse",
                edit_paths_outside_the_allowlist_refuse,
            ),
            ("more_than_three_files_refuse", more_than_three_files_refuse),
            ("gain_reads_the_rule", gain_reads_the_rule),
            (
                "the_ledger_refuses_a_count_mismatch_and_a_duplicate",
                the_ledger_refuses_a_count_mismatch_and_a_duplicate,
            ),
            (
                "the_cap_drops_candidates_before_the_base_refuses",
                the_cap_drops_candidates_before_the_base_refuses,
            ),
            (
                "a_cycle_opens_one_pr_and_lands_a_row_per_cluster",
                a_cycle_opens_one_pr_and_lands_a_row_per_cluster,
            ),
            (
                "keep_base_runs_no_candidate_cohort",
                keep_base_runs_no_candidate_cohort,
            ),
            (
                "ship_reads_already_open_and_opens_one_pr_otherwise",
                ship_reads_already_open_and_opens_one_pr_otherwise,
            ),
            (
                "probation_retires_a_change_that_stopped_paying",
                probation_retires_a_change_that_stopped_paying,
            ),
            (
                "probation_suspends_on_an_unproved_golden",
                probation_suspends_on_an_unproved_golden,
            ),
        ];
        let failed: Vec<&str> = checks
            .iter()
            .filter(|(_, check)| std::panic::catch_unwind(*check).is_err())
            .map(|(name, _)| *name)
            .collect();
        assert!(failed.is_empty(), "failed contracts: {failed:?}");
    }
}
