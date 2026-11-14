//! The native `fno do docs` family: tie a doc page to its release for the
//! worker writing it (law D2707's enforcement side). Three actions:
//!
//! - `bind` stamps the front matter that fno-web's release gate judges
//!   (`since:`, `nav:`, `pr:`), turns an edit to a live page into its
//!   `<page>.next.mdx` twin (`since:` + `replaces:`), then reruns
//!   `scripts/docs-release-gate.mjs --apply` so `.mintignore` and the
//!   docs.json nav regenerate. The script is the sole writer of the
//!   generated block; bind writes page front matter only.
//! - `surface` prints the changed files that touch a user surface (the
//!   merge gate's own matcher, so the docs step and the gate cannot
//!   disagree about what is user-facing).
//! - `gate` runs the fno-web release gate's check or `--apply`.
//!
//! The front door (crates/fno) claims `do docs` lexically and execs this
//! binary with the argv intact (the `do pr` handover); the arm in
//! bin/client.rs strips `do docs` and lands here.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fno-web checkout refuses to answer without its gate script; every
/// resolution path must land on one that carries it.
const GATE_SCRIPT: &str = "scripts/docs-release-gate.mjs";
const DOCS_DIR: &str = "fno-sh-docs";
const LIVE_RELEASES: &str = "scripts/docs-live-releases.json";
/// A release tag: vX.Y.Z for the CLI, or `<product>-vX.Y.Z` for a product
/// on its own version line (fno-app-v0.1.0). Mirrors the gate script's TAG.
const TWIN_SUFFIX: &str = ".next";

/// One exit-code contract for the family: usage errors and refusals are 2,
/// unreadable state is 4, a missing `node` is 127 (the pr family's
/// binary-missing code), and the gate script's own exit passes through.
#[derive(Debug)]
enum DocsError {
    /// Bad invocation: unknown action, missing argument, malformed value.
    Usage(String),
    /// The world refused the action: no fno-web checkout, twin exists,
    /// no derivable release tag.
    Refused(String),
    /// State the action needed could not be read: page absent, JSON bad.
    Read(String),
}

impl DocsError {
    fn exit(&self) -> i32 {
        match self {
            DocsError::Usage(_) | DocsError::Refused(_) => 2,
            DocsError::Read(_) => 4,
        }
    }
}

impl std::fmt::Display for DocsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocsError::Usage(m) => write!(f, "{m}"),
            DocsError::Refused(m) => write!(f, "{m}"),
            DocsError::Read(m) => write!(f, "{m}"),
        }
    }
}

/// The typed shape of `scripts/docs-live-releases.json` (parsed once at
/// the boundary; nothing past it reads fields by string key).
#[derive(Deserialize)]
struct LiveReleases {
    #[serde(default)]
    #[allow(dead_code)]
    source: String,
    #[serde(default)]
    tags: Vec<String>,
}

const HELP_ROWS: &[(&str, &str)] = &[
    ("bind <page> [--release T] [--nav G] [--pr N] [--web P]", "Stamp the front matter the fno-web release gate judges, then rerun the gate with --apply. A page whose since: names a live tag becomes <page>.next.mdx with since: and replaces: (the live page is never edited); a draft gets since: (explicit --release, else the newest since: among pages whose tag is not yet live, else a refusal naming --release), plus --nav/--pr when given. Exit 0 applied, 2 refusal, 4 page/live-list unreadable, 127 node missing."),
    ("surface [--diff-base B]", "Print the changed files (B...HEAD plus the dirty tree, default B=origin/main) that touch a user surface, through the merge gate's own matcher. Empty output means the diff is not user-facing. Always exits 0."),
    ("gate [--apply] [--web P]", "Run fno-web's scripts/docs-release-gate.mjs: check (exit 1 on drift) or --apply (move live twins, then pages into or out of the nav). The script's exit passes through; 127 when node is missing."),
];

fn print_help() {
    println!("Docs release binding (fno-web pages: bind, surface, gate)");
    println!();
    println!("Usage: fno do docs <action> [args]");
    println!();
    for (spelling, wording) in HELP_ROWS {
        println!("  {spelling}");
        println!("      {wording}");
        println!();
    }
}

fn spelling_help(sub: &str) -> Option<&'static str> {
    HELP_ROWS
        .iter()
        .find(|(name, _)| name.split_whitespace().next() == Some(sub))
        .map(|(_, wording)| *wording)
}

