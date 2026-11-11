//! `--all`: move every live claude worker on this host, one at a time, in
//! registry order. It stops at the first failure and names the workers left.
//! A lead (any row with a `role_scope`) is not resumed on the target: its
//! canon doc is copied and the hand-off command is printed for a successor.

use super::{carry, check_host, read_rows, Locate, MoveError, Options, Row, Shell, Step};
use std::path::PathBuf;

/// The rows `--all` sorts into three piles.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AllPlan {
    pub workers: Vec<String>,
    pub leads: Vec<(String, String)>,
    pub skipped: Vec<(String, String)>,
}

/// Sort rows: a live claude row with no scope is a worker, with a scope a
/// lead; anything else is skipped with its reason.
pub fn partition(rows: &[Row]) -> AllPlan {
    let mut plan = AllPlan::default();
    for row in rows {
        if row.harness != "claude" {
            plan.skipped.push((
                row.name.clone(),
                format!("{} is not built yet", row.harness),
            ));
        } else if row.liveness.as_deref() != Some("alive") {
            plan.skipped.push((row.name.clone(), "not live".into()));
        } else if let Some(scope) = row.role_scope.as_deref().filter(|s| !s.is_empty()) {
            plan.leads.push((row.name.clone(), scope.to_string()));
        } else {
            plan.workers.push(row.name.clone());
        }
    }
    plan
}

/// The workers after position `i`: the ones a stopped `--all` did not move.
pub fn left_after(workers: &[String], i: usize) -> Vec<&str> {
    workers.iter().skip(i + 1).map(String::as_str).collect()
}

/// The hand-off line for a lead: a successor on the target takes the scope.
fn hand_off(scope: &str, host: &str) -> String {
    format!("on {host}, a person runs: fno agents org promote <handle> --scope {scope}")
}

pub fn render_dry_run(plan: &AllPlan, host: &str) -> String {
    let mut out =
        format!("dry run: move this host's live claude sessions -> {host}; nothing is moved\n");
    for name in &plan.workers {
        out.push_str(&format!("  move {name}\n"));
    }
    for (name, scope) in &plan.leads {
        out.push_str(&format!(
            "  carry canon doc for {name} (scope {scope}); no resume\n"
        ));
        out.push_str(&format!("  hand off: {}\n", hand_off(scope, host)));
    }
    for (name, why) in &plan.skipped {
        out.push_str(&format!("  skip {name}: {why}\n"));
    }
    out
}

/// True when the target's canon doc is newer than the source's: copying
/// would overwrite later work, so the lead's carry fails instead.
fn target_is_newer(shell: &dyn Shell, host: &str, doc: &std::path::Path) -> bool {
    match (
        super::local_mtime(doc),
        super::remote_mtime(shell, host, doc),
    ) {
        (Some(source), Some(target)) => target > source,
        _ => false,
    }
}

/// The canon doc path for a scope, from `fno config paths handoff`.
fn canon_path(shell: &dyn Shell, scope: &str) -> Option<PathBuf> {
    let args = ["config", "paths", "handoff", "--scope", scope].map(String::from);
    let out = shell.run("fno", &args).ok()?;
    let path = out.stdout.trim();
    (out.code == 0 && !path.is_empty()).then(|| PathBuf::from(path))
}

pub(super) fn run(opts: &Options, shell: &dyn Shell, local_host: &str, locate: Locate<'_>) -> i32 {
    let Some(host) = opts.to.as_deref() else {
        return 2;
    };
    if let Err(r) = check_host(host, local_host) {
        eprintln!("fno agents session move: {r}");
        return 2;
    }
    let rows = match read_rows(shell) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno agents session move: {e}");
            return 1;
        }
    };
    let plan = partition(&rows);
    if opts.dry_run {
        print!("{}", render_dry_run(&plan, host));
        return 0;
    }
    for (i, name) in plan.workers.iter().enumerate() {
        let one = Options {
            name: Some(name.clone()),
            to: Some(host.to_string()),
            all: false,
            dry_run: false,
        };
        let code = super::move_one(&one, shell, local_host, locate);
        if code != 0 {
            let left = left_after(&plan.workers, i);
            eprintln!(
                "fno agents session move --all: {name} failed; not moved: {}",
                left.join(", ")
            );
            return 1;
        }
    }
    for (name, scope) in &plan.leads {
        let carried = match canon_path(shell, scope) {
            None => Err(MoveError::failed(
                Step::CanonDocSync,
                format!("no canon doc for scope {scope}"),
            )),
            Some(doc) if target_is_newer(shell, host, &doc) => Err(MoveError::failed(
                Step::CanonDocSync,
                format!("{host} holds a newer canon doc at {}", doc.display()),
            )),
            Some(doc) => carry::carry_canon_doc(shell, host, &doc),
        };
        match carried {
            Ok(()) => println!("{name}: canon doc carried. {}", hand_off(scope, host)),
            Err(e) => {
                eprintln!("fno agents session move --all: {name}: {e}");
                return 1;
            }
        }
    }
    for (name, why) in &plan.skipped {
        println!("skipped {name}: {why}");
    }
    0
}
