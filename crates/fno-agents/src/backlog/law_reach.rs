//! A new law reaches the work it changes.
//!
//! Before the law door writes, it reads the live rulings that say nearly the
//! same thing, and refuses until each one is named by `--supersedes` (it dies)
//! or `--keep` (it stands). After the write, it reads the open nodes the law
//! touches and mails each owner session.
//!
//! Both reads compare words, never meaning. A ruling filed under a node id
//! still shares the law's words, so the near read takes subject and decision
//! text together. A node that names the same thing in other words needs
//! `--sweep <word>` from the recorder.

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

/// A near ruling shares at least this many topic words with the new law, and
/// they are at least half of the shorter word set. Measured on 377 live laws
/// (2026-11-13): 42 sit near another at this bar. The lockstep specimen
/// shares 7 words at 0.78.
const NEAR_MIN_SHARED: usize = 6;
const NEAR_MIN_RATIO: f64 = 0.5;

/// An open node joins the sweep when its words carry this share of the law's
/// word weight (inverse document frequency over the open nodes).
const SWEEP_MIN_SCORE: f64 = 0.3;
const SWEEP_MAX_SCORED: usize = 5;
const SWEEP_MAX_NAMED: usize = 10;

fn s<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn topic_words(text: &str) -> BTreeSet<String> {
    let mut words = crate::rules::tokens(text);
    words.retain(|w| w.len() >= 3 && !FILLER.contains(&w.as_str()));
    words
}

/// The live laws and lead role rulings near a new law in the same scope; a
/// `global` row on either side reaches every scope.
pub(crate) fn near_rulings<'a>(
    index: &'a decision_index::Index,
    subject: &str,
    decision: &str,
    scope: &str,
) -> Vec<&'a Value> {
    let new_words = topic_words(&format!("{subject} {decision}"));
    let in_scope = |r: &str| r == scope || r == "global" || scope == "global";
    let near = |row: &Value| {
        let words = topic_words(&format!("{} {}", s(row, "subject"), s(row, "decision")));
        let shared = words.intersection(&new_words).count();
        let shorter = words.len().min(new_words.len()).max(1);
        shared >= NEAR_MIN_SHARED && shared as f64 / shorter as f64 >= NEAR_MIN_RATIO
    };
    index
        .rows
        .iter()
        .filter(|r| decision_index::is_law(r) || s(r, "authority_source") == "role")
        .filter(|r| !s(r, "decision_id").is_empty())
        .filter(|r| in_scope(decision_index::row_scope(r)) && near(r))
        .collect()
}

/// The refusal for near rulings the recorder did not name.
pub(crate) fn unnamed_near_refusal(
    near: &[&Value],
    supersedes: Option<&str>,
    keep: &[String],
) -> Option<String> {
    let named = |id: &str| {
        supersedes.is_some_and(|s| s.eq_ignore_ascii_case(id))
            || keep.iter().any(|k| k.eq_ignore_ascii_case(id))
    };
    let open: Vec<_> = near
        .iter()
        .filter(|r| !named(s(r, "decision_id")))
        .collect();
    if open.is_empty() {
        return None;
    }
    let mut message = format!(
        "{} live ruling(s) say nearly the same thing as this law. Name each one: \
         --supersedes <id> retires it, --keep <id> keeps it beside the new law",
        open.len()
    );
    for r in open {
        let text = crate::law_match::one_line(s(r, "decision"), 120);
        let (id, subject) = (s(r, "decision_id"), s(r, "subject"));
        message.push_str(&format!("\n  {id} ({subject}): {text}"));
    }
    Some(message)
}

/// The session id that owns a node, from its lock. A lock that names no
/// session (a handover placeholder) has no owner to mail.
fn owner_session(row: &Value) -> Option<&str> {
    let lock = s(row, "locked_by");
    let id = lock.rsplit(':').next().unwrap_or(lock).trim();
    let uuid = id.len() == 36
        && id.chars().filter(|c| *c == '-').count() == 4
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    uuid.then_some(id)
}

fn node_text(row: &Value) -> String {
    format!("{} {}", s(row, "title"), s(row, "details"))
}

