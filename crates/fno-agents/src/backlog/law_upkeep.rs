//! `fno inbox law upkeep`: the nightly pass that acts on stale laws.
//!
//! The recheck reader already computes every verdict and the check-in prints
//! it, but nothing acts on a stale row: `recheck --apply` retracts only
//! guard-fired laws, so a law whose premise moved sits stale forever. This
//! door is the actor. A stale law whose row carries no `quote` retracts with
//! a receipt naming the moved reading; a stale law holding the user's own
//! words files ONE outstanding question and never a silent delete. Guard-
//! fired laws keep their existing retraction under `--apply`. `install`
//! schedules the pass nightly through launchd on the groom installer's
//! pattern, so the check-in `recheck` contract stays byte-compatible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::outstanding;
use crate::decision_index;
use crate::law_match::{
    evidence_repo_root, row_id, row_subject, write_retraction_event, RetractOutcome, RetractWrite,
};
use crate::law_recheck::{guard_verdict, reading_verdict, Verdict};
use crate::pr_watch::refresh;
use crate::question_intake::{run_intake, IntakeRequest};

const USAGE: &str = "usage: fno inbox law upkeep run [--apply] [--json] | fno inbox law upkeep install [--hour N] [--dry-run] | fno inbox law upkeep status [--json] | fno inbox law upkeep uninstall";

/// After groom's default hour (2), so the nightly passes never race.
const HOUR_DEFAULT: i64 = 2;
const MINUTE: i64 = 30;

const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  Nightly law upkeep: retires stale laws against merged code and CI, and
  asks instead of deleting when a law holds the user's own words.
-->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{fno_binary}</string>
    <string>inbox</string>
    <string>law</string>
    <string>upkeep</string>
    <string>run</string>
    <string>--apply</string>
  </array>

  <!-- launchd launches with a minimal PATH; capture install-time PATH so
       fno / gh resolve without a login shell. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{home}</string>
  </dict>

  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>{hour}</integer>
    <key>Minute</key>
    <integer>30</integer>
  </dict>

  <key>RunAtLoad</key>
  <false/>

  <key>ProcessType</key>
  <string>Standard</string>

  <!-- A repo root: the stored readings run against the evidence root the
       install captured, not $HOME. -->
  <key>WorkingDirectory</key>
  <string>{workdir}</string>

  <key>StandardOutPath</key>
  <string>{log_out}</string>

  <key>StandardErrorPath</key>
  <string>{log_err}</string>
</dict>
</plist>
"#;

/// Why the pass could not run. Typed, so the door prints one line and the
/// caller keeps a machine-readable cause.
#[derive(Debug)]
pub(crate) enum UpkeepError {
    IndexRead(String),
    Launch(String),
}

impl std::fmt::Display for UpkeepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpkeepError::IndexRead(e) => write!(f, "the decision index could not be read ({e})"),
            UpkeepError::Launch(e) => write!(f, "launchd refused ({e})"),
        }
    }
}

/// What the stale arm does with one law. Pure, so tests cover the decision
/// and the pass only wires the IO.
#[derive(Debug, PartialEq)]
enum StaleAction {
    Retract,
    Question,
    Report,
}

fn stale_action(row: &Value, apply: bool) -> StaleAction {
    if !apply {
        return StaleAction::Report;
    }
    let quoted = row
        .pointer("/quote")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|q| !q.is_empty());
    if quoted {
        StaleAction::Question
    } else {
        StaleAction::Retract
    }
}

enum QuestionOutcome {
    Filed(String),
    AlreadyOpen,
    Refused(String),
}