/// A release tag: optional product prefix of lowercase alnum groups joined
/// by single dashes, then `vX.Y.Z`. Hand-rolled to mirror the gate script's
/// TAG without a regex dependency.
pub fn is_release_tag(value: &str) -> bool {
    let Some((prefix, core)) = value.rsplit_once("-v") else {
        return core_is_tag(value);
    };
    if prefix.is_empty() || !prefix.split('-').all(valid_group) {
        return false;
    }
    core_is_tag(&format!("v{core}"))
}

fn valid_group(group: &str) -> bool {
    !group.is_empty()
        && group.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && group
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn core_is_tag(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('v') else {
        return false;
    };
    let mut parts = rest.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    [major, minor, patch]
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// The numeric core of a tag for newest-first ordering; non-tag values sort
/// last (None).
fn tag_order(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag.rsplit_once("-v").map_or(tag, |(_, c)| c);
    let rest = core.strip_prefix('v')?;
    let mut nums = Vec::new();
    for part in rest.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums.push(part.parse::<u64>().ok()?);
    }
    if nums.len() != 3 {
        return None;
    }
    Some((nums[0], nums[1], nums[2]))
}

/// Split `text` into (fields, body-after-front-matter). A page with no
/// front matter carries no fields and the whole text is the body.
fn front_fields(text: &str) -> (Vec<(String, String)>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (Vec::new(), text);
    };
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let block = &rest[..end];
    let fields = block
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    (fields, &rest[end + 4..])
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Rewrite `text` with `key: value` set in its front matter, replacing an
/// existing line in place or appending before the closing fence. A page
/// with no front matter gains one.
fn with_field(text: &str, key: &str, value: &str) -> String {
    if !text.starts_with("---\n") {
        return format!("---\n{key}: {value}\n---\n{text}");
    }
    let mut lines: Vec<String> = text
        .strip_prefix("---\n")
        .unwrap_or(text)
        .split_inclusive('\n')
        .map(str::to_string)
        .collect();
    let closing = lines.iter().position(|l| l.trim_end() == "---");
    let mut replaced = false;
    for line in lines.iter_mut() {
        if line.starts_with(&format!("{key}:")) {
            *line = format!("{key}: {value}\n");
            replaced = true;
            break;
        }
    }
    if !replaced {
        let at = closing.unwrap_or(lines.len());
        lines.insert(at, format!("{key}: {value}\n"));
    }
    let mut out = String::from("---\n");
    for line in lines {
        out.push_str(&line);
    }
    out
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn is_web_repo(path: &Path) -> bool {
    path.join(GATE_SCRIPT).is_file()
}

/// Resolve the fno-web checkout: `--web`, then `[paths] docs_web_dir` in
/// the config candidates, then `../fno-web` beside the repo root. Every
/// landing must carry the gate script, or the refusal names the flag.
fn web_root(cwd: &Path, flag: Option<&str>) -> Result<PathBuf, DocsError> {
    let mut tried: Vec<String> = Vec::new();
    if let Some(flag) = flag {
        let path = expand_home(flag);
        if is_web_repo(&path) {
            return Ok(path);
        }
        return Err(DocsError::Usage(format!(
            "fno do docs: --web {} carries no {GATE_SCRIPT}; name the fno-web checkout",
            path.display()
        )));
    }
    let configured = crate::spawn::agents_config::config_lookup(cwd, &["paths", "docs_web_dir"])
        .and_then(|v| v.as_str().map(str::to_string));
    if let Some(raw) = configured {
        let path = expand_home(&raw);
        if is_web_repo(&path) {
            return Ok(path);
        }
        tried.push(format!("paths.docs_web_dir = {}", path.display()));
    }
    let root = crate::paths::canonical_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let sibling = root.parent().map(|p| p.join("fno-web"));
    if let Some(sibling) = sibling {
        if is_web_repo(&sibling) {
            return Ok(sibling);
        }
        tried.push(format!("{} (sibling of the repo root)", sibling.display()));
    }
    Err(DocsError::Refused(format!(
        "fno do docs: no fno-web checkout found (tried {}); pass --web <path> or set [paths] docs_web_dir",
        if tried.is_empty() {
            "no candidates".to_string()
        } else {
            tried.join(", ")
        }
    )))
}

fn read_live_tags(web: &Path) -> Result<Vec<String>, DocsError> {
    let path = web.join(LIVE_RELEASES);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        DocsError::Read(format!("fno do docs: cannot read {}: {e}", path.display()))
    })?;
    let parsed: LiveReleases = serde_json::from_str(&text).map_err(|e| {
        DocsError::Read(format!(
            "fno do docs: {} is not the live-releases shape: {e}",
            path.display()
        ))
    })?;
    Ok(parsed.tags)
}

