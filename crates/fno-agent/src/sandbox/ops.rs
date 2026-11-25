//! The loop ops a sandboxed worker may ask the host for. Default deny: an op
//! is one of these typed variants, the host builds every argv from the spawn
//! it owns, and guest text crosses by value only. No op takes a node, branch,
//! repo, recipient, path or argv from the guest. There is no merge, attest,
//! spawn, decide or backlog-update op; the refusal names the lead.

use super::{Refusal, Res};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use super::engine::Engine;

/// The guest-socket method a client inside calls with one op.
pub const OP_METHOD: &str = "sandbox.op";
/// The most guest text one op carries.
pub const MAX_TEXT: usize = 64 * 1024;
/// One host command an op runs.
const HOST_BOUND_S: u64 = 300;
/// The Stop decision, which may read GitHub several times.
const STOP_BOUND_S: u64 = 600;
const DEFAULT_BRANCHES: [&str; 4] = ["main", "master", "develop", "dev"];

/// The three kinds of mail a sandbox may send its spawner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailKind {
    Status,
    Question,
    Blocked,
}

/// One op. The JSON form is `{"op": "<name>", ...fields}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    NodeGet,
    NodeNote {
        text: String,
    },
    TargetStart,
    TargetStatus,
    ClaimStatus,
    /// `item` is an unknown id (`U3`) or a route (`route:2`).
    PlanSettle {
        item: String,
        text: String,
    },
    Mail {
        kind: MailKind,
        text: String,
    },
    Push,
    PrCreate {
        title: String,
        body: String,
    },
    PrStatus,
    PrComments,
    PrReply {
        comment: u64,
        text: String,
    },
    LoopStop {
        message: String,
    },
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Self::NodeGet => "node-get",
            Self::NodeNote { .. } => "node-note",
            Self::TargetStart => "target-start",
            Self::TargetStatus => "target-status",
            Self::ClaimStatus => "claim-status",
            Self::PlanSettle { .. } => "plan-settle",
            Self::Mail { .. } => "mail",
            Self::Push => "push",
            Self::PrCreate { .. } => "pr-create",
            Self::PrStatus => "pr-status",
            Self::PrComments => "pr-comments",
            Self::PrReply { .. } => "pr-reply",
            Self::LoopStop { .. } => "loop-stop",
        }
    }

    /// Every text field within its cap. Run before the op leaves the guest
    /// and again on the host.
    pub fn check(&self) -> Res<()> {
        let fields: Vec<&str> = match self {
            Self::NodeNote { text } | Self::Mail { text, .. } | Self::PrReply { text, .. } => {
                vec![text.as_str()]
            }
            Self::PlanSettle { item, text } => vec![item.as_str(), text.as_str()],
            Self::PrCreate { title, body } => vec![title.as_str(), body.as_str()],
            Self::LoopStop { message } => vec![message.as_str()],
            _ => Vec::new(),
        };
        if fields
            .iter()
            .any(|f| f.len() > MAX_TEXT || f.contains('\0'))
        {
            return Err(Refusal(format!(
                "{} carries text over {MAX_TEXT} bytes or a NUL",
                self.name()
            )));
        }
        Ok(())
    }
}

/// The refusal for a command the sandbox has no op for.
pub fn refused(what: &str) -> Refusal {
    Refusal(format!(
        "the sandbox refuses `{}`: a sandboxed worker may ask for node get and note, \
         target start and status, claim status, plan settle, mail, push, PR create, \
         status, comments and reply, and loop stop. The lead does everything else, \
         including merge, review attestation, spawn and backlog updates.",
        super::escape(what, 120)
    ))
}

/// What the host bound at spawn. The guest never names any of it.
#[derive(Debug)]
pub struct Bound {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub session: Option<String>,
    /// The spawner, the only mail recipient.
    pub from: Option<String>,
    pub node: Option<String>,
    pub branch: Option<String>,
    /// The host worktree the clone came from. Its HEAD stays at `base`.
    pub worktree: PathBuf,
    pub plan: Option<PathBuf>,
    pub base: String,
    pub store: PathBuf,
    pub transcript: PathBuf,
    pub engine: Engine,
    /// One export at a time: push and loop stop share the guest's out dir.
    pub export: Mutex<()>,
}