/// One outstanding question per stale episode: an open question naming the
/// law id short-circuits the filing, so a nightly pass never re-asks.
fn file_stale_question(
    id: &str,
    subject: &str,
    why: &str,
    quote: &str,
    repo_root: &Path,
) -> QuestionOutcome {
    let index = outstanding::project_log(repo_root, "questions.jsonl");
    match outstanding::read_open_questions(&index) {
        Ok(open) => {
            if open.iter().any(|q| q.question.contains(id)) {
                return QuestionOutcome::AlreadyOpen;
            }
        }
        Err(e) => return QuestionOutcome::Refused(e),
    }
    let question = format!(
        "Law upkeep: law {id} \"{subject}\" holds the user's own words and its premise moved: \
         {why} Their words: \"{quote}\" Retire, renew or restore it by hand: \
         fno inbox law retract {id} --reason \"<why>\" | fno inbox law renew {id} \
         --until <date> | fno inbox law restore {id}. Nothing was deleted."
    );
    let request: IntakeRequest = match serde_json::from_value(json!({
        "question": question,
        "template": false,
        "options": [],
        "blocks": [],
        "laws": [],
        "storage_root": repo_root.display().to_string(),
        "index_path": index.display().to_string(),
        "subject": id,
        "asker": "law-upkeep",
        "cwd": repo_root.display().to_string(),
    })) {
        Ok(r) => r,
        Err(e) => return QuestionOutcome::Refused(e.to_string()),
    };
    let home = crate::paths::AgentsHome::from_env();
    let answer = run_intake(&request, &home);
    match answer.qid {
        Some(qid) if answer.exit_code == 0 => QuestionOutcome::Filed(qid),
        _ => QuestionOutcome::Refused(answer.lines.join("; ")),
    }
}

/// Live laws grouped by normalized subject; groups past one are duplicate
/// proposals, report-only. The one-time set audit owns the merge work; this
/// pass only feeds it evidence.
fn duplicate_groups(rows: &[Value]) -> Vec<Value> {
    let mut by_subject: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows.iter().filter(|r| decision_index::is_law(r)) {
        let subject = row_subject(row).trim().to_lowercase();
        by_subject
            .entry(subject)
            .or_default()
            .push(row_id(row).to_string());
    }
    by_subject
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(subject, ids)| json!({"subject": subject, "decision_ids": ids}))
        .collect()
}

/// One upkeep pass over the live laws. `apply` retracts guard-fired laws
/// (the existing guard arm) and stale quote-free laws, and files one
/// question per stale law that carries the user's own words. Every count
/// rides the receipt so the caller can assert acted == stale.
pub(crate) fn upkeep_pass(apply: bool) -> Result<Value, UpkeepError> {
    let (rows, damaged) = match decision_index::read_store_rows(
        &crate::graph_get::default_graph_path(),
        &decision_index::default_state_path("decisions.jsonl"),
    ) {
        Ok(read) => read,
        // A project that never recorded a law has no store: nothing to keep.
        Err(e) if e.starts_with("no decision store") => (Vec::new(), 0),
        Err(e) => return Err(UpkeepError::IndexRead(e)),
    };
    let index = decision_index::derive_rows_unguarded(rows, damaged);
    let entries = crate::backlog::decisions_cli::read_graph_entries().ok();
    let root = evidence_repo_root();
    let mut runner = |cmd: &str, root: &Path| crate::evidence::shell_run(cmd, root, 20);
    let now = chrono::Utc::now().to_rfc3339();
    let mut report: Vec<Value> = Vec::new();
    let (mut stale, mut expired, mut unknown, mut retracted, mut questioned) = (0, 0, 0, 0, 0);
    let (mut retracted_stale, mut retracted_expired) = (0, 0);
    for row in index.rows.iter().filter(|r| decision_index::is_law(r)) {
        let guard = guard_verdict(row, entries.as_deref(), &now);
        let verdict = match guard {
            Verdict::Ok => reading_verdict(row, &mut runner, &root),
            other => other,
        };
        let (state, why) = match &verdict {
            Verdict::Ok => continue,
            Verdict::Expired(w) => ("expired", w.clone()),
            Verdict::Stale(w) => ("stale", w.clone()),
            Verdict::Unknown(w) => ("unknown", w.clone()),
        };
        match state {
            "expired" => expired += 1,
            "stale" => stale += 1,
            _ => unknown += 1,
        }
        let id = row_id(row).to_string();
        let mut action = "report".to_string();
        let mut done = false;
        let retractable = state == "expired"
            || (state == "stale" && stale_action(row, apply) == StaleAction::Retract);
        if apply && retractable {
            let outcome = write_retraction_event(&RetractWrite {
                target_decision_id: id.clone(),
                subject: row_subject(row).trim().to_string(),
                reason: format!("{state}: {why}"),
                retracted_by: "law-upkeep".into(),
                authority_source: if state == "expired" {
                    "guard"
                } else {
                    "reading"
                },
                attested_by: None,
                relayed_by: None,
                quote: None,
            });
            done = matches!(outcome, RetractOutcome::Done);
            if done {
                retracted += 1;
                if state == "stale" {
                    retracted_stale += 1;
                } else {
                    retracted_expired += 1;
                }
                action = "retract".into();
            }
        } else if apply && state == "stale" {
            let quote = row
                .pointer("/quote")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            match file_stale_question(&id, row_subject(row).trim(), &why, &quote, &root) {
                QuestionOutcome::Filed(qid) => {
                    questioned += 1;
                    action = format!("question:{qid}");
                }
                QuestionOutcome::AlreadyOpen => action = "question:open".into(),
                QuestionOutcome::Refused(e) => action = format!("question refused: {e}"),
            }
        }
        report.push(json!({
            "decision_id": id,
            "subject": row_subject(row),
            "state": state,
            "why": why,
            "action": action,
            "retracted": done,
        }));
    }
    Ok(json!({
        "laws": report,
        "stale": stale,
        "expired": expired,
        "unknown": unknown,
        "retracted": retracted,
        "retracted_stale": retracted_stale,
        "retracted_expired": retracted_expired,
        "questioned": questioned,
        "duplicates": duplicate_groups(index.rows.as_slice()),
    }))
}

