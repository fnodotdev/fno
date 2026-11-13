//! `fno backlog admin backfill-tags`: stamp the required facets (audience,
//! area) on open nodes from the vocabulary classifier. Only a high-confidence
//! guess is written; every low-confidence guess prints on the review list,
//! so a guess never lands silently. A tag a node already carries is never
//! replaced.

use serde_json::{json, Value};
use std::collections::BTreeMap;

use super::settings;
use super::tags::{classify, Confidence, Guess, REQUIRED_FACETS};
use super::workflows::{refuse_non_graph_backend, text_at};
use crate::graph_store;

const HELP: &str = "Usage: fno backlog admin backfill-tags [--apply] [--json]

Guess the audience and area tags open nodes lack, from the keywords in
crates/fno-agents/src/backlog/tag_vocabulary.toml. Only a high-confidence
guess is written. Each low-confidence guess prints on the review list:
tag those nodes by hand with `fno backlog update <id> tags+=<facet:value>`.
An existing tag is never replaced. Re-run to read the list again.

Options:
  --apply     Write the high-confidence tags. Default is a dry-run report.
  -J, --json  Emit a JSON report
  -h, --help  Print help
";

const OPEN: [&str; 6] = [
    "idea",
    "design",
    "ready",
    "in_progress",
    "in_review",
    "blocked",
];

fn is_open(e: &Value) -> bool {
    text_at(e, "archived_at").is_none() && text_at(e, "status").is_some_and(|s| OPEN.contains(&s))
}

/// The guesses for one row, split into the tags to write and the review list.
fn split(e: &Value) -> (Vec<String>, Vec<Guess>) {
    let (high, low): (Vec<Guess>, Vec<Guess>) = classify(e)
        .into_iter()
        .partition(|g| g.confidence == Confidence::High);
    (high.into_iter().map(|g| g.tag).collect(), low)
}

/// What one read of the graph proposes: the sure tags to write per node,
/// the unsure guesses for a human, and the facets with no guess at all.
#[derive(Default)]
struct Proposal {
    stamp: BTreeMap<String, Vec<String>>,
    review: Vec<(String, String, Guess)>,
    untagged: Vec<(String, String, &'static str)>,
}

fn propose(rows: &[Value]) -> Proposal {
    let mut p = Proposal::default();
    for e in rows.iter().filter(|e| is_open(e)) {
        let id = text_at(e, "id").unwrap_or("?").to_string();
        let title = text_at(e, "title").unwrap_or("").to_string();
        let (high, low) = split(e);
        for facet in REQUIRED_FACETS {
            let guessed = high
                .iter()
                .chain(low.iter().map(|g| &g.tag))
                .any(|t| t.split_once(':').is_some_and(|(f, _)| f == facet));
            if !super::tags::has_facet(e, facet) && !guessed {
                p.untagged.push((id.clone(), title.clone(), facet));
            }
        }
        if !high.is_empty() {
            p.stamp.insert(id.clone(), high);
        }
        p.review
            .extend(low.into_iter().map(|g| (id.clone(), title.clone(), g)));
    }
    p
}

/// Write the sure tags under the graph lock; a facet the row gained since
/// the read is kept. Returns the count written, or the exit code.
fn write(graph: &std::path::Path, stamp: &BTreeMap<String, Vec<String>>) -> Result<usize, i32> {
    let mut written = 0usize;
    let applied = graph_store::mutate_rows(
        graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for node in working.iter_mut() {
                let Some(add) = text_at(node, "id").and_then(|id| stamp.get(id)) else {
                    continue;
                };
                let Some(obj) = node.as_object_mut() else {
                    continue;
                };
                let mut tags: Vec<Value> = match obj.remove("tags") {
                    Some(Value::Array(a)) => a,
                    _ => Vec::new(),
                };
                for t in add {
                    let facet = t.split_once(':').map_or("", |(f, _)| f);
                    let has = tags
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|have| have.split_once(':').is_some_and(|(f, _)| f == facet));
                    if !has {
                        tags.push(json!(t));
                        written += 1;
                    }
                }
                obj.insert("tags".into(), Value::Array(tags));
            }
            Ok(true)
        },
    );
    match applied {
        Ok(_) => Ok(written),
        Err(err) => {
            eprintln!("Error: {err}");
            Err(1)
        }
    }
}

fn report(p: &Proposal, apply: bool, written: usize, json_out: bool) {
    let stamped: usize = p.stamp.values().map(Vec::len).sum();
    if json_out {
        let review: Vec<Value> = p
            .review
            .iter()
            .map(|(id, title, g)| json!({"id": id, "title": title, "guess": g.tag, "why": g.why}))
            .collect();
        let no_guess: Vec<Value> = p
            .untagged
            .iter()
            .map(|(id, title, facet)| json!({"id": id, "title": title, "facet": facet}))
            .collect();
        let out = json!({
            "dry_run": !apply,
            "high_confidence_tags": stamped,
            "written": written,
            "nodes": p.stamp.len(),
            "review": review,
            "no_guess": no_guess,
        });
        println!("{out}");
        return;
    }
    println!(
        "review list ({} low-confidence guesses; tag these by hand):",
        p.review.len()
    );
    for (id, title, g) in &p.review {
        println!("  {id}\tguess {}\t{}\t{title}", g.tag, g.why);
    }
    println!(
        "no guess ({} facets the classifier could not place):",
        p.untagged.len()
    );
    for (id, title, facet) in &p.untagged {
        println!("  {id}\t{facet}\t{title}");
    }
    if apply {
        println!("wrote {written} tags on {} nodes", p.stamp.len());
    } else {
        println!(
            "dry run: {stamped} high-confidence tags on {} nodes; pass --apply to write",
            p.stamp.len()
        );
    }
}

pub fn run(tail: &[String]) -> i32 {
    let mut apply = false;
    let mut json_out = false;
    for arg in tail {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return 0;
            }
            "--apply" => apply = true,
            "-J" | "--json" => json_out = true,
            other => {
                eprintln!("fno backlog admin backfill-tags: unexpected argument {other:?}");
                return 2;
            }
        }
    }
    if let Some(code) = refuse_non_graph_backend("backfill-tags") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let proposal = propose(&rows);
    let written = if apply && !proposal.stamp.is_empty() {
        match write(&graph, &proposal.stamp) {
            Ok(n) => n,
            Err(code) => return code,
        }
    } else {
        0
    };
    report(&proposal, apply, written, json_out);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_open_rows_get_guesses_and_low_ones_stay_off_the_write_list() {
        let done = json!({"id": "x-1", "status": "done", "title": "mux pane keeper"});
        assert!(!is_open(&done));
        let open = json!({"id": "x-2", "status": "idea", "title": "mux pane keeper portal"});
        assert!(is_open(&open));
        let (high, low) = split(&open);
        assert_eq!(
            high,
            vec!["area:mux".to_string(), "audience:user".to_string()]
        );
        assert!(low.is_empty());
        let vague = json!({"id": "x-3", "status": "idea", "title": "the review merge"});
        let (high, low) = split(&vague);
        assert!(high.is_empty() && !low.is_empty());
    }
}