impl Bound {
    fn node(&self) -> Res<&str> {
        self.node
            .as_deref()
            .ok_or_else(|| "this sandbox is bound to no node".into())
    }

    fn branch(&self) -> Res<&str> {
        match self.branch.as_deref() {
            Some(b) if !DEFAULT_BRANCHES.contains(&b) => Ok(b),
            Some(b) => Err(Refusal(format!(
                "the bound branch {b} is a default branch; a sandbox never pushes there"
            ))),
            None => Err("this sandbox is bound to no branch".into()),
        }
    }

    /// The host env a command runs under: the lane's own env (this
    /// process's), plus the session the claim and manifest are keyed to.
    fn env(&self, c: &mut Command) {
        c.current_dir(&self.worktree);
        c.env_remove("FNO_AGENT_SANDBOXED");
        if let (Some(sid), "claude") = (&self.session, self.harness.as_str()) {
            c.env("CLAUDE_CODE_SESSION_ID", sid);
        }
        if !self.name.is_empty() {
            c.env("FNO_AGENT_SELF", &self.name);
        }
    }
}

/// A sibling binary of this executable, else the name on PATH.
pub fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// One host command with a host-built argv. Returns stdout, or the refusal
/// that names the command and its stderr.
fn host(b: &Bound, bin: &str, args: &[&str]) -> Res<String> {
    let mut c = Command::new(sibling(bin));
    c.args(args);
    b.env(&mut c);
    let out = crate::bounded_cmd::output_with_timeout_result(c, HOST_BOUND_S)
        .map_err(|e| format!("{bin} {}: {e}", args.first().copied().unwrap_or("")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(Refusal(format!(
            "{bin} {} exited {}: {}",
            args.iter().take(3).copied().collect::<Vec<_>>().join(" "),
            out.status.code().unwrap_or(-1),
            super::escape(String::from_utf8_lossy(&out.stderr).trim(), 600)
        )))
    }
}

/// A git read on the host worktree with external diff and textconv off.
fn git(b: &Bound, args: &[&str]) -> Res<String> {
    let mut c = Command::new("git");
    c.args(["-c", "core.hooksPath=/dev/null", "-c", "diff.external="])
        .args(args);
    b.env(&mut c);
    let out = crate::bounded_cmd::output_with_timeout_result(c, HOST_BOUND_S)
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(Refusal(format!(
            "git {} exited {}: {}",
            args.first().copied().unwrap_or(""),
            out.status.code().unwrap_or(-1),
            super::escape(String::from_utf8_lossy(&out.stderr).trim(), 400)
        )))
    }
}

fn label(b: &Bound, kind: &str, text: &str) -> String {
    format!(
        "[sandbox {} {kind}, sandbox origin] {}",
        b.id,
        super::github::neutral(text.trim(), b.node.as_deref())
    )
}

