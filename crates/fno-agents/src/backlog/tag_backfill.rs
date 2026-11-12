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
    e.get("archived_at").is_none_or(Value::is_null)
        && text_at(e, "status").is_some_and(|s| OPEN.contains(&s))
}

/// The guesses for one row, split into the tags to write and the review list.
fn split(e: &Value) -> (Vec<String>, Vec<Guess>) {
    let (high, low): (Vec<Guess>, Vec<Guess>) = classify(e)
        .into_iter()
        .partition(|g| g.confidence == Confidence::High);
    (high.into_iter().map(|g| g.tag).collect(), low)
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
    let mut stamp: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut review: Vec<(String, String, Guess)> = Vec::new();
    let mut untagged: Vec<(String, String, &str)> = Vec::new();
    for e in rows.iter().filter(|e| is_open(e)) {
        let id = text_at(e, "id").unwrap_or("?").to_string();
        let title = text_at(e, "title").unwrap_or("").to_string();
        let (high, low) = split(e);
        for facet in REQUIRED_FACETS {
            let carried = super::tags::has_facet(e, facet);
            let guessed = high
                .iter()
                .chain(low.iter().map(|g| &g.tag))
                .any(|t| t.split_once(':').is_some_and(|(f, _)| f == facet));
            if !carried && !guessed {
                untagged.push((id.clone(), title.clone(), facet));
            }
        }
        if !high.is_empty() {
            stamp.insert(id.clone(), high);
        }
        review.extend(low.into_iter().map(|g| (id.clone(), title.clone(), g)));
    }

    let mut written = 0usize;
    if apply && !stamp.is_empty() {
        let applied = graph_store::mutate_rows(
            &graph,
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
                    let mut tags: Vec<Value> = match obj.get("tags") {
                        Some(Value::Array(a)) => a.clone(),
                        _ => Vec::new(),
                    };
                    for t in add {
                        // The row may have gained the facet since the read.
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
        if let Err(err) = applied {
            eprintln!("Error: {err}");
            return 1;
        }
    }

    let stamped_count: usize = stamp.values().map(Vec::len).sum();
    if json_out {
        let review_json: Vec<Value> = review
            .iter()
            .map(|(id, title, g)| json!({"id": id, "title": title, "guess": g.tag, "why": g.why}))
            .collect();
        let untagged_json: Vec<Value> = untagged
            .iter()
            .map(|(id, title, facet)| json!({"id": id, "title": title, "facet": facet}))
            .collect();
        let report = json!({
            "dry_run": !apply,
            "high_confidence_tags": stamped_count,
            "written": written,
            "nodes": stamp.len(),
            "review": review_json,
            "no_guess": untagged_json,
        });
        println!(
            "{}",
            serde_json::to_string(&report).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    println!(
        "review list ({} low-confidence guesses; tag these by hand):",
        review.len()
    );
    for (id, title, g) in &review {
        println!("  {id}\tguess {}\t{}\t{title}", g.tag, g.why);
    }
    println!(
        "no guess ({} facets the classifier could not place):",
        untagged.len()
    );
    for (id, title, facet) in &untagged {
        println!("  {id}\t{facet}\t{title}");
    }
    if apply {
        println!("wrote {written} tags on {} nodes", stamp.len());
    } else {
        println!(
            "dry run: {stamped_count} high-confidence tags on {} nodes; pass --apply to write",
            stamp.len()
        );
    }
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
