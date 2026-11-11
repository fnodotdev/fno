//! `fno config plugins install <url>`: a pack from a git URL, behind one
//! consent boundary. No step before consent lets repo content run.
//!
//! 1. Stage: a shallow clone with hooks pointed at /dev/null and no
//!    submodules, into a dotdir under the role root the plugin layer walk
//!    never reads.
//! 2. Measure: the manifest parses, verify passes on the staged tree, and a
//!    content digest covers every file (path and bytes, sorted). A symlink
//!    or an agent-facing component refuses here.
//! 3. Show: the consent card names the URL, commit, digest, every argv a mod
//!    runs or reads, and every declared effect.
//! 4. Consent: an attended terminal answers yes, or an authorized principal
//!    approves the `local.plugin-install` request bound to the digest. An
//!    unattended run files that request, prints the approve line, exits 2.
//! 5. Commit: the staged tree moves into place and the registry records the
//!    source. A mod that runs code starts off; `launch_check` re-hashes the
//!    pack before any launch, so an edited tree needs a fresh consent.

use super::manifest::PackManifest;
use super::registry::{Source, Store};
use super::ui::Render;
use super::verify::{load_manifest, verify_manifest};
use crate::gate::effect_gate::{self, EffectRequest};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const EFFECT_CLASS: &str = "local.plugin-install";

/// A clone or a git read past this bound is killed and reads as a fetch failure.
const GIT_SECONDS: u64 = 300;

/// Components that reach agents rather than the UI install from a local
/// path only.
const AGENT_FACING: [&str; 6] = [
    "roles",
    "skills",
    "agents",
    "adapters",
    "workflows",
    "evaluators",
];

#[derive(Debug)]
pub enum InstallError {
    /// The clone or a git read failed; carries git's own stderr line.
    Fetch(String),
    /// The pack is not installable as staged; carries what refused.
    Refused(String),
    /// No consent yet: the request digest an authorized principal approves.
    AwaitingConsent { request_digest: String },
    /// The registry or the filesystem refused a write.
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Fetch(m) => write!(f, "fetch failed: {m}"),
            InstallError::Refused(m) => write!(f, "refused: {m}"),
            InstallError::AwaitingConsent { request_digest } => write!(
                f,
                "consent required: run `fno inbox approvals decide {request_digest} --as <principal> --approve`, then run install again"
            ),
            InstallError::Io(m) => write!(f, "install failed: {m}"),
        }
    }
}

/// What the operator consents to.
pub struct Card {
    pub url: String,
    pub commit: String,
    pub content_digest: String,
    pub manifest: PackManifest,
}

impl Card {
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![
            format!("pack     {} {}", self.manifest.id, self.manifest.version),
            format!("url      {}", self.url),
            format!("commit   {}", self.commit),
            format!("digest   {}", self.content_digest),
        ];
        for m in &self.manifest.mods {
            let what = match m.render() {
                Some(Render::Run(argv)) => format!("RUNS CODE: {}", super::ui::shell_words(argv)),
                Some(Render::Feed(feed)) => {
                    format!("reads: {}", super::ui::shell_words(&feed.argv))
                }
                Some(Render::Native(view)) => format!("native view: {view}"),
                None => "no render path".to_string(),
            };
            out.push(format!("mod      {}  {what}", m.id));
        }
        for l in &self.manifest.layouts {
            out.push(format!("layout   {}", l.id));
        }
        for e in &self.manifest.permissions {
            out.push(format!("effect   {} -> {}", e.effect_class, e.destination));
        }
        out.push("not reviewed by fno".to_string());
        out
    }
}

/// Where URL installs live: a dotdir under the role root.
pub fn packs_dir(root: &Path) -> PathBuf {
    root.join(".packs")
}