/// Run one op for the spawn `b` owns. Every arm builds its own argv.
pub fn handle(b: &Bound, op: &Op) -> Res<Value> {
    op.check()?;
    match op {
        Op::NodeGet => node_get(b),
        Op::NodeNote { text } => {
            let node = b.node()?;
            host(
                b,
                "fno",
                &["backlog", "note", node, &label(b, "note", text)],
            )
            .map(|out| json!({ "noted": node, "receipt": out.trim() }))
        }
        Op::TargetStart => target_start(b),
        Op::TargetStatus => host(b, "fno", &["do", "target", "status"])
            .map(|out| json!({ "status": out, "worktree": super::CONTAINER_WORK })),
        Op::ClaimStatus => {
            let key = format!("node:{}", b.node()?);
            host(b, "fno", &["agent", "claim", "status", &key])
                .map(|out| json!({ "claim": key, "status": out }))
        }
        Op::PlanSettle { item, text } => plan_settle(b, item, text),
        Op::Mail { kind, text } => {
            let to = b
                .from
                .as_deref()
                .ok_or("this sandbox has no spawner to mail; it reports on its row only")?;
            let kind = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let body = label(b, &kind, text);
            let mut args = vec!["agent", "mail", "send", to, body.as_str()];
            if !b.name.is_empty() {
                args.extend(["--from-name", b.name.as_str()]);
            }
            host(b, "fno", &args).map(|_| json!({ "mailed": to }))
        }
        Op::Push => push(b),
        Op::PrCreate { title, body } => pr_create(b, title, body),
        Op::PrStatus => {
            let mut c = Command::new(sibling("fno"));
            c.args(["do", "pr", "status"]);
            b.env(&mut c);
            let out = crate::bounded_cmd::output_with_timeout_result(c, HOST_BOUND_S)
                .map_err(|e| format!("fno do pr status: {e}"))?;
            Ok(json!({
                "exit": out.status.code(),
                "status": String::from_utf8_lossy(&out.stdout).trim(),
            }))
        }
        Op::PrComments => pr_comments(b),
        Op::PrReply { comment, text } => {
            let pr = pr_number(b)?;
            let path = format!("repos/{{owner}}/{{repo}}/pulls/{pr}/comments/{comment}/replies");
            let body = format!(
                "body={}",
                super::github::reply(text, &b.id, b.node.as_deref())
            );
            host(b, "gh", &["api", "-X", "POST", &path, "-f", &body])
                .map(|_| json!({ "replied": comment, "pr": pr }))
        }
        Op::LoopStop { message } => loop_stop(b, message),
    }
}

fn node_get(b: &Bound) -> Res<Value> {
    let node = b.node()?;
    let read = |id: &str| -> Res<Value> {
        let out = host(b, "fno", &["backlog", "get", id])?;
        let start = out.find('{').unwrap_or(0);
        serde_json::from_str(out.get(start..).unwrap_or_default())
            .map_err(|e| Refusal(format!("fno backlog get {id}: {e}")))
    };
    #[derive(serde::Deserialize)]
    struct Parent {
        #[serde(default)]
        parent: Option<String>,
    }
    let got = read(node)?;
    let parent = serde_json::from_value::<Parent>(got.clone())
        .ok()
        .and_then(|p| p.parent)
        .filter(|p| !p.is_empty());
    let parent = match parent {
        Some(p) => read(&p).ok(),
        None => None,
    };
    Ok(json!({ "node": got, "parent": parent }))
}