/// The one-line human summary: empty when nothing needed acting on. Every
/// count rides the receipt, so the summary reads and never recounts.
fn summary_line(reading: &Value) -> String {
    let n = |k: &str| {
        reading
            .pointer(&format!("/{k}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    if n("stale") + n("expired") + n("unknown") == 0 {
        return String::new();
    }
    format!(
        "laws upkeep: {} stale ({} retracted, {} questioned), {} expired ({} retracted), {} unknown; duplicates: {}",
        n("stale"),
        n("retracted_stale"),
        n("questioned"),
        n("expired"),
        n("retracted_expired"),
        n("unknown"),
        reading
            .pointer("/duplicates")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0),
    )
}

fn home() -> PathBuf {
    crate::paths::AgentsHome::from_env().root().to_path_buf()
}

/// This HOME's law-upkeep launchd label.
fn label() -> String {
    crate::launchd_labels::law_upkeep_label()
}

fn launch_agents_dir() -> PathBuf {
    home().join("Library").join("LaunchAgents")
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|p| p.display().to_string())
}

/// The repo root the scheduled run works from: the `FNO_REPO_ROOT` pin, else
/// the checkout around the cwd.
fn repo_root() -> Option<String> {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        let pin = PathBuf::from(pin);
        return Some(pin.canonicalize().unwrap_or(pin).display().to_string());
    }
    let cwd = std::env::current_dir().ok()?;
    Some(crate::paths::worktree_repo_root(&cwd).display().to_string())
}

/// Render the nightly law-upkeep plist. No filesystem writes.
fn render_plist(fno_binary: &str, install_path: &str, hour: i64, workdir: Option<&str>) -> String {
    let home_s = home().display().to_string();
    let logs = home().join(".fno").join("logs");
    let esc = refresh::xml_escape;
    PLIST
        .replace("{label}", &esc(&label()))
        .replace("{fno_binary}", &esc(fno_binary))
        .replace("{path}", &esc(&refresh::augment_path(install_path)))
        .replace("{home}", &esc(&home_s))
        .replace("{workdir}", &esc(workdir.unwrap_or(&home_s)))
        .replace("{hour}", &hour.to_string())
        .replace(
            "{log_out}",
            &esc(&logs.join("law-upkeep.out.log").display().to_string()),
        )
        .replace(
            "{log_err}",
            &esc(&logs.join("law-upkeep.err.log").display().to_string()),
        )
}

