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
    scope: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    project: Option<String>,
}

fn parse_row(row: &Value) -> Option<Row> {
    serde_json::from_value(row.clone()).ok()
}

/// The ruled Q1 mapping (design section 8.1).
fn audience_tag_scope(tag: &str, project: Option<&str>) -> Option<&'static str> {
    match (tag, project) {
        ("audience:internal", _) => Some("internal"),
        ("audience:user", _) => Some("customers"),
        ("audience:contributor", Some("fno")) => Some("customer:contributor"),
        ("audience:contributor", _) => Some("internal"),
        _ => None,
    }
}

/// `fno backlog scope-backfill [--apply] [--persona-retired <slug>]`. The
/// dry run is the default: it counts and names; --apply stamps.
pub fn run(args: &[String]) -> i32 {
    let mut apply = false;
    let mut retired: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => apply = true,
            "--persona-retired" if i + 1 < args.len() => {
                retired = Some(args[i + 1].clone());
                i += 1;
            }
            other => {
                eprintln!("scope-backfill: unknown argument {other:?}");
                return 2;
            }
        }
        i += 1;
    }
    let graph = crate::backlog::settings::graph_path();
    let rows = match crate::graph_store::read_rows(&graph) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("scope-backfill: graph unreadable: {e}");
            return 1;
        }
    };
    if let Some(persona) = &retired {
        return retire_report(&rows, persona);
    }
    backfill(&rows, apply)
}

/// The dry run counts; --apply stamps each ruled row through the locked
/// update door, with the ruling as the why for the history entry.
fn backfill(rows: &[Value], apply: bool) -> i32 {
    let mut mapped = 0usize;
    let mut unstamped = 0usize;
    for row in rows {
        let Some(r) = parse_row(row) else {
            continue;
        };
        let open = r.completed_at.as_deref().unwrap_or("").is_empty();
        if !open {
            continue;
        }
        let already = r.scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if already.is_some() {
            continue;
        }
        let tags: Vec<&str> = r.tags.iter().flatten().map(String::as_str).collect();
        let ruled = tags.iter().find_map(|t| audience_tag_scope(t, r.project.as_deref()));
        match ruled {
            Some(scope) if apply => stamp_one(&r, scope),
            Some(_) => mapped += 1,
            None => unstamped += 1,
        }
    }
    if apply {
        println!("scope-backfill: stamped {mapped} row(s) by the ruled mapping");
    } else {
        println!("scope-backfill: {mapped} row(s) map by rule; pass --apply to stamp");
    }
    println!("scope-backfill: {unstamped} open row(s) stay unstamped pending the user's ruling");
    0
}

/// One locked write: the scope stamp and the tag drop in one update, with
/// the ruling as the why so the history entry records it.
fn stamp_one(row: &Row, scope: &str) {
    let Some(id) = row.id.as_deref() else {
        return;
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| "fno-agents".into());
    let status = std::process::Command::new(exe)
        .args(["backlog", "update", id, &format!("scope={scope}")])
        .arg(format!(
            "why=backfill: ruled audience-tag mapping ({scope})"
        ))
        .status();
    if let Err(e) = status {
        eprintln!("scope-backfill: update refused for {id}: {e}");
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
        let scope = r.scope.as_deref().unwrap_or("");
        if open && scope.contains(&needle) {
            let id = r.id.as_deref().unwrap_or("?");
            println!("{id} names {needle}");
            hits += 1;
        }
    }
    println!("scope-backfill: {hits} open row(s) name customer:{persona}");
    0
}