/// sha256 over every file under `dir` except `.git`, in sorted path order:
/// each file contributes its relative path, a NUL, its length, a NUL, and its
/// bytes. A symlink refuses: it could point outside the pack.
pub fn content_digest(dir: &Path) -> Result<String, InstallError> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for rel in &files {
        let bytes =
            std::fs::read(dir.join(rel)).map_err(|e| InstallError::Io(format!("{rel}: {e}")))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(bytes.len().to_string().as_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), InstallError> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| InstallError::Io(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| InstallError::Io(e.to_string()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| InstallError::Io(e.to_string()))?;
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if rel == ".git" {
            continue;
        }
        if kind.is_symlink() {
            return Err(InstallError::Refused(format!("{rel} is a symlink")));
        }
        if kind.is_dir() {
            walk(root, &path, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

fn git(args: &[&str]) -> Result<String, InstallError> {
    let mut cmd = Command::new("git");
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0");
    let output = crate::bounded_cmd::output_with_timeout_result(cmd, GIT_SECONDS)
        .map_err(|e| InstallError::Fetch(format!("git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("git failed");
        return Err(InstallError::Fetch(line.trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Steps 1 to 3: stage and measure. The staged dir is the caller's to keep
/// or remove.
pub fn stage(url: &str, root: &Path) -> Result<(PathBuf, Card), InstallError> {
    let staging = packs_dir(root).join(format!(".staging-{}", super::random_hex(6)));
    std::fs::create_dir_all(packs_dir(root)).map_err(|e| InstallError::Io(e.to_string()))?;
    let target = staging.to_string_lossy().into_owned();
    let measured = (|| -> Result<Card, InstallError> {
        git(&[
            "-c",
            "core.hooksPath=/dev/null",
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--no-recurse-submodules",
            "--",
            url,
            &target,
        ])?;
        let commit = git(&["-C", &target, "rev-parse", "HEAD"])?;
        let manifest = load_manifest(&staging).map_err(|c| {
            InstallError::Refused(c.detail.unwrap_or_else(|| "manifest did not load".into()))
        })?;
        let raw: Vec<&str> = AGENT_FACING
            .iter()
            .copied()
            .filter(|field| declares(&manifest, field))
            .collect();
        if !raw.is_empty() {
            return Err(InstallError::Refused(format!(
                "a URL install carries mods and layouts only; this pack declares {} (install those from a local path)",
                raw.join(", ")
            )));
        }
        let report = verify_manifest(&manifest, &Default::default(), &staging);
        if let Some(bad) = report.conditions.iter().find(|c| !c.ok()) {
            return Err(InstallError::Refused(format!(
                "verify {} {}: {}",
                bad.name,
                bad.result,
                bad.detail.clone().unwrap_or_default()
            )));
        }
        let content_digest = content_digest(&staging)?;
        Ok(Card {
            url: url.to_string(),
            commit,
            content_digest,
            manifest,
        })
    })();
    match measured {
        Ok(card) => Ok((staging, card)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

fn declares(manifest: &PackManifest, field: &str) -> bool {
    match field {
        "roles" => !manifest.roles.is_empty(),
        "skills" => !manifest.skills.is_empty(),
        "agents" => !manifest.agents.is_empty(),
        "adapters" => !manifest.adapters.is_empty(),
        "workflows" => !manifest.workflows.is_empty(),
        _ => !manifest.evaluators.is_empty(),
    }
}

/// Step 5: move the staged tree into place and record the source.
pub fn commit(
    root: &Path,
    staging: &Path,
    card: &Card,
    consented_by: &str,
) -> Result<PathBuf, InstallError> {
    let io = |e: std::io::Error| InstallError::Io(e.to_string());
    let store = Store::at_root(root);
    let _held = store.lock().map_err(io)?;
    let mut registry = store.load().map_err(|e| InstallError::Io(e.0))?;
    // An outside repo never takes over an id installed from somewhere else:
    // that would serve its tree under a first-party or local pack's name.
    if let Some(installed) = registry.pack_by_id(&card.manifest.id) {
        let from = installed.source.as_ref().map(|s| s.url.as_str());
        if from != Some(card.url.as_str()) {
            return Err(InstallError::Refused(format!(
                "pack id {} is already installed from {}; a URL pack cannot replace it",
                card.manifest.id,
                from.unwrap_or("a local path")
            )));
        }
    }
    let dest = packs_dir(root).join(&card.manifest.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(io)?;
    }
    std::fs::rename(staging, &dest).map_err(io)?;
    registry.install(&card.manifest, &dest.join("plugin.yaml"));
    if let Some(pack) = registry.pack_by_id_mut(&card.manifest.id) {
        pack.source = Some(Source {
            url: card.url.clone(),
            commit: card.commit.clone(),
            content_digest: card.content_digest.clone(),
            consented_by: consented_by.to_string(),
            consented_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        });
        // A new commit is a new consent: switches reset to their defaults.
        pack.toggles.clear();
    }
    store.save(&registry).map_err(InstallError::Io)?;
    Ok(dest)
}

/// Before the host launches a code mod: the pack on disk still hashes to
/// the digest consent recorded. Packs from a local path carry no source and
/// pass; the operator owns that tree.
pub fn launch_check(root: &Path, pack_id: &str) -> Result<(), InstallError> {
    let registry = Store::at_root(root)
        .load()
        .map_err(|e| InstallError::Io(e.0))?;
    let pack = registry
        .pack_by_id(pack_id)
        .ok_or_else(|| InstallError::Refused(format!("no installed pack {pack_id}")))?;
    let Some(source) = &pack.source else {
        return Ok(());
    };
    let dir = Path::new(&pack.manifest_path)
        .parent()
        .unwrap_or(Path::new(""));
    let now = content_digest(dir)?;
    if now != source.content_digest {
        return Err(InstallError::Refused(format!(
            "{pack_id} changed on disk since consent (digest {} now {}); reinstall to consent again",
            &source.content_digest[..12],
            &now[..12]
        )));
    }
    Ok(())
}

/// Step 4 through the approvals door: an approved request bound to this
/// digest consents; otherwise file one and return its digest.
fn approval(card: &Card) -> Result<Option<String>, InstallError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let db = effect_gate::default_db_path(&cwd).map_err(InstallError::Io)?;
    let conn = effect_gate::open_db(&db).map_err(InstallError::Io)?;
    let mut statement = conn
        .prepare("SELECT request_digest FROM requests WHERE action_digest = ? AND effect_class = ?")
        .map_err(|e| InstallError::Io(e.to_string()))?;
    let filed: Vec<String> = statement
        .query_map([card.content_digest.as_str(), EFFECT_CLASS], |row| {
            row.get(0)
        })
        .map_err(|e| InstallError::Io(e.to_string()))?
        .filter_map(Result::ok)
        .collect();
    let now = chrono::Utc::now();
    for digest in &filed {
        if effect_gate::verdict(&conn, &cwd, digest, now).unwrap_or(false) {
            return Ok(Some(format!("approval:{}", &digest[..12])));
        }
    }
    let request = EffectRequest {
        request_id: super::random_hex(8),
        principal_id: "fno-config-plugins".into(),
        work_order_id: format!("plugin-install:{}", card.manifest.id),
        attempt_id: card.commit.clone(),
        effect_id: card.manifest.id.clone(),
        effect_class: EFFECT_CLASS.into(),
        destination: card.url.clone(),
        action_digest: card.content_digest.clone(),
        created_at: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        expires_at: (now + chrono::Duration::hours(24))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    };
    let request_digest =
        effect_gate::submit(&conn, None, &request).map_err(InstallError::Refused)?;
    Err(InstallError::AwaitingConsent { request_digest })
}

fn attended_yes(runs_code: bool) -> bool {
    if !crate::backlog::law_match::attended_terminal() {
        return false;
    }
    if runs_code {
        eprintln!("this pack runs code on your machine. Its command mods stay off until you switch each one on.");
    }
    eprint!("install this pack? [y/N] ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("y")
}

/// The whole flow with the consent source injected, so tests drive step 4.
pub fn install_with(
    url: &str,
    root: &Path,
    consent: impl FnOnce(&Card) -> Result<Option<String>, InstallError>,
) -> Result<(Card, PathBuf), InstallError> {
    let (staging, card) = stage(url, root)?;
    let decided = consent(&card);
    let consented_by = match decided {
        Ok(Some(who)) => who,
        Ok(None) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Refused("the operator declined".into()));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    match commit(root, &staging, &card, &consented_by) {
        Ok(dest) => Ok((card, dest)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

pub fn run(json: bool, root: &Path, url: &str) -> i32 {
    let outcome = install_with(url, root, |card| {
        for line in card.lines() {
            eprintln!("{line}");
        }
        if crate::backlog::law_match::attended_terminal() {
            let user = std::env::var("USER").unwrap_or_else(|_| "operator".into());
            let runs_code = card.manifest.mods.iter().any(|m| m.runs_code());
            return Ok(attended_yes(runs_code).then(|| format!("attended-terminal:{user}")));
        }
        approval(card)
    });
    match outcome {
        Ok((card, dest)) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "pack_id": card.manifest.id,
                        "commit": card.commit,
                        "content_digest": card.content_digest,
                        "path": dest,
                        "version": 1,
                    })
                );
            } else {
                println!("installed {} at {}", card.manifest.id, card.commit);
                if card.manifest.mods.iter().any(|m| m.runs_code()) {
                    println!(
                        "  mods that run code start off: fno config plugins enable {}/<mod>",
                        card.manifest.id
                    );
                }
            }
            0
        }
        Err(e) => {
            let code = match e {
                InstallError::AwaitingConsent { .. } | InstallError::Refused(_) => 2,
                _ => 1,
            };
            if json {
                let digest = match &e {
                    InstallError::AwaitingConsent { request_digest } => json!(request_digest),
                    _ => json!(null),
                };
                println!(
                    "{}",
                    json!({"error": e.to_string(), "request_digest": digest, "version": 1})
                );
            } else {
                eprintln!("{e}");
            }
            code
        }
    }
}