/// Write the plist and bootstrap it into launchd, on the groom installer's
/// bounce without its kickstart: the pass mutates the law store, so forcing
/// a run at install time would act outside its nightly window.
fn install_agent(hour: i64, dry_run: bool) -> Value {
    let fno_binary = which("fno").unwrap_or_else(|| "fno".into());
    let install_path = refresh::default_agent_path(&fno_binary);
    let state = super::settings::state_dir().unwrap_or_else(home);
    let _ = std::fs::create_dir_all(state.join("logs"));
    let workdir = repo_root();
    let rendered = render_plist(&fno_binary, &install_path, hour, workdir.as_deref());
    if dry_run {
        return json!({"status": "dry-run", "plist": rendered});
    }
    if !cfg!(target_os = "macos") {
        return json!({
            "status": "unsupported",
            "detail": "launchd is macOS-only; the cron line runs the same command",
            "cron": format!("30 {hour} * * * {fno_binary} inbox law upkeep run --apply"),
        });
    }
    let plist_path = launch_agents_dir().join(crate::launchd_labels::LAW_UPKEEP_PLIST_FILENAME);
    if let Some(dir) = plist_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match refresh::write_if_changed(&plist_path, &rendered) {
        Ok(false) => {
            return json!({"status": "unchanged", "detail": "plist matches; not re-registered"})
        }
        Ok(true) => {}
        Err(err) => {
            return json!({"status": "failed", "detail": format!("write {}: {err}", plist_path.display())})
        }
    }
    let (msg, rc) = refresh::bounce(&plist_path, &label(), false, false, "unknown", &state);
    json!({
        "status": if rc == 0 { "installed" } else { "failed" },
        "plist": plist_path.display().to_string(),
        "hour": hour,
        "workdir": workdir,
        "detail": msg,
    })
}

/// Truthful installed state: the plist on disk AND the label loaded in
/// launchd, plus the last run's age from the out log.
fn status_json() -> Value {
    let label = label();
    let plist_path = launch_agents_dir().join(crate::launchd_labels::LAW_UPKEEP_PLIST_FILENAME);
    let loaded = Command::new("launchctl")
        .arg("list")
        .arg(&label)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let installed = plist_path.is_file() && loaded;
    let log = home().join(".fno").join("logs").join("law-upkeep.out.log");
    let age = std::fs::metadata(&log)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| (d.as_secs_f64() / 3600.0 * 10.0).round() / 10.0);
    json!({
        "installed": installed,
        "label": label,
        "plist": plist_path.display().to_string(),
        "log": log.display().to_string(),
        "last_run_age_hours": age,
    })
}

fn uninstall_agent() -> Value {
    let label = label();
    let plist_path = launch_agents_dir().join(crate::launchd_labels::LAW_UPKEEP_PLIST_FILENAME);
    if cfg!(target_os = "macos") {
        // SAFETY: getuid reads a per-process kernel value; it cannot fail.
        let uid = unsafe { libc::getuid() };
        let outcome = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{label}")])
            .output();
        if let Err(e) = outcome {
            return json!({"status": "failed", "detail": format!("launchctl bootout: {e}")});
        }
    }
    match std::fs::remove_file(&plist_path) {
        Ok(()) => json!({"status": "uninstalled", "label": label}),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"status": "absent", "label": label})
        }
        Err(e) => {
            json!({"status": "failed", "detail": format!("remove {}: {e}", plist_path.display())})
        }
    }
}

