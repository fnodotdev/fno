//! `fno do review read`: serve the whole scoped diff, then record a receipt.
//!
//! The review lane calls this command in place of a hand-typed `git diff`.
//! The scope comes from git and the attestation journals, never from flags or
//! emit data: a later round starts at the newest attested head that also has
//! a receipt, else at the merge base. A clean `code-review` attestation needs
//! a receipt from the same session at the same head whose sha256 matches a
//! fresh computation of the scoped diff.
//!
//! A receipt proves the diff was served to this session. It does not prove
//! the diff was read or understood.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// What one `fno do review read` run records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub session: String,
    pub head: String,
    pub branch: String,
    pub base: String,
    pub files: Vec<String>,
    pub diff_sha256: String,
}

/// The range a round must have read: `base..head`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scope {
    pub head: String,
    pub branch: String,
    pub base: String,
}

/// The head and branch an emit names in its own data.
pub(crate) struct Claim<'a> {
    pub head: &'a str,
    pub branch: &'a str,
}

#[derive(Debug)]
pub(crate) enum ScopeError {
    Git(&'static str),
    Journal(String),
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(what) => write!(f, "git gave no answer for {what}"),
            Self::Journal(why) => write!(f, "the attestation journals could not be read: {why}"),
        }
    }
}

/// The scoped diff as served: bytes, sha256 and file names.
pub(crate) struct Served {
    pub diff: Vec<u8>,
    pub sha: String,
    pub files: Vec<String>,
}

#[derive(Deserialize)]
struct Identity {
    #[serde(default)]
    attester_session_id: String,
    #[serde(default)]
    attester_conflict: Option<String>,
}

fn git_bytes(args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args);
    let out =
        crate::config_doctor::bounded_output(&mut command, None, Duration::from_secs(120)).ok()?;
    out.status.success().then_some(out.stdout)
}

