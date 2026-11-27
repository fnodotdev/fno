//! The scope backfill: one idempotent pass over today's open rows. The ruled
//! audience-tag mappings stamp through the locked update door (the ruling is
//! the why, so the history entry records it); the rest propose for the
//! user's review file. A persona rename or retire lists every open row that
//! names it. Design of record: the approved personas design, sections 7-8.

use serde_json::Value;

/// The fields the backfill reads, parsed once per row: the boundary
/// deserializes, and nothing past it reads a string key.
#[derive(serde::Deserialize, Default)]
struct Row {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    done_means: Option<String>,
}

fn parse_row(row: &Value) -> Option<Row> {
    serde_json::from_value(row.clone()).ok()
}

/// The ruled Q1 mapping (design section 8.1): contributor maps to internal.
/// The plan's "except the docs row" exception holds no rule anywhere, so it
/// maps nothing; a docs row waits on the user's ruling like any other.
fn audience_tag_scope(tag: &str) -> Option<&'static str> {
    match tag {
        "audience:internal" => Some("internal"),
        "audience:user" => Some("customers"),
        "audience:contributor" => Some("internal"),
        _ => None,
    }
}

/// `fno backlog audience-backfill [--apply] [--persona-retired <slug>]`. The
/// dry run is the default: it counts and names; --apply stamps.
pub fn run(args: &[String]) -> i32 {
    let mut apply = false;
    let mut retired: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--persona-retired" => match iter.next() {
                Some(v) => retired = Some(v.clone()),
                None => {
                    eprintln!("audience-backfill: --persona-retired needs a value");
                    return 2;
                }
            },
            other => {
                eprintln!("audience-backfill: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let graph = crate::backlog::settings::graph_path();
    let rows = match crate::graph_store::read_rows(&graph) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("audience-backfill: graph unreadable: {e}");
            return 1;
        }
    };
    if let Some(persona) = &retired {
        return retire_report(&rows, persona);
    }
    backfill(&rows, apply)
}

/// The dry run counts; --apply stamps each ruled row through the locked
/// update door (the tag drops in the same write) and writes the rest to the
/// vault review file. The review path resolves beside the plans dir, the
/// decision-log idiom.
fn backfill(rows: &[Value], apply: bool) -> i32 {
    let anchor = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let review = crate::plans_path::plans_content_dir(&anchor)
        .and_then(|plans| plans.parent().map(|p| p.join("scope-review.md")));
    let mut updates: Vec<Vec<String>> = Vec::new();
    let (code, mapped, unstamped) =
        backfill_into(rows, apply, review.as_deref(), &mut |cmd: &[String]| {
            updates.push(cmd.to_vec());
            run_update(cmd)
        });
    if apply {
        println!("audience-backfill: stamped {mapped} row(s) by the ruled mapping");
        if let Some(path) = review.filter(|_| unstamped > 0) {
            println!("audience-backfill: proposals at {}", path.display());
        }
    } else {
        println!("audience-backfill: {mapped} row(s) map by rule; pass --apply to stamp");
    }
    println!("audience-backfill: {unstamped} open row(s) stay unstamped pending the user's ruling");
    code
}

/// One locked write: the scope stamp, the ruling why and the tag drop in one
/// update, so the history entry records the ruling and the tag cannot map
/// twice.
fn run_update(cmd: &[String]) -> std::io::Result<()> {
    let exe = crate::client_exe::current().unwrap_or_else(|_| "fno-agents".into());
    let mut command = std::process::Command::new(exe);
    command.args(cmd);
    command.status().map(|_| ())
}

