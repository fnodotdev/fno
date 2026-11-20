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
//! copying them (x-6e14 route 1). Candidate surfaces are repo text only
//! (law d-78f7daea): hooks, routing and fnh wait on their own nodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::reader_route::ReaderRoute;

use crate::evals::bank::TaskSpec;
use crate::evals::failure_types::{prove, Catalog};
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

/// Typed failures of one propose cycle (R6: no new `Result<_, String>` in
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
/// preregistered rule (law d-d2bc0f62): the proposer reads only train
/// goldens, and a change ships only on held-out proof with the whole tier
/// unregressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Train,
    HeldOut,
}

impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Role::Train => "train",
            Role::HeldOut => "held-out",
        }
    }
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
    if words.len() < 8 {
        return Vec::new();
    }
    (0..=words.len() - 8)
        .map(|i| words[i..i + 8].join(" ").to_lowercase())
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
                "edit path {:?} is outside AGENTS.md, docs/, skills/*/SKILL.md and skills/*/references/ (q-38521bc5)",
                edit.path
            )));
        }
    }
    // The 8-word run: per golden, so a refusal names the id (AC2-HP).
    for (id, keys) in golden_key_grams(tasks) {
        for edit in &reply.edits {
            let words: Vec<&str> = edit.content.split_whitespace().collect();
            if words.len() < 8 {
                continue;
            }
            for i in 0..=words.len() - 8 {
                if keys.contains(&words[i..i + 8].join(" ").to_lowercase()) {
                    return Err(ProposeError::Reply(format!(
                        "answer_key_quote: an edit quotes 8 consecutive words of golden {id}"
                    )));
                }
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

/// Candidate surfaces (change 2, q-38521bc5): repo text only.
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
        if !seen.insert(id) {
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

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
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

    #[test]
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
        let none = vec![row("a1", "base"), row("c1", "cand")];
        let err = assert_ledger(&none, "base", &cands, &planned).unwrap_err();
        assert!(format!("{err}").contains("no attempt id"), "{err}");
    }

    #[test]
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
    const CATALOG: &str =
        "cost_order: [trust]\ntypes:\n  - {id: t1, cost: trust, description: type one}\n";

    /// The split is preregistered (law d-d2bc0f62): these two roles were
    /// measured 2026-11-20 at SEED 20_261_102 and must never move.
    #[test]
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
        for (name, body) in [
            ("golden-answer-the-question.yaml", BANK_A),
            ("golden-agent-decide-no-operator-authority.yaml", BANK_B),
        ] {
            std::fs::write(bank.join(name), body).unwrap();
            let id = name.trim_end_matches(".yaml");
            std::fs::write(metrics.join(format!("{id}.yaml")), FIXTURE_OK).unwrap();
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
    #[test]
    fn proposer_input_carries_train_only_and_strips_answer_keys() {
        let f = write_bank(false);
        let bank_dir = f.dir.path().join("evals").join("bank");
        let list = clusters(&f.rows, &f.tasks, &bank_dir, 3).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        let c = &list[0];
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
    #[test]
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
    #[test]
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

    #[test]
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
    #[test]
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

    #[test]
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

    #[test]
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
}