/// The open nodes a law touches: the best word matches over the open board
/// (rare words weigh more), then every node that carries a `--sweep` word.
fn touched_nodes<'a>(rows: &'a [Value], law: &str, sweep: &[String]) -> Vec<&'a Value> {
    let closed = ["done", "superseded", "deferred", "archived", "cancelled"];
    let open: Vec<&Value> = rows
        .iter()
        .filter(|r| !closed.contains(&s(r, "status")))
        .collect();
    let docs: Vec<BTreeSet<String>> = open.iter().map(|r| topic_words(&node_text(r))).collect();
    let mut df: BTreeMap<&str, f64> = BTreeMap::new();
    for w in docs.iter().flatten() {
        *df.entry(w.as_str()).or_default() += 1.0;
    }
    let n = docs.len() as f64;
    let idf = |w: &str| {
        let d = df.get(w).copied().unwrap_or(0.0);
        (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
    };
    let query = topic_words(law);
    let total: f64 = query.iter().map(|w| idf(w)).sum();
    let mut scored: Vec<(f64, usize)> = docs
        .iter()
        .enumerate()
        .map(|(i, words)| {
            (
                query.intersection(words).map(|w| idf(w)).sum::<f64>() / total,
                i,
            )
        })
        .filter(|(score, _)| *score >= SWEEP_MIN_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<usize> = scored.iter().take(SWEEP_MAX_SCORED).map(|p| p.1).collect();
    let words: Vec<String> = sweep
        .iter()
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let named: Vec<usize> = (0..open.len())
        .filter(|i| !picked.contains(i))
        .filter(|&i| {
            let text = node_text(open[i]).to_lowercase();
            words.iter().any(|w| text.contains(w.as_str()))
        })
        .take(SWEEP_MAX_NAMED)
        .collect();
    picked.extend(named);
    picked.into_iter().map(|i| open[i]).collect()
}

/// The sweep after a law lands: one mail per owned node, one stderr line per
/// node. Best effort: an unreadable board is one line, never a failed write.
pub(crate) fn sweep_after_law(decision_id: &str, subject: &str, decision: &str, sweep: &[String]) {
    let rows = match crate::backlog::decisions_cli::read_graph_entries() {
        Ok(rows) => rows,
        Err(reason) => return eprintln!("law: open-work sweep skipped ({reason})"),
    };
    let label = crate::numbers::display(decision_id);
    for row in touched_nodes(&rows, &format!("{subject} {decision}"), sweep) {
        let mut work = format!("node {} ({})", s(row, "id"), s(row, "title"));
        if let Some(pr) = row.get("pr_number").and_then(Value::as_u64) {
            work.push_str(&format!(", PR {pr}"));
        }
        if !s(row, "plan_path").is_empty() {
            work.push_str(&format!(", plan {}", s(row, "plan_path")));
        }
        // A test process never mails the live fleet.
        let Some(owner) = owner_session(row).filter(|_| !cfg!(test)) else {
            eprintln!("law: {label} touches {work}. No owner session to mail.");
            continue;
        };
        let text = format!(
            "Law {label} ({subject}) landed: {}. It can change your {work}. \
             Read it: fno inbox decisions {decision_id}. Align the work, or reply why it stands.",
            crate::law_match::one_line(decision, 200)
        );
        let runner: crate::burn_watch::Runner = &mut crate::burn_watch::run_command;
        let sender = crate::system_sender::system_name("law-sweep");
        match crate::first_check::send_mail(runner, &sender, owner, &text) {
            Ok(()) => eprintln!("law: {label} mailed the owner of {work}."),
            Err(e) => eprintln!("law: {label} touches {work}. The mail failed ({e})."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_contradicting_ruling_under_a_node_subject_is_near_and_must_be_named() {
        let lockstep = "fno-app versions in lockstep with fno; wire compatibility is read from the server's .ver sidecar";
        let row = |id: &str, subject: &str, authority: &str, scope: Option<&str>, text: &str| {
            json!({"decision_id": id, "subject": subject, "authority_source": authority,
                   "scope": scope, "ts": "2026-11-12T08:30:14Z", "decision": text})
        };
        let idx = decision_index::Index {
            rows: vec![
                row("d-afb64731", "x-dddd", "role", None, lockstep),
                row(
                    "d-11111111",
                    "merge-authority",
                    "operator",
                    None,
                    "Merges belong to the operator",
                ),
                row(
                    "d-22222222",
                    "x-dddd",
                    "role",
                    Some("project:fno-web"),
                    lockstep,
                ),
            ],
            damaged: 0,
        };
        let law = "fno-app has its own version line, starting at 0.1.0 at launch. It no longer \
             moves in lockstep with fno. Wire compatibility stays on the server .ver sidecar.";
        let near = near_rulings(&idx, "release:fno-app-version", law, "project:fno");
        let ids: Vec<&str> = near.iter().map(|r| s(r, "decision_id")).collect();
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
        let node = |id: &str, status: &str, title: &str, lock: Option<&str>| json!({"id": id, "status": status, "title": title, "locked_by": lock});
        let crates = "Each crate carries its own version line; no lockstep";
        let session = "target-session:36675be7-6519-45df-8c7e-943731994329";
        let mut rows = vec![
            node("x-aaaa", "in_progress", crates, Some(session)),
            node("x-bbbb", "done", crates, None),
            node(
                "x-cccc",
                "in_review",
                "docs: the desktop guide",
                Some("spawn-handover:t-x"),
            ),
        ];
        rows.extend((0..20).map(|_| node("x-ffff", "ready", "paint the mux status bar", None)));
        let law = "release:fno-app-version fno-app has its own version line, no lockstep";
        let touched = touched_nodes(&rows, law, &[]);
        let ids: Vec<&str> = touched.iter().map(|r| s(r, "id")).collect();
        assert_eq!(ids, vec!["x-aaaa"], "a done node is not open work");
        assert_eq!(
            owner_session(touched[0]),
            Some("36675be7-6519-45df-8c7e-943731994329")
        );
        let named = touched_nodes(&rows, law, &["Desktop".to_string()]);
        let cccc = named
            .iter()
            .find(|r| s(r, "id") == "x-cccc")
            .expect("a named word sweeps");
        assert_eq!(
            owner_session(cccc),
            None,
            "a handover placeholder is no session"
        );
    }
}