/// The `fno inbox law upkeep` door. The worker owns stdout and the exit code
/// (0 answered or acted, 1 the pass could not run, 2 usage).
pub(crate) fn run_upkeep(argv: &[String]) -> i32 {
    let Some(action) = argv.first() else {
        eprintln!("fno inbox law upkeep: a subcommand is required (run | install | status | uninstall)\n{USAGE}");
        return 2;
    };
    let rest = &argv[1..];
    match action.as_str() {
        "run" => {
            let mut apply = false;
            let mut as_json = false;
            for a in rest {
                match a.as_str() {
                    "--apply" => apply = true,
                    "--json" | "-J" => as_json = true,
                    _ => {
                        eprintln!("fno inbox law upkeep run: no such option: {a}\n{USAGE}");
                        return 2;
                    }
                }
            }
            match upkeep_pass(apply) {
                Ok(reading) => {
                    if as_json {
                        println!("{reading}");
                    } else {
                        println!("{}", summary_line(&reading));
                    }
                    0
                }
                Err(e) => {
                    eprintln!("fno inbox law upkeep run: {e}");
                    1
                }
            }
        }
        "install" => {
            let mut hour = HOUR_DEFAULT;
            let mut dry_run = false;
            let mut args = rest.iter();
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--dry-run" => dry_run = true,
                    "--hour" => match args.next().and_then(|v| v.parse::<i64>().ok()) {
                        Some(h) => hour = h,
                        None => {
                            eprintln!(
                                "fno inbox law upkeep install: --hour needs a number\n{USAGE}"
                            );
                            return 2;
                        }
                    },
                    _ => {
                        eprintln!("fno inbox law upkeep install: no such option: {a}\n{USAGE}");
                        return 2;
                    }
                }
            }
            println!("{}", install_agent(hour, dry_run));
            0
        }
        "status" => {
            println!("{}", status_json());
            0
        }
        "uninstall" => {
            println!("{}", uninstall_agent());
            0
        }
        _ => {
            eprintln!("fno inbox law upkeep: no such subcommand: {action}\n{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fields: Value) -> Value {
        let mut base = json!({"decision_id": "d-test0001", "subject": "s"});
        if let (Some(base), Some(over)) = (base.as_object_mut(), fields.as_object()) {
            for (k, v) in over {
                base.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn a_quote_free_stale_law_retracts_and_a_quoted_one_only_asks() {
        let bare = row(json!({}));
        let quoted = row(json!({"quote": "merge main, never rebase"}));
        assert_eq!(stale_action(&bare, true), StaleAction::Retract);
        assert_eq!(stale_action(&quoted, true), StaleAction::Question);
        // Without apply the pass reports and touches nothing.
        assert_eq!(stale_action(&bare, false), StaleAction::Report);
        assert_eq!(stale_action(&quoted, false), StaleAction::Report);
        // A blank quote is no quote: the user's own words must be real.
        let blank = row(json!({"quote": "   "}));
        assert_eq!(stale_action(&blank, true), StaleAction::Retract);
    }

    #[test]
    fn same_subject_in_two_laws_reads_as_one_duplicate_group() {
        let groups = duplicate_groups(&[
            json!({"decision_id": "d-aaa", "subject": "Merge Main"}),
            json!({"decision_id": "d-bbb", "subject": "merge main"}),
            json!({"decision_id": "d-ccc", "subject": "other law"}),
        ]);
        assert_eq!(groups.len(), 1, "{groups}");
        assert_eq!(groups[0]["subject"], "merge main");
        assert_eq!(groups[0]["decision_ids"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn plist_renders_the_nightly_apply_command() {
        let rendered = render_plist("/usr/local/bin/fno", "/usr/local/bin", 3, Some("/repo"));
        assert!(
            rendered.contains("<key>StartCalendarInterval</key>"),
            "{rendered}"
        );
        assert!(rendered.contains("<integer>3</integer>"), "{rendered}");
        assert!(rendered.contains("<string>--apply</string>"), "{rendered}");
        assert!(rendered.contains("law</string>"), "{rendered}");
        assert!(rendered.contains("/repo"), "{rendered}");
    }

    #[test]
    fn the_door_refuses_an_unknown_subcommand_with_usage() {
        let code = run_upkeep(&["nope".to_string()]);
        assert_eq!(code, 2);
        assert_eq!(run_upkeep(&[]), 2);
    }
}