/// Every .mdx page under `docs`, depth-first, `images` dirs skipped (the
/// gate script's own walk).
fn mdx_pages(docs: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "images") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "mdx") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(docs, &mut out);
    out
}

/// The newest `since:` among pages whose tag is not yet live: the release
/// the tree is currently writing toward. None when every page is live, so
/// the caller refuses instead of guessing the next version.
fn derive_release(docs: &Path, live: &[String]) -> Option<String> {
    let mut newest: Option<(String, (u64, u64, u64))> = None;
    for page in mdx_pages(docs) {
        let Ok(text) = std::fs::read_to_string(&page) else {
            continue;
        };
        let (fields, _) = front_fields(&text);
        let Some(since) = field(&fields, "since") else {
            continue;
        };
        if live.iter().any(|t| t == since) {
            continue;
        }
        let Some(order) = tag_order(since) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(_, o)| order > *o) {
            newest = Some((since.to_string(), order));
        }
    }
    newest.map(|(tag, _)| tag)
}

/// Normalize a page argument to its docs-relative stem ("guides/x") and its
/// file path. Accepts `guides/x.mdx`, `fno-sh-docs/guides/x.mdx`, or an
/// absolute path inside the checkout.
fn page_paths(docs: &Path, raw: &str) -> Result<(String, PathBuf), DocsError> {
    let mut raw = raw.trim().to_string();
    for strip in ["/", "./"] {
        raw = raw.strip_prefix(strip).unwrap_or(&raw).to_string();
    }
    let prefix = format!("{DOCS_DIR}/");
    if let Some(stripped) = raw.strip_prefix(&prefix) {
        raw = stripped.to_string();
    }
    if raw.is_empty() || !raw.ends_with(".mdx") {
        return Err(DocsError::Usage(format!(
            "fno do docs bind: {raw} is not a .mdx page path relative to {DOCS_DIR}"
        )));
    }
    let path = if Path::new(&raw).is_absolute() {
        PathBuf::from(&raw)
    } else {
        docs.join(&raw)
    };
    let stem = raw.trim_end_matches(".mdx").to_string();
    Ok((stem, path))
}

/// Pull one `--flag value` / `--flag=value` off the tail; duplicate flags
/// are a usage error.
struct Flags {
    release: Option<String>,
    nav: Option<String>,
    pr: Option<String>,
    web: Option<String>,
    diff_base: Option<String>,
    apply: bool,
}

fn parse_flags(tail: &[String]) -> Result<Flags, DocsError> {
    let mut flags = Flags {
        release: None,
        nav: None,
        pr: None,
        web: None,
        diff_base: None,
        apply: false,
    };
    let mut i = 0;
    while i < tail.len() {
        let arg = &tail[i];
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        let mut taken: Option<String> = None;
        let mut advance = 1usize;
        if inline.is_none() {
            match name.as_str() {
                "--release" | "--nav" | "--pr" | "--web" | "--diff-base" => {
                    let value = tail.get(i + 1).ok_or_else(|| {
                        DocsError::Usage(format!("fno do docs: {name} needs a value"))
                    })?;
                    taken = Some(value.clone());
                    advance = 2;
                }
                _ => {}
            }
        } else {
            taken = inline.clone();
        }
        let store = |slot: &mut Option<String>| -> Result<(), DocsError> {
            if slot.is_some() {
                return Err(DocsError::Usage(format!("fno do docs: {name} given twice")));
            }
            if taken.is_none() {
                return Err(DocsError::Usage(format!(
                    "fno do docs: {name} needs a value"
                )));
            }
            *slot = taken.clone();
            Ok(())
        };
        match name.as_str() {
            "--release" => store(&mut flags.release)?,
            "--nav" => store(&mut flags.nav)?,
            "--pr" => store(&mut flags.pr)?,
            "--web" => store(&mut flags.web)?,
            "--diff-base" => store(&mut flags.diff_base)?,
            "--apply" => {
                flags.apply = true;
            }
            other => {
                return Err(DocsError::Usage(format!(
                    "fno do docs: unknown flag {other}"
                )))
            }
        }
        i += advance;
    }
    Ok(flags)
}