fn git_text(args: &[&str]) -> Option<String> {
    let text = String::from_utf8(git_bytes(args)?).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn receipts_dir() -> Option<PathBuf> {
    let common = PathBuf::from(git_text(&["rev-parse", "--git-common-dir"])?);
    let root = if common.is_absolute() {
        common
    } else {
        std::env::current_dir().ok()?.join(common)
    };
    Some(root.join("fno-review-reads"))
}

fn receipt_file(head: &str, session: &str) -> String {
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{head}-{safe}.json")
}

/// Does any session hold a receipt for this head?
fn has_receipt(head: &str) -> bool {
    let prefix = format!("{head}-");
    receipts_dir()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .is_some_and(|mut entries| {
            entries.any(|e| {
                e.ok()
                    .is_some_and(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            })
        })
}

/// The newest attested head that also has a receipt and is an ancestor of
/// `head`: where a later round's scope starts.
fn prior_base(branch: &str, head: &str) -> Result<Option<String>, ScopeError> {
    let mut journals = crate::config_cli::history::event_journals();
    for journal in &journals {
        crate::event_cli::hermetic::check(journal)
            .map_err(|e| ScopeError::Journal(e.to_string()))?;
    }
    journals.sort();
    journals.dedup();
    let found = super::review_prior_head::resolve_prior_head(branch, head, &journals)
        .map_err(|lines| ScopeError::Journal(lines.join("; ")))?;
    let Some(prior) = found else { return Ok(None) };
    let carried = has_receipt(&prior.sha)
        && git_bytes(&["merge-base", "--is-ancestor", &prior.sha, head]).is_some();
    Ok(carried.then_some(prior.sha))
}

pub(crate) fn scope() -> Result<Scope, ScopeError> {
    let head = git_text(&["rev-parse", "HEAD"]).ok_or(ScopeError::Git("HEAD"))?;
    let branch = git_text(&["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|b| b != "HEAD")
        .ok_or(ScopeError::Git("the branch (a detached HEAD has none)"))?;
    let default = git_text(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .unwrap_or_else(|| "origin/main".to_string());
    let merge_base =
        git_text(&["merge-base", &head, &default]).ok_or(ScopeError::Git("the merge base"))?;
    let base = prior_base(&branch, &head)?.unwrap_or(merge_base);
    Ok(Scope { head, branch, base })
}

pub(crate) fn serve(scope: &Scope) -> Option<Served> {
    let range = format!("{}..{}", scope.base, scope.head);
    let diff = git_bytes(&["diff", &range])?;
    let names = String::from_utf8(git_bytes(&["diff", "--name-only", &range])?).ok()?;
    let sha = format!("{:x}", Sha256::digest(&diff));
    let files = names.lines().map(str::to_string).collect();
    Some(Served { diff, sha, files })
}

/// The refusal text when the receipt does not vouch for this session, head
/// and diff, or none when it does.
pub(crate) fn check(
    receipt: Option<&Receipt>,
    session: &str,
    scope: &Scope,
    fresh_sha: Option<&str>,
    claim: &Claim<'_>,
) -> Option<String> {
    let why = match (fresh_sha, receipt) {
        (None, _) => "git gave no answer for the scoped diff, so no receipt can be checked",
        (Some(_), None) => "this session has no receipt for this head",
        (Some(_), Some(r)) if r.session != session => "the receipt belongs to another session",
        (Some(_), Some(r)) if r.head != scope.head => "the receipt is for an older head",
        (Some(_), Some(r)) if claim.head != r.head => {
            "the emit's head_sha is missing or is not the receipt's head"
        }
        (Some(_), Some(r)) if claim.branch != r.branch => {
            "the emit's branch is missing or is not the receipt's branch"
        }
        (Some(_), Some(r)) if r.base != scope.base => "the scope moved since the receipt",
        (Some(sha), Some(r)) if r.diff_sha256 != sha => "the diff changed since the receipt",
        _ => return None,
    };
    Some(format!("a clean code-review pass needs a receipt from `fno do review read` for this session and head, and {why}. Run `fno do review read`, read all of its output, then emit."))
}

/// The emit's check: refusal text, or none when the receipt holds.
pub(crate) fn emit_refusal(session: &str, claim: &Claim<'_>) -> Option<String> {
    let scope = match scope() {
        Ok(scope) => scope,
        Err(error) => {
            return Some(format!("a clean code-review pass needs a receipt from `fno do review read`, and {error}. Run it again once git and the journals answer."));
        }
    };
    let fresh = serve(&scope).map(|served| served.sha);
    let receipt = receipts_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join(receipt_file(&scope.head, session))).ok())
        .and_then(|text| serde_json::from_str::<Receipt>(&text).ok());
    check(receipt.as_ref(), session, &scope, fresh.as_deref(), claim)
}

fn attester_session() -> Option<String> {
    let mut command = Command::new(crate::digest_overlay::fno_agent_bin());
    command.args(["attest", "event-context"]);
    let out =
        crate::config_doctor::bounded_output(&mut command, None, Duration::from_secs(60)).ok()?;
    if !out.status.success() {
        return None;
    }
    let id: Identity = serde_json::from_slice(&out.stdout).ok()?;
    (id.attester_conflict.is_none() && !id.attester_session_id.is_empty())
        .then_some(id.attester_session_id)
}

fn write_receipt(receipt: &Receipt) -> bool {
    let Some(dir) = receipts_dir() else {
        return false;
    };
    let Ok(text) = serde_json::to_string(receipt) else {
        return false;
    };
    std::fs::create_dir_all(&dir).is_ok()
        && std::fs::write(
            dir.join(receipt_file(&receipt.head, &receipt.session)),
            text,
        )
        .is_ok()
}

const HELP: &str = "Usage: fno do review read

  Print the whole diff this review round must read, then record a receipt for
  this session and head. The scope comes from git and the attestation
  journals: a later round starts at the newest attested head that has a
  receipt, else at the merge base. The command takes no flags.

  A clean code-review attestation needs this receipt. The receipt proves the
  diff was served to this session. It does not prove it was read.

Exit codes: 0 served and recorded, 2 bad usage, 3 git, the journals or the
session identity gave no answer.
";

pub fn run(args: &[OsString]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    if !args.is_empty() {
        eprintln!("error: this command takes no arguments; the scope comes from git");
        return 2;
    }
    let Some(session) = attester_session() else {
        eprintln!("error: cannot resolve this session's identity, so no receipt can be recorded");
        return 3;
    };
    let scope = match scope() {
        Ok(scope) => scope,
        Err(error) => {
            eprintln!("error: {error}");
            return 3;
        }
    };
    let Some(served) = serve(&scope) else {
        eprintln!("error: git gave no answer for the scoped diff");
        return 3;
    };
    let mut out = std::io::stdout().lock();
    if out
        .write_all(&served.diff)
        .and_then(|()| out.flush())
        .is_err()
    {
        eprintln!("error: could not write the diff");
        return 3;
    }
    let receipt = Receipt {
        session,
        head: scope.head,
        branch: scope.branch,
        base: scope.base,
        files: served.files,
        diff_sha256: served.sha,
    };
    if !write_receipt(&receipt) {
        eprintln!("error: the diff was served but the receipt could not be recorded");
        return 3;
    }
    eprintln!(
        "receipt: head {} base {} files {} sha256 {}",
        receipt.head,
        receipt.base,
        receipt.files.len(),
        receipt.diff_sha256
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            head: "h2".into(),
            branch: "feature/x".into(),
            base: "b".into(),
        }
    }

    fn receipt() -> Receipt {
        Receipt {
            session: "s1".into(),
            head: "h2".into(),
            branch: "feature/x".into(),
            base: "b".into(),
            files: vec!["a.rs".into()],
            diff_sha256: "sha".into(),
        }
    }

    const CLAIM: Claim<'static> = Claim {
        head: "h2",
        branch: "feature/x",
    };

    fn refusal(r: Option<Receipt>, session: &str, scope: &Scope, fresh: Option<&str>) -> String {
        check(r.as_ref(), session, scope, fresh, &CLAIM).unwrap_or_default()
    }

    #[test]
    fn a_matching_receipt_passes() {
        assert!(check(Some(&receipt()), "s1", &scope(), Some("sha"), &CLAIM).is_none());
    }

    #[test]
    fn no_receipt_is_refused() {
        assert!(refusal(None, "s1", &scope(), Some("sha")).contains("no receipt for this head"));
    }

    #[test]
    fn another_sessions_receipt_is_refused() {
        let why = refusal(Some(receipt()), "s2", &scope(), Some("sha"));
        assert!(why.contains("another session"));
    }

    #[test]
    fn an_older_heads_receipt_is_refused() {
        let mut older = receipt();
        older.head = "h1".into();
        assert!(refusal(Some(older), "s1", &scope(), Some("sha")).contains("older head"));
    }

    #[test]
    fn a_changed_diff_is_refused() {
        let why = refusal(Some(receipt()), "s1", &scope(), Some("other"));
        assert!(why.contains("diff changed"));
    }

    #[test]
    fn no_git_answer_is_refused_even_with_a_receipt() {
        let why = refusal(Some(receipt()), "s1", &scope(), None);
        assert!(why.contains("no answer"));
    }

    #[test]
    fn an_emit_naming_another_head_or_branch_is_refused() {
        let other_head = Claim {
            head: "h9",
            branch: "feature/x",
        };
        let why = check(Some(&receipt()), "s1", &scope(), Some("sha"), &other_head);
        assert!(why.unwrap_or_default().contains("head_sha"));
        let no_branch = Claim {
            head: "h2",
            branch: "",
        };
        let why = check(Some(&receipt()), "s1", &scope(), Some("sha"), &no_branch);
        assert!(why.unwrap_or_default().contains("branch"));
    }

    #[test]
    fn receipt_file_names_carry_head_and_a_safe_session() {
        assert_eq!(receipt_file("h", "a/b c"), "h-a_b_c.json");
    }
}
