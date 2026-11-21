//! `visual shot-save`: copy a worker's shots into the vault mux dir under the
//! head they show. A review board entry cites artifact assets, so the shots a
//! round posted lived only behind the artifact door and the next round
//! regenerated an old head's before pair with a second app launch that can
//! differ from what the user approved. The door makes the disk copy one
//! command, keyed by node and head, so a later round reads its before pair
//! from the vault and never regenerates an old head.
//!
//! The saved name is `{node}-{head12}-{view}-{role}-{theme}.png`. The pairing
//! in `visual_screen` strips the head token, so a base-head before shot pairs
//! a PR-head after shot and the after shot whose head matches the critique
//! wins. Same key and same bytes is idempotent; same key and different bytes
//! refuses, because a shot is the evidence of one head.

use std::path::{Path, PathBuf};

/// The role tokens a shot name wears: `before`/`main` is the main side,
/// `after` is the PR side.
const ROLES: [&str; 3] = ["before", "main", "after"];

/// One parsed shot stem.
struct ParsedShot<'a> {
    view: String,
    role: &'a str,
    theme: &'a str,
}

/// Read the head as `--head=<sha>`, else `git rev-parse HEAD` in the cwd.
/// A bare `--head` refuses (exit 2): a positional sha would eat a png path.
fn resolve_head(rest: &[String]) -> Result<String, i32> {
    for a in rest {
        if let Some(h) = a.strip_prefix("--head=") {
            return Ok(h.to_string());
        }
    }
    if rest.iter().any(|a| a == "--head") {
        return Err(2);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "HEAD"]).current_dir(&cwd);
    match crate::bounded_cmd::output_with_timeout(cmd, 10) {
        Some(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => Err(1),
    }
}

/// Why a shot stem cannot be saved under the head-keyed name.
#[derive(Debug)]
enum NameError {
    NoTheme(String),
    NoRole(String),
    NoView(String),
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shape = "<node>-<view>-<role>-<theme>.png or <view>-<role>-<theme>.png";
        match self {
            Self::NoTheme(stem) => write!(
                f,
                "no dark/light theme token at the end of `{stem}`; expected {shape}"
            ),
            Self::NoRole(stem) => write!(
                f,
                "no before/main/after role token beside the theme in `{stem}`; expected {shape}"
            ),
            Self::NoView(stem) => write!(
                f,
                "no view token in `{stem}`; expected {shape} (a shot names the view it shows)"
            ),
        }
    }
}

/// Parse a stem of shape `<node>?-<view>-<role>-<theme>`: the role sits
/// directly before the theme, the theme is the last token, head tokens are
/// ignored, and a leading run of the node's own tokens (a legacy vault name)
/// drops off.
fn parse_shot_name<'a>(stem: &'a str, node_tokens: &[&str]) -> Result<ParsedShot<'a>, NameError> {
    let mut parts: Vec<&str> = stem
        .split('-')
        .filter(|p| !crate::visual_screen::is_head_token(p))
        .collect();
    let theme = match parts.last() {
        Some(&"dark") | Some(&"light") => parts.pop().unwrap(),
        _ => return Err(NameError::NoTheme(stem.to_string())),
    };
    let role = match parts.last() {
        Some(&r) if ROLES.contains(&r) => r,
        _ => return Err(NameError::NoRole(stem.to_string())),
    };
    parts.pop();
    let mut view = parts;
    // A legacy vault name starts with the node's own tokens; the save door
    // re-inserts them, so a leading node run drops off either way.
    if view.starts_with(node_tokens) {
        let cut = node_tokens.len();
        view.drain(..cut);
    }
    if view.is_empty() {
        return Err(NameError::NoView(stem.to_string()));
    }
    Ok(ParsedShot {
        view: view.join("-"),
        role,
        theme,
    })
}

/// `fno-agent visual shot-save <node> <png...> [--head=<sha>]`: save each png
/// into the vault mux dir under the head it shows.
pub(crate) fn run_shot_save(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("shot-save") || args.len() < 3 {
        eprintln!("usage: fno-agent visual shot-save <node> <png...> [--head=<sha>]");
        return 2;
    }
    let node = &args[1];
    let rest = &args[2..];
    let head = match resolve_head(rest) {
        Ok(h) => h,
        Err(code) => {
            if code == 2 {
                eprintln!("shot-save: pass the head as --head=<sha>, not a bare --head");
            } else {
                eprintln!("shot-save: cannot read HEAD; pass --head=<sha>");
            }
            return code;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = crate::paths::canonical_repo_root(&cwd).unwrap_or(cwd);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
    let node_tokens: Vec<&str> = node.split('-').collect();
    let head12 = crate::visual_screen::head12(&head);
    let mut exit = 0;
    for png in rest.iter().filter(|a| !a.starts_with("--")) {
        let src = Path::new(png);
        let bytes = match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("shot-save: cannot read {png}: {e}");
                exit = 1;
                continue;
            }
        };
        let stem = match src.file_stem() {
            Some(s) => s.to_string_lossy().to_string(),
            None => {
                eprintln!("shot-save: cannot name {png}: the path carries no file stem");
                exit = 1;
                continue;
            }
        };
        let parsed = match parse_shot_name(&stem, &node_tokens) {
            Ok(p) => p,
            Err(why) => {
                eprintln!("shot-save: cannot name {png}: {why}");
                exit = 1;
                continue;
            }
        };
        let dest = dir.join(format!(
            "{node}-{head12}-{view}-{role}-{theme}.png",
            view = parsed.view,
            role = parsed.role,
            theme = parsed.theme,
        ));
        if dest.exists() {
            let same = std::fs::read(&dest).is_ok_and(|old| old == bytes);
            if same {
                println!("already saved: {}", dest.display());
                continue;
            }
            eprintln!(
                "shot-save: refusing to overwrite {png} onto {}: bytes differ; a shot is \
                 the evidence of one head, so the same key with different bytes needs a \
                 human decision",
                dest.display()
            );
            exit = 1;
            continue;
        }
        match std::fs::copy(src, &dest) {
            Ok(_) => println!("saved: {}", dest.display()),
            Err(e) => {
                eprintln!("shot-save: cannot write {}: {e}", dest.display());
                exit = 1;
            }
        }
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_the_short_and_vault_shapes() {
        let short = parse_shot_name("chat-after-dark", &[]).expect("the short shape parses");
        assert_eq!(short.view, "chat");
        assert_eq!(short.role, "after");
        assert_eq!(short.theme, "dark");
        let vault = parse_shot_name("x-bed8-chat-before-dark", &["x", "bed8"])
            .expect("the vault shape parses");
        assert_eq!(vault.view, "chat");
        assert_eq!(vault.role, "before");
        // A head token in the name is ignored by the parse.
        let keyed = parse_shot_name("x-1-313522b78d12-chat-after-light", &["x", "1"])
            .expect("a head-keyed name parses");
        assert_eq!(keyed.view, "chat");
        assert_eq!(keyed.theme, "light");
    }

    #[test]
    fn parse_refuses_the_junk_shapes() {
        // No view token.
        assert!(parse_shot_name("before-dark", &[]).is_err());
        // No role token beside the theme.
        assert!(parse_shot_name("chat-dark", &[]).is_err());
        // No theme token at the end.
        assert!(parse_shot_name("chat-after", &[]).is_err());
    }
}
