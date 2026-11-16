//! The cite check: a customers-scope PR whose body or commits cite a D
//! number whose stored scope is internal gets one review-lane finding. CI
//! cannot read the private store, so the check runs where the store lives.
//! `fno do pr cite-check --node <id> --body-file <path>`.

use serde_json::Value;

/// The D numbers a body cites: three to six digits after a capital D.
pub(crate) fn cited_d_numbers(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in body.split(|c: char| c.is_whitespace() || "()[]{}:,;".contains(c)) {
        let t = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let Some(rest) = t.strip_prefix('D') else {
            continue;
        };
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits >= 3 && digits == rest.len() {
            out.push(t.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One finding per internal-scope D number the PR cites. `node_scope` is the
/// citing node's stored scope; a check on an internal node reports nothing.
/// The cited label resolves through the decision numbering table, and the
/// ruling's scope reads from the decision store (the node reader never sees
/// a decision row).
pub(crate) fn findings_for_body(body: &str, node_scope: &str) -> Vec<String> {
    if !node_scope.contains("customer") {
        return Vec::new();
    }
    let numbers = crate::numbers::labels();
    let graph = crate::backlog::settings::graph_path();
    let (rows, _) = crate::decision_index::read_store_rows(
        &graph,
        &crate::decision_index::graph_journal(&graph),
    )
    .unwrap_or_default();
    let internal = |decision_id: &str| -> bool {
        rows.iter()
            .find(|row| {
                crate::backlog::question_intake::str_field(row, "decision_id") == Some(decision_id)
                    || crate::backlog::question_intake::str_field(row, "decision_id").is_some_and(|id| id.ends_with(decision_id))
            })
            .and_then(|row| crate::backlog::question_intake::str_field(row, "scope"))
            .map(|s| s == "internal")
            .unwrap_or(false)
    };
    cited_d_numbers(body)
        .into_iter()
        .filter(|cite| {
            numbers
                .iter()
                .any(|(id, label)| label.eq_ignore_ascii_case(cite) && internal(id))
        })
        .collect()
}

/// The review-lane entry: the door passes --node and --body-file.
pub fn run(args: &[String]) -> i32 {
    let mut node_id = String::new();
    let mut body_file = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--node" if i + 1 < args.len() => {
                node_id = args[i + 1].clone();
                i += 1;
            }
            "--body-file" if i + 1 < args.len() => {
                body_file = args[i + 1].clone();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if body_file.is_empty() {
        eprintln!("cite-check: usage: fno do pr cite-check --node <id> --body-file <path>");
        return 2;
    }
    let Ok(body) = std::fs::read_to_string(&body_file) else {
        eprintln!("cite-check: cannot read {body_file}");
        return 1;
    };
    let node_scope = {
        let graph = crate::backlog::settings::graph_path();
        crate::graph_store::read_one(&graph, &node_id)
            .ok()
            .flatten()
            .and_then(|row| crate::backlog::question_intake::str_field(&row, "scope").map(String::from))
            .unwrap_or_default()
    };
    let findings = findings_for_body(&body, &node_scope);
    if findings.is_empty() {
        println!("cite-check: clean");
        0
    } else {
        for f in &findings {
            eprintln!("cite-check: finding: {f} is internal-scope; a customers PR cites no internal ruling as product truth");
        }
        1
    }
}
