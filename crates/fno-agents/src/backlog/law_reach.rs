//! A new law reaches the work it changes.
//!
//! Two reads around the law door's write. Before it: the live rulings that
//! say nearly the same thing as the new law. The door refuses until each one
//! is named by `--supersedes` (it dies) or `--keep` (it stands beside the new
//! law). After it: the open nodes whose text the law touches. Each owner gets
//! one mail, and the recorder reads one line per node.
//!
//! Both reads compare words, never meaning. A contradicting ruling filed under
//! a node subject (a lead ruling sat under a node id) still shares the law's words, so
//! the near read takes subject and decision text together. A node that names
//! the same thing in other words (a desktop guide for an `fno-app` version
//! law) needs `--sweep <word>` from the recorder.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::decision_index;

/// Words that carry no topic: grammar, and the role words every ruling uses.
const FILLER: &[&str] = &[
    "be", "by", "with", "at", "as", "its", "this", "that", "from", "when", "every", "one", "any",
    "never", "only", "must", "may", "are", "was", "were", "has", "have", "each", "so", "if",
    "than", "then", "lead", "review", "agent", "user", "fno", "session", "sessions", "node",
    "nodes",
];

/// A near ruling shares at least this many topic words with the new law.
const NEAR_MIN_SHARED: usize = 6;
/// ... and the shared words are at least half of the shorter word set.
/// Measured on 377 live laws (2026-11-13): 42 sit near another at this bar.
/// D3322 and D3310 share 7 words at 0.78.
const NEAR_MIN_RATIO: f64 = 0.5;

/// An open node joins the sweep when its words carry this share of the law's
/// word weight (inverse document frequency over the open nodes).
const SWEEP_MIN_SCORE: f64 = 0.3;
const SWEEP_MAX_SCORED: usize = 5;
const SWEEP_MAX_NAMED: usize = 10;

/// The topic words of one text.
pub(crate) fn topic_words(text: &str) -> BTreeSet<String> {
    let mut words = crate::rules::tokens(text);
    words.retain(|w| w.len() >= 3 && !FILLER.contains(&w.as_str()));
    words
}

fn law_text(row: &Value) -> String {
    format!(
        "{} {}",
        row.get("subject").and_then(Value::as_str).unwrap_or(""),
        row.get("decision").and_then(Value::as_str).unwrap_or("")
    )
}

/// One live ruling near the new law.
#[derive(Debug, PartialEq)]
pub(crate) struct NearRuling {
    pub(crate) decision_id: String,
    pub(crate) subject: String,
    pub(crate) decision: String,
}