/// Bind one page: stamp or twin its front matter, then rerun the gate.
fn bind(tail: &[String]) -> Result<i32, DocsError> {
    let flags = parse_flags(tail)?;
    let Some(page_raw) = tail.first() else {
        return Err(DocsError::Usage(
            "fno do docs bind: name the page (fno do docs bind <page> --help for the flags)".into(),
        ));
    };
    let page_raw = page_raw.clone();
    if page_raw.starts_with("--") {
        return Err(DocsError::Usage(
            "fno do docs bind: name the page before the flags".into(),
        ));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let web = web_root(&cwd, flags.web.as_deref())?;
    let docs = web.join(DOCS_DIR);
    let (stem, page_path) = page_paths(&docs, &page_raw)?;
    let text = std::fs::read_to_string(&page_path).map_err(|e| {
        DocsError::Read(format!(
            "fno do docs bind: cannot read {}: {e}",
            page_path.display()
        ))
    })?;
    let live = read_live_tags(&web)?;
    let (fields, _) = front_fields(&text);
    let existing_since = field(&fields, "since").map(str::to_string);

    let is_live = existing_since
        .as_deref()
        .is_some_and(|s| live.iter().any(|t| t == s));
    let release = match flags.release.clone() {
        Some(tag) => {
            if !is_release_tag(&tag) {
                return Err(DocsError::Usage(format!(
                    "fno do docs bind: --release {tag} is not a vX.Y.Z or <product>-vX.Y.Z tag name"
                )));
            }
            tag
        }
        None => derive_release(&docs, &live).ok_or_else(|| {
            DocsError::Refused(
                "fno do docs bind: no unpublished page names a release to derive; pass --release <tag>".into(),
            )
        })?,
    };

    if is_live {
        let file_name = page_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let twin_name = file_name.replacen(".mdx", &format!("{TWIN_SUFFIX}.mdx"), 1);
        let twin_path = page_path.with_file_name(twin_name);
        if twin_path.exists() {
            return Err(DocsError::Refused(format!(
                "fno do docs bind: twin {} already exists; edit it instead",
                twin_path.display()
            )));
        }
        let mut twin = with_field(&text, "since", &release);
        twin = with_field(&twin, "replaces", &stem);
        if let Some(nav) = flags.nav.as_deref() {
            twin = with_field(&twin, "nav", nav);
        }
        if let Some(pr) = flags.pr.as_deref() {
            twin = with_field(&twin, "pr", pr);
        }
        std::fs::write(&twin_path, twin).map_err(|e| {
            DocsError::Read(format!(
                "fno do docs bind: cannot write {}: {e}",
                twin_path.display()
            ))
        })?;
        println!(
            "twin {} (since: {release}, replaces: {stem})",
            twin_path.display()
        );
    } else {
        let mut page = text.clone();
        if existing_since.is_none() || flags.release.is_some() {
            page = with_field(&page, "since", &release);
        }
        if let Some(nav) = flags.nav.as_deref() {
            page = with_field(&page, "nav", nav);
        }
        if let Some(pr) = flags.pr.as_deref() {
            if pr.chars().all(|c| c.is_ascii_digit()) {
                page = with_field(&page, "pr", pr);
            } else {
                return Err(DocsError::Usage(format!(
                    "fno do docs bind: --pr {pr} is not a PR number"
                )));
            }
        }
        if page != text {
            std::fs::write(&page_path, page).map_err(|e| {
                DocsError::Read(format!(
                    "fno do docs bind: cannot write {}: {e}",
                    page_path.display()
                ))
            })?;
            println!("stamped {} (since: {release})", page_path.display());
        } else {
            println!("{} already carries its release fields", page_path.display());
        }
    }
    run_gate(&web, true)
}

/// Run the fno-web release gate script; its output streams through, its
/// exit passes back.
fn run_gate(web: &Path, apply: bool) -> Result<i32, DocsError> {
    let script = web.join(GATE_SCRIPT);
    let mut command = Command::new("node");
    command.arg(&script);
    if apply {
        command.arg("--apply");
    }
    command.current_dir(web);
    let status = command.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DocsError::Usage(format!(
                "fno do docs: node is missing; the gate script needs it (install node, or run {} by hand)",
                script.display()
            ))
        } else {
            DocsError::Read(format!("fno do docs: cannot run {}: {e}", script.display()))
        }
    })?;
    Ok(status.code().unwrap_or(1))
}