/// One review comment as the host hands it in: GitHub content, not authority.
#[derive(serde::Deserialize, serde::Serialize)]
struct Comment {
    id: u64,
    #[serde(default)]
    user: Option<Login>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    body: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Login {
    login: String,
}

fn target_start(b: &Bound) -> Res<Value> {
    let plan = b
        .plan
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    Ok(json!({
        "node": b.node,
        "branch": b.branch,
        "worktree": super::CONTAINER_WORK,
        "base": b.base,
        "plan": plan,
        "plan_copy": "read-only; settle an unknown or a route with `fno do plan settle <U<n>|route:<n>> <text>`",
        "sandboxed": true,
        "no_merge": true,
    }))
}

/// Settle one line of a plan. `item` is `U<n>` (an open line under
/// `## Unknowns`) or `route:<n>` (a numbered line under `## Routes`). Any
/// other item, a line with a newline, an absent line or a second chosen line
/// refuses and leaves the text unchanged. Frontmatter is never touched.
pub fn settle(plan: &str, item: &str, text: &str) -> Res<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 500 || text.chars().any(char::is_control) {
        return Err("a settle line is one line of 1 to 500 characters".into());
    }
    let lines: Vec<&str> = plan.split('\n').collect();
    // The body starts after the closing frontmatter fence.
    let body_start = if lines.first().map(|l| l.trim()) == Some("---") {
        lines
            .iter()
            .skip(1)
            .position(|l| l.trim() == "---")
            .map_or(lines.len(), |i| i + 2)
    } else {
        0
    };
    let section = |name: &str| -> Option<(usize, usize)> {
        let start =
            (body_start..lines.len()).find(|&i| lines.get(i).map(|l| l.trim()) == Some(name))?;
        let end = (start + 1..lines.len())
            .find(|&i| lines.get(i).is_some_and(|l| l.starts_with("## ")))
            .unwrap_or(lines.len());
        Some((start + 1, end))
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    if let Some(n) = item
        .strip_prefix('U')
        .filter(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
    {
        let (from, to) = section("## Unknowns").ok_or("the plan has no ## Unknowns section")?;
        let open = format!("- [ ] U{n}:");
        let hits: Vec<usize> = (from..to)
            .filter(|&i| lines.get(i).is_some_and(|l| l.starts_with(&open)))
            .collect();
        let [at] = hits.as_slice() else {
            return Err(Refusal(format!(
                "no single open `{open}` line under ## Unknowns"
            )));
        };
        let line = lines.get(*at).copied().unwrap_or_default();
        let rest = line.strip_prefix("- [ ]").unwrap_or(line);
        if let Some(slot) = out.get_mut(*at) {
            *slot = format!("- [x]{rest} - settled: {text}");
        }
    } else if let Some(n) = item
        .strip_prefix("route:")
        .filter(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
    {
        let (from, to) = section("## Routes").ok_or("the plan has no ## Routes section")?;
        let numbered = format!("{n}. ");
        if !(from..to).any(|i| lines.get(i).is_some_and(|l| l.starts_with(&numbered))) {
            return Err(Refusal(format!("no route {n} under ## Routes")));
        }
        if (from..to).any(|i| lines.get(i).is_some_and(|l| l.starts_with("chosen:"))) {
            return Err("## Routes already has a chosen line".into());
        }
        // After the section's last non-blank line.
        let last = (from..to)
            .rev()
            .find(|&i| lines.get(i).is_some_and(|l| !l.trim().is_empty()))
            .unwrap_or(from.saturating_sub(1));
        out.insert(last + 1, format!("chosen: {n} - {text}"));
    } else {
        return Err(Refusal(format!(
            "settle names `{}`; it takes U<n> or route:<n>",
            super::escape(item, 40)
        )));
    }
    Ok(out.join("\n"))
}

fn plan_settle(b: &Bound, item: &str, text: &str) -> Res<Value> {
    let path = b
        .plan
        .as_deref()
        .ok_or("this sandbox is bound to no plan")?;
    let plan = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = super::github::neutral(text, b.node.as_deref());
    let next = settle(&plan, item, &format!("{text} (sandbox {})", b.id))?;
    std::fs::write(path, next).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(json!({ "settled": item }))
}

/// Export the live container's commits into `refs/sandbox/<id>/head`.
fn export(b: &Bound) -> Res<super::repo::Export> {
    let _one = b.export.lock().unwrap_or_else(|e| e.into_inner());
    super::repo::export(&b.engine, &b.id, &b.id, &b.base, &b.worktree, &b.store)
}

/// The files the sandbox head changes against its merge base with the
/// default branch, so a merge of main never counts.
fn changed_files(b: &Bound, head: &str) -> Res<Vec<String>> {
    let base = git(b, &["merge-base", "origin/main", head]).unwrap_or_else(|_| b.base.clone());
    let range = format!("{base}..{head}");
    let out = git(
        b,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            &range,
        ],
    )?;
    Ok(out.lines().map(str::to_string).collect())
}

/// The refusal for a head that edits CI: a push runs branch workflows with
/// the repository's secrets policy.
pub fn ci_edits(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.starts_with(".github/workflows/") || f.starts_with(".github/actions/"))
        .cloned()
        .collect()
}

fn push(b: &Bound) -> Res<Value> {
    let branch = b.branch()?.to_string();
    let out = export(b)?;
    let head = out
        .head
        .ok_or("the sandbox has no commits past its base; commit first")?;
    let ci = ci_edits(&changed_files(b, &head)?);
    if !ci.is_empty() {
        return Err(Refusal(format!(
            "the sandbox head edits CI ({}); a sandbox never pushes workflow or action edits. \
             The lead pushes those.",
            ci.join(", ")
        )));
    }
    let remote_head = git(b, &["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .ok()
        .and_then(|l| l.split_whitespace().next().map(str::to_string))
        .unwrap_or_default();
    let ctx = crate::pr_push::PushCtx {
        git_bin: "git".into(),
        gh_bin: "gh".into(),
        fno_bin: sibling("fno").display().to_string(),
        cwd: b.worktree.clone(),
        stamps_dir: crate::pr_push::default_stamps_dir(),
        force: false,
        // Fast-forward only: no lease, so a rewrite refuses at the remote.
        lease: None,
        remote_branch: Some(branch.clone()),
        source: Some(head.clone()),
    };
    match crate::pr_push::guarded_push(&ctx, &remote_head) {
        crate::pr_push::PushOutcome::Pushed { remote_branch, .. } => {
            Ok(json!({ "pushed": head, "branch": remote_branch }))
        }
        crate::pr_push::PushOutcome::InFlight { check, .. } => Err(Refusal(format!(
            "a run is in flight on {branch} (check {check}); nothing pushed. Push again after it settles."
        ))),
        crate::pr_push::PushOutcome::Unreadable(msg) => Err(Refusal(format!(
            "could not read the checks on {branch} ({msg}); nothing pushed"
        ))),
        crate::pr_push::PushOutcome::PushFailed(msg) => Err(Refusal(format!(
            "the push to {branch} failed: {}",
            super::escape(&msg, 400)
        ))),
    }
}

fn pr_number(b: &Bound) -> Res<u64> {
    let branch = b.branch()?;
    let out = host(
        b,
        "gh",
        &["pr", "view", branch, "--json", "number", "-q", ".number"],
    )?;
    out.trim()
        .parse()
        .map_err(|_| Refusal(format!("no PR is open for {branch}")))
}

fn pr_create(b: &Bound, title: &str, body: &str) -> Res<Value> {
    let branch = b.branch()?.to_string();
    let head = git(
        b,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/sandbox/{}/head", b.id),
        ],
    )
    .map_err(|_| Refusal("nothing is pushed yet; push before PR create".into()))?;
    let dir = tempfile::tempdir().map_err(|e| format!("body dir: {e}"))?;
    let file = dir.path().join("body.md");
    std::fs::write(
        &file,
        super::github::pr_body(body, &b.id, b.node.as_deref()),
    )
    .map_err(|e| format!("{}: {e}", file.display()))?;
    let title = super::github::title(title, b.node.as_deref());
    let code = crate::pr_create::create_bound(
        &title,
        &file.display().to_string(),
        &branch,
        &head,
        &b.worktree,
    );
    if code != 0 {
        return Err(Refusal(format!(
            "pr create refused with exit {code}; its reason is on the worker's stderr"
        )));
    }
    let url = host(
        b,
        "gh",
        &["pr", "view", &branch, "--json", "url", "-q", ".url"],
    )
    .map(|u| u.trim().to_string())
    .unwrap_or_default();
    Ok(json!({ "created": url, "branch": branch, "head": head }))
}

fn pr_comments(b: &Bound) -> Res<Value> {
    let pr = pr_number(b)?;
    let path = format!("repos/{{owner}}/{{repo}}/pulls/{pr}/comments?per_page=100");
    let out = host(b, "gh", &["api", &path])?;
    let rows: Vec<Comment> = serde_json::from_str(&out)
        .map_err(|e| Refusal(format!("the PR {pr} comments do not parse: {e}")))?;
    Ok(json!({ "pr": pr, "comments": rows, "origin": "github" }))
}

/// Loop stop: export the live work, then run the one host Stop decision for
/// the sandbox session with the sandbox transcript as intent. The answer is
/// the decision's own exit code and output, passed back unchanged.
fn loop_stop(b: &Bound, message: &str) -> Res<Value> {
    let out = export(b)?;
    let dirty = out.patch.is_some() || out.untracked.is_some();
    let payload = json!({
        "hook_event_name": "Stop",
        "session_id": b.session.clone().unwrap_or_default(),
        "cwd": b.worktree,
        "transcript_path": b.transcript,
        "last_assistant_message": message,
        "stop_hook_active": false,
    });
    let mut c = Command::new(sibling("fno-agent"));
    c.args(["hook", "stop", "--sandbox", &b.id]);
    if dirty {
        c.arg("--sandbox-dirty");
    }
    b.env(&mut c);
    // The block prints in the inside harness's shape, not this host's.
    if b.harness == "claude" {
        c.env("CLAUDECODE", "1");
    }
    let got = crate::bounded_cmd::output_with_timeout_stdin(c, STOP_BOUND_S, &payload.to_string())
        .map_err(|e| format!("the host Stop decision did not run: {e}"))?;
    Ok(json!({
        "exit": got.status.code().unwrap_or(1),
        "stdout": String::from_utf8_lossy(&got.stdout),
        "stderr": String::from_utf8_lossy(&got.stderr),
    }))
}

/// The worktree path the host bound for a spawn, read back from
/// `fno do target start`'s receipt.
pub fn receipt_worktree(receipt: &str) -> Option<PathBuf> {
    receipt
        .lines()
        .rev()
        .find_map(|l| {
            l.split_whitespace()
                .find_map(|w| w.strip_prefix("worktree="))
        })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The plan a host manifest binds, if any.
pub fn manifest_plan(worktree: &Path) -> Option<PathBuf> {
    let manifest = crate::target_orient::manifest_path(worktree)?;
    let text = std::fs::read_to_string(manifest).ok()?;
    crate::loopcheck::scan_manifest_field(&text, "plan_path")
        .filter(|p| !p.is_empty() && p != "null")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC7-PLAN: a settle that names frontmatter, carries a newline or names
    /// an absent line leaves the plan byte-identical; a valid unknown id
    /// changes that one line, and a route gains one chosen line.
    #[test]
    fn settle_edits_one_line_or_refuses() {
        let plan = "---\ntitle: x\n- [ ] U1: in frontmatter\n---\n# P\n\n## Routes\n\n1. one\n2. two\n\n## Unknowns\n\n- [ ] U1: open\n- [x] U2: done\n\n## Done\n";
        for (item, text) in [
            ("U2", "already settled"),
            ("U9", "absent"),
            ("U1", "two\nlines"),
            ("status", "ready"),
            ("route:7", "absent route"),
            ("U1", ""),
        ] {
            assert!(settle(plan, item, text).is_err(), "{item} {text:?}");
        }
        let out = settle(plan, "U1", "measured").unwrap();
        assert!(out.contains("- [ ] U1: in frontmatter"), "{out}");
        assert!(out.contains("- [x] U1: open - settled: measured"), "{out}");
        assert_eq!(out.lines().count(), plan.lines().count());
        let out = settle(plan, "route:2", "fewer parts").unwrap();
        assert!(out.contains("2. two\nchosen: 2 - fewer parts\n"), "{out}");
        assert!(settle(&out, "route:1", "again").is_err());
        let op: Op =
            serde_json::from_value(json!({"op": "plan-settle", "item": "U1", "text": "x"}))
                .unwrap();
        assert_eq!(op.name(), "plan-settle");
        assert!(serde_json::from_value::<Op>(json!({"op": "merge"})).is_err());
        assert!(serde_json::from_value::<Op>(json!({"op": "attest"})).is_err());
        assert_eq!(
            ci_edits(&[".github/workflows/ci.yml".into(), "src/a.rs".into()]),
            [".github/workflows/ci.yml"]
        );
        assert_eq!(
            receipt_worktree("x\nworktree=/w/x-1  base=origin/main\n"),
            Some(PathBuf::from("/w/x-1"))
        );
    }
}