/// The live rulings near a new law in the same law-world: a law, or a lead's
/// role ruling (D3310 was one). `scope` is the new law's stamp; a `global`
/// row on either side reaches every scope.
pub(crate) fn near_rulings(
    index: &decision_index::Index,
    subject: &str,
    decision: &str,
    scope: &str,
) -> Vec<NearRuling> {
    let new_words = topic_words(&format!("{subject} {decision}"));
    let mut near = Vec::new();
    for row in &index.rows {
        let authority = row.get("authority_source").and_then(Value::as_str);
        if !decision_index::is_law(row) && authority != Some("role") {
            continue;
        }
        let row_scope = decision_index::row_scope(row);
        if row_scope != scope && row_scope != "global" && scope != "global" {
            continue;
        }
        let words = topic_words(&law_text(row));
        let shared = words.intersection(&new_words).count();
        let shorter = words.len().min(new_words.len()).max(1);
        if shared < NEAR_MIN_SHARED || (shared as f64) / (shorter as f64) < NEAR_MIN_RATIO {
            continue;
        }
        let Some(id) = row.get("decision_id").and_then(Value::as_str) else {
            continue;
        };
        near.push(NearRuling {
            decision_id: id.to_string(),
            subject: row
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            decision: row
                .get("decision")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    near
}

/// The refusal for near rulings the recorder did not name. `None` when every
/// near ruling is the supersession target or a kept id.
pub(crate) fn unnamed_near_refusal(
    near: &[NearRuling],
    supersedes: Option<&str>,
    keep: &[String],
) -> Option<String> {
    let named = |id: &str| {
        supersedes.is_some_and(|s| s.eq_ignore_ascii_case(id))
            || keep.iter().any(|k| k.eq_ignore_ascii_case(id))
    };
    let open: Vec<&NearRuling> = near.iter().filter(|n| !named(&n.decision_id)).collect();
    if open.is_empty() {
        return None;
    }
    let mut message = format!(
        "{} live ruling(s) say nearly the same thing as this law. Name each one: \
         --supersedes <id> retires it, --keep <id> keeps it beside the new law",
        open.len()
    );
    for n in open {
        message.push_str(&format!(
            "\n  {} ({}): {}",
            n.decision_id,
            n.subject,
            crate::law_match::one_line(&n.decision, 120)
        ));
    }
    Some(message)
}

/// One open node the new law touches.
#[derive(Debug, PartialEq)]
pub(crate) struct Touched {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) owner: Option<String>,
    pub(crate) pr: Option<u64>,
    pub(crate) plan: Option<String>,
}

fn is_open(row: &Value) -> bool {
    !matches!(
        row.get("status").and_then(Value::as_str).unwrap_or(""),
        "done" | "superseded" | "deferred" | "archived" | "cancelled"
    )
}

/// The session id that owns a node, from its lock. A lock that names no
/// session (a handover placeholder) has no owner to mail.
fn owner_session(row: &Value) -> Option<String> {
    let lock = row.get("locked_by").and_then(Value::as_str)?;
    let id = lock.rsplit(':').next().unwrap_or(lock).trim();
    let uuid = id.len() == 36
        && id.chars().filter(|c| *c == '-').count() == 4
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    uuid.then(|| id.to_string())
}

fn node_text(row: &Value) -> String {
    format!(
        "{} {}",
        row.get("title").and_then(Value::as_str).unwrap_or(""),
        row.get("details").and_then(Value::as_str).unwrap_or("")
    )
}

/// The open nodes a law touches: the best word matches over the open board
/// (rare words weigh more), then every node that carries a `--sweep` word.
pub(crate) fn touched_nodes(rows: &[Value], law: &str, sweep: &[String]) -> Vec<Touched> {
    let open: Vec<&Value> = rows.iter().filter(|r| is_open(r)).collect();
    let docs: Vec<BTreeSet<String>> = open.iter().map(|r| topic_words(&node_text(r))).collect();
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for words in &docs {
        for w in words {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let n = docs.len() as f64;
    let idf = |w: &str| {
        let d = *df.get(w).unwrap_or(&0) as f64;
        (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
    };
    let query = topic_words(law);
    let total: f64 = query.iter().map(|w| idf(w)).sum();
    let mut scored: Vec<(f64, usize)> = Vec::new();
    if total > 0.0 {
        for (i, words) in docs.iter().enumerate() {
            let score = query.intersection(words).map(|w| idf(w)).sum::<f64>() / total;
            if score >= SWEEP_MIN_SCORE {
                scored.push((score, i));
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<usize> = scored
        .into_iter()
        .take(SWEEP_MAX_SCORED)
        .map(|(_, i)| i)
        .collect();
    let lowered: Vec<String> = sweep
        .iter()
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let mut named = 0;
    for (i, row) in open.iter().enumerate() {
        if named == SWEEP_MAX_NAMED || picked.contains(&i) {
            continue;
        }
        let text = node_text(row).to_lowercase();
        if lowered.iter().any(|w| text.contains(w.as_str())) {
            picked.push(i);
            named += 1;
        }
    }
    picked
        .into_iter()
        .map(|i| {
            let row = open[i];
            Touched {
                id: row
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                owner: owner_session(row),
                pr: row.get("pr_number").and_then(Value::as_u64),
                plan: row
                    .get("plan_path")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn work_of(t: &Touched) -> String {
    let mut parts = vec![format!("node {} ({})", t.id, t.title)];
    if let Some(pr) = t.pr {
        parts.push(format!("PR {pr}"));
    }
    if let Some(plan) = &t.plan {
        parts.push(format!("plan {plan}"));
    }
    parts.join(", ")
}

/// The sweep after a law lands: one mail per owned node, one stderr line per
/// node. Best effort: an unreadable board is one line, never a failed write.
pub(crate) fn sweep_after_law(decision_id: &str, subject: &str, decision: &str, sweep: &[String]) {
    let rows = match crate::backlog::decisions_cli::read_graph_entries() {
        Ok(rows) => rows,
        Err(reason) => {
            eprintln!("law: open-work sweep skipped ({reason})");
            return;
        }
    };
    let law = format!("{subject} {decision}");
    let label = crate::numbers::display(decision_id);
    for t in touched_nodes(&rows, &law, sweep) {
        let work = work_of(&t);
        let Some(owner) = &t.owner else {
            eprintln!("law: {label} may change {work}; no owner session to mail.");
            continue;
        };
        let text = format!(
            "Law {label} ({subject}) landed: {}. It may change your {work}. \
             Read it: fno inbox decisions {decision_id}. Align the work, or reply why it stands.",
            crate::law_match::one_line(decision, 200)
        );
        // A test process never mails the live fleet.
        if cfg!(test) {
            continue;
        }
        let runner: crate::burn_watch::Runner = &mut crate::burn_watch::run_command;
        let sender = crate::system_sender::system_name("law-sweep");
        match crate::first_check::send_mail(runner, &sender, owner, &text) {
            Ok(()) => eprintln!("law: {label} mailed the owner of {work}."),
            Err(e) => eprintln!("law: {label} may change {work}; the mail failed ({e})."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn index(rows: Vec<Value>) -> decision_index::Index {
        decision_index::Index { rows, damaged: 0 }
    }

    #[test]
    fn a_contradicting_ruling_under_a_node_subject_is_near_and_must_be_named() {
        // The specimen: D3310 sat under a node subject with role authority.
        let old = json!({
            "decision_id": "d-afb64731", "subject": "x-dddd", "authority_source": "role",
            "ts": "2026-11-12T08:30:14Z",
            "decision": "fno-app versions in lockstep with fno; wire compatibility is read from the server's .ver sidecar",
        });
        let unrelated = json!({
            "decision_id": "d-11111111", "subject": "merge-authority", "authority_source": "operator",
            "ts": "2026-11-12T08:30:14Z", "decision": "Merges belong to the operator",
        });
        let other_project = json!({
            "decision_id": "d-22222222", "subject": "x-dddd", "authority_source": "role",
            "scope": "project:fno-web", "ts": "2026-11-12T08:30:14Z",
            "decision": "fno-app versions in lockstep with fno; wire compatibility is read from the server's .ver sidecar",
        });
        let idx = index(vec![old, unrelated, other_project]);
        let near = near_rulings(
            &idx,
            "release:fno-app-version",
            "fno-app has its own version line, starting at 0.1.0 at launch. It no longer \
             moves in lockstep with fno. Wire compatibility stays on the server .ver sidecar.",
            "project:fno",
        );
        let ids: Vec<&str> = near.iter().map(|n| n.decision_id.as_str()).collect();
        assert_eq!(ids, vec!["d-afb64731"]);
        let refusal = unnamed_near_refusal(&near, None, &[]).expect("refuses");
        assert!(
            refusal.contains("d-afb64731") && refusal.contains("--keep"),
            "{refusal}"
        );
        assert!(unnamed_near_refusal(&near, Some("D-AFB64731"), &[]).is_none());
        assert!(unnamed_near_refusal(&near, None, &["d-afb64731".into()]).is_none());
    }

    #[test]
    fn the_sweep_picks_open_word_matches_and_named_words_and_mails_only_sessions() {
        let node = |id: &str, status: &str, title: &str, lock: Option<&str>| json!({"id": id, "status": status, "title": title, "details": "", "locked_by": lock});
        let mut rows = vec![
            node(
                "x-aaaa",
                "in_progress",
                "Each crate carries its own version line; no lockstep",
                Some("target-session:36675be7-6519-45df-8c7e-943731994329"),
            ),
            node(
                "x-bbbb",
                "done",
                "Each crate carries its own version line; no lockstep",
                None,
            ),
            node(
                "x-cccc",
                "in_review",
                "docs: the desktop guide",
                Some("spawn-handover:t-x"),
            ),
        ];
        for i in 0..20 {
            rows.push(node(
                &format!("x-f{i:03}"),
                "ready",
                "paint the mux status bar",
                None,
            ));
        }
        let law = "release:fno-app-version fno-app has its own version line, no lockstep";
        let touched = touched_nodes(&rows, law, &[]);
        let ids: Vec<&str> = touched.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["x-aaaa"], "a done node is not open work");
        assert_eq!(
            touched[0].owner.as_deref(),
            Some("36675be7-6519-45df-8c7e-943731994329")
        );
        let named = touched_nodes(&rows, law, &["Desktop".to_string()]);
        let cccc = named
            .iter()
            .find(|t| t.id == "x-cccc")
            .expect("a named word sweeps");
        assert_eq!(cccc.owner, None, "a handover placeholder is no session");
    }
}