/// Print the changed files that touch a user surface, through the merge
/// gate's own matcher, so the docs step and the gate cannot disagree.
fn surface(tail: &[String]) -> Result<i32, DocsError> {
    let flags = parse_flags(tail)?;
    let base = flags.diff_base.as_deref().unwrap_or("origin/main");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut files: Vec<String> = Vec::new();
    for spec in [format!("{base}...HEAD"), "HEAD".to_string()] {
        let out = Command::new("git")
            .args(["diff", "--name-only", &spec])
            .output()
            .map_err(|e| DocsError::Read(format!("fno do docs surface: git failed: {e}")))?;
        if !out.status.success() {
            return Err(DocsError::Read(format!(
                "fno do docs surface: git diff {} failed; is {} a ref here?",
                spec, base
            )));
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if !line.is_empty() && !files.iter().any(|f| f == line) {
                files.push(line.to_string());
            }
        }
    }
    let touched = crate::docs_gate::surface_files(&files, &cwd);
    for file in &touched {
        println!("{file}");
    }
    Ok(0)
}

fn gate(tail: &[String]) -> Result<i32, DocsError> {
    let flags = parse_flags(tail)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let web = web_root(&cwd, flags.web.as_deref())?;
    run_gate(&web, flags.apply)
}

/// The family door: `fno do docs <action> [args]`.
pub fn run(args: &[String]) -> i32 {
    let result = match args.first().map(String::as_str) {
        None | Some("--help" | "-h") => {
            print_help();
            return 0;
        }
        Some("bind") => bind(&args[1..]),
        Some("surface") => surface(&args[1..]),
        Some("gate") => gate(&args[1..]),
        Some(other) => Err(DocsError::Usage(format!(
            "fno do docs: unknown action {other}; bind, surface and gate are the actions"
        ))),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err}");
            err.exit()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tags_read_like_the_gate_script() {
        assert!(is_release_tag("v0.4.9"));
        assert!(is_release_tag("fno-app-v0.1.0"));
        assert!(is_release_tag("v10.20.30"));
        assert!(!is_release_tag("v0.4"));
        assert!(!is_release_tag("nightly"));
        assert!(!is_release_tag("v0.4.9rc1"));
        assert!(!is_release_tag("Bin-v0.1.0"));
        assert!(!is_release_tag("-v0.1.0"));
    }

    #[test]
    fn front_matter_fields_set_in_place_or_created() {
        let stamped = with_field("hello\n", "since", "v0.5.0");
        assert_eq!(stamped, "---\nsince: v0.5.0\n---\nhello\n");
        let replaced = with_field(&stamped, "since", "v0.6.0");
        assert!(replaced.starts_with("---\nsince: v0.6.0\n---\nhello\n"));
        let appended = with_field(&replaced, "replaces", "guides/x");
        assert!(appended.contains("since: v0.6.0\nreplaces: guides/x\n"));
        let quoted = with_field("hello\n", "nav", "Reference");
        assert_eq!(quoted, "---\nnav: Reference\n---\nhello\n");
    }

    #[test]
    fn derive_release_picks_the_newest_unpublished_tag() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join(DOCS_DIR);
        std::fs::create_dir_all(docs.join("guides")).unwrap();
        std::fs::write(docs.join("live.mdx"), "---\nsince: v0.4.8\n---\n").unwrap();
        std::fs::write(docs.join("guides/next.mdx"), "---\nsince: v0.4.9\n---\n").unwrap();
        std::fs::write(docs.join("guides/newer.mdx"), "---\nsince: v0.5.0\n---\n").unwrap();
        std::fs::create_dir_all(docs.join("images")).unwrap();
        std::fs::write(docs.join("images/skip.mdx"), "---\nsince: v9.9.9\n---\n").unwrap();
        let live = vec!["v0.4.8".to_string()];
        assert_eq!(derive_release(&docs, &live).as_deref(), Some("v0.5.0"));
        // Everything live: no derivation, the caller refuses.
        let all_live = vec![
            "v0.4.8".to_string(),
            "v0.4.9".to_string(),
            "v0.5.0".to_string(),
        ];
        assert_eq!(derive_release(&docs, &all_live), None);
    }

    #[test]
    fn page_paths_accept_the_documented_shapes() {
        let docs = Path::new("/web/fno-sh-docs");
        let (stem, path) = page_paths(docs, "guides/x.mdx").unwrap();
        assert_eq!(stem, "guides/x");
        assert_eq!(path, docs.join("guides/x.mdx"));
        let (stem, path) = page_paths(docs, "fno-sh-docs/guides/x.mdx").unwrap();
        assert_eq!(stem, "guides/x");
        assert_eq!(path, docs.join("guides/x.mdx"));
        assert!(page_paths(docs, "guides/x").is_err());
        assert!(page_paths(docs, "").is_err());
    }
}
