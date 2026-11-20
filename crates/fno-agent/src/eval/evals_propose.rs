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

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::evals::bank::TaskSpec;
use crate::evals::failure_types::{prove, Catalog};
use crate::evals_power::SEED;
use crate::evals_trend::Row;

/// Clusters one cycle drafts. One change per cluster keeps the per-change
/// verdict honest: two merged changes in one probation window read
/// `confounded` (second pass, change 5).
pub(crate) const MAX_CLUSTERS: usize = 2;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::bank::load_task;

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
}