/// The core pass over the rows. `run` executes one built update (the seam the
/// unit tests collect into); the store hears only ruled rows, the review file
/// hears the rest.
fn backfill_into(
    rows: &[Value],
    apply: bool,
    review: Option<&std::path::Path>,
    run: &mut dyn FnMut(&[String]) -> std::io::Result<()>,
) -> (i32, usize, usize) {
    let mut mapped = 0usize;
    let mut unstamped = 0usize;
    let mut proposals: Vec<String> = Vec::new();
    for row in rows {
        let Some(r) = parse_row(row) else {
            continue;
        };
        let open = r.completed_at.as_deref().unwrap_or("").is_empty();
        if !open {
            continue;
        }
        let already = r
            .audience
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if already.is_some() {
            continue;
        }
        let tags: Vec<&str> = r.tags.iter().flatten().map(String::as_str).collect();
        let ruled = tags
            .iter()
            .find_map(|t| audience_tag_scope(t).map(|scope| (*t, scope)));
        match ruled {
            Some((tag, scope)) => {
                if apply {
                    let cmd = stamp_args(r.id.as_deref().unwrap_or("?"), tag, scope);
                    if let Err(e) = run(&cmd) {
                        eprintln!(
                            "audience-backfill: update refused for {}: {e}",
                            r.id.as_deref().unwrap_or("?")
                        );
                    }
                }
                mapped += 1;
            }
            None => {
                unstamped += 1;
                proposals.push(proposal_block(&r));
            }
        }
    }
    if apply && unstamped > 0 {
        if let Some(path) = review {
            write_review_file(path, &proposals);
        }
    }
    (0, mapped, unstamped)
}

/// The update-door tokens for one ruled row: scope stamp, ruling why, tag
/// drop, in one write.
fn stamp_args(id: &str, tag: &str, scope: &str) -> Vec<String> {
    vec![
        "backlog".into(),
        "update".into(),
        id.into(),
        format!("audience={scope}"),
        format!("why=backfill: ruled audience-tag mapping ({scope})"),
        format!("tags-={tag}"),
    ]
}

/// The proposal one unstamped row contributes to the review file: the three
/// facts the user rules, the row's own values where they exist.
fn proposal_block(r: &Row) -> String {
    let id = r.id.as_deref().unwrap_or("?");
    let title = r.title.as_deref().unwrap_or("(no title)");
    let project = r.project.as_deref().unwrap_or("(no project)");
    let why = r
        .why
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("none on the row; draft one before stamping");
    let done = r
        .done_means
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("none on the row; state how done is verified");
    format!(
        "## {id}: {title}\n- project: {project}\n- audience: needs ruling (internal, customers, or customer:<persona>)\n- why: {why}\n- done-means: {done}\n"
    )
}

/// The vault review file: proposals only, never a store write. Rewritten on
/// every --apply so it stays current; lists the spot-check ask for the user.
fn write_review_file(path: &std::path::Path, proposals: &[String]) {
    let mut text = String::from(
        "# Scope review: open rows pending the user's ruling\n\n\
         The backfill stamps ruled `audience:` tags itself; the rows below\n\
         carry no ruled mapping. Rule each by hand:\n\n\
         - fno backlog update <id> audience=<scope>\n\
         - fno backlog update <id> why=<why>\n\
         - fno backlog update <id> done_means=<done-means>\n\n\
         Spot-check ask: sample 30 of the stamped rows and confirm each\n\
         stamped scope names who the row serves; a miss files a follow-up\n\
         with the corrected scope.\n",
    );
    for block in proposals {
        text.push('\n');
        text.push_str(block);
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "audience-backfill: review dir unwritable at {}: {e}",
                parent.display()
            );
        }
    }
    if let Err(e) = std::fs::write(path, text) {
        eprintln!(
            "audience-backfill: review file unwritable at {}: {e}",
            path.display()
        );
    }
}

/// Every open row naming customer:<persona>, for a rename or retire.
fn retire_report(rows: &[Value], persona: &str) -> i32 {
    let needle = format!("customer:{persona}");
    let mut hits = 0usize;
    for row in rows {
        let Some(r) = parse_row(row) else {
            continue;
        };
        let open = r.completed_at.as_deref().unwrap_or("").is_empty();
        let scope = r.audience.as_deref().unwrap_or("");
        if open && scope.contains(&needle) {
            let id = r.id.as_deref().unwrap_or("?");
            println!("{id} names {needle}");
            hits += 1;
        }
    }
    println!("audience-backfill: {hits} open row(s) name customer:{persona}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("scope-review-{tag}-{}", std::process::id()))
    }

    fn row(id: &str, tags: &[&str], audience: &str, completed_at: &str) -> Value {
        serde_json::json!({
            "id": id,
            "title": "Ship the persona page",
            "project": "fno",
            "tags": tags,
            "audience": audience,
            "why": "the user asked why nodes carry no customers",
            "done_means": "pr create refuses a body without a Serves section",
            "completed_at": completed_at,
        })
    }

    #[test]
    fn apply_stamps_drops_and_leaves_the_rest_to_the_review_file() {
        let dir = fixture("apply");
        let review = dir.join("scope-review.md");
        let rows = vec![
            row("ab-1", &["audience:internal"], "", ""),
            row("ab-5", &["audience:contributor"], "", ""),
            row("ab-2", &["shaped-note"], "", ""),
            row("ab-3", &[], "internal", ""),
            row("ab-4", &["audience:user"], "", "2026-11-19T00:00:00Z"),
        ];
        let mut updates: Vec<Vec<String>> = Vec::new();
        let (code, mapped, unstamped) =
            backfill_into(&rows, true, Some(&review), &mut |cmd: &[String]| {
                updates.push(cmd.to_vec());
                Ok(())
            });
        assert_eq!(code, 0);
        // The count is the defect's visible symptom: --apply counts too.
        assert_eq!(mapped, 2, "--apply counts the ruled rows it stamps");
        assert_eq!(unstamped, 1);
        // One store write per ruled row: the scope stamp, the ruling why
        // and the tag drop in the one update.
        assert_eq!(updates.len(), 2, "the store hears only the ruled rows");
        assert!(updates.iter().any(|c| {
            c.contains(&"audience=internal".to_string())
                && c.contains(&"tags-=audience:internal".to_string())
        }));
        // The ruled Q1 mapping: contributor maps to internal; the docs-row
        // exception holds no rule, so no persona scope ever stamps.
        assert!(updates.iter().any(|c| {
            c.contains(&"tags-=audience:contributor".to_string())
                && c.contains(&"audience=internal".to_string())
        }));
        assert!(
            updates
                .iter()
                .all(|c| !c.contains(&"customer:contributor".to_string())),
            "{updates:?}"
        );
        assert!(updates
            .iter()
            .all(|c| c.iter().any(|a| a.starts_with("why=backfill: ruled"))));
        // The unmapped row's id reaches the review file, never the store.
        assert!(updates.iter().all(|c| !c.contains(&"ab-2".to_string())));
        let text = std::fs::read_to_string(&review).unwrap();
        assert!(text.contains("## ab-2"), "{text:?}");
        assert!(text.contains("audience"), "{text:?}");
        assert!(text.contains("why:"), "{text:?}");
        assert!(text.contains("done-means"), "{text:?}");
        assert!(text.contains("Spot-check ask: sample 30"), "{text:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_review_file_names_audience_why_and_done_means() {
        let dir = fixture("review");
        let review = dir.join("nested").join("scope-review.md");
        // An unmapped row carries all three facts for the user to rule;
        // a second apply pass over settled rows writes nothing new. The
        // row builds inline with empty why and done_means so the
        // placeholder path renders.
        let rows = vec![serde_json::json!({
            "id": "ab-9",
            "title": "Ship the persona page",
            "project": "fno",
            "tags": ["some-other-tag"],
            "audience": "",
            "why": "",
            "done_means": "",
            "completed_at": "",
        })];
        let mut updates: Vec<Vec<String>> = Vec::new();
        let (_, mapped, unstamped) =
            backfill_into(&rows, true, Some(&review), &mut |cmd: &[String]| {
                updates.push(cmd.to_vec());
                Ok(())
            });
        assert_eq!(mapped, 0);
        assert_eq!(unstamped, 1);
        assert!(updates.is_empty(), "no store write for an unmapped row");
        let text = std::fs::read_to_string(&review).unwrap();
        assert!(text.contains("## ab-9"), "{text:?}");
        assert!(
            text.contains("none on the row; draft one before stamping"),
            "the why placeholder: {text:?}"
        );
        assert!(
            text.contains("none on the row; state how done is verified"),
            "the done-means placeholder: {text:?}"
        );
        // Idempotence: the row settled (audience set) reruns as a skip.
        let settled = vec![row("ab-9", &["some-other-tag"], "customers", "")];
        let (code, mapped, unstamped) =
            backfill_into(&settled, true, Some(&review), &mut |cmd: &[String]| {
                updates.push(cmd.to_vec());
                Ok(())
            });
        assert_eq!(code, 0);
        assert_eq!(mapped, 0, "a settled row maps no more");
        assert_eq!(unstamped, 0, "a settled row proposes no more");
        std::fs::remove_dir_all(&dir).ok();
    }
}
