//! Self-update. The app reads the `fno-app-v*` release feed, stages a signed
//! build in the background, and offers a relaunch. Nothing here touches the
//! running bundle until the user clicks Restart.
//!
//! The staged build must pass four proofs before it is offered: the sha256
//! sidecar matches the tarball, `codesign --verify --deep --strict` passes,
//! the bundle's Team ID equals the running bundle's, and the staged binary
//! names the release version. The module fails closed on the Team ID: a
//! running bundle with no Team ID (ad hoc signed or unsigned) never checks
//! for updates and never installs one, and neither does a dev run outside a
//! `.app`.
//!
//! State lives under `~/Library/Application Support/fno-app/update/`: one
//! directory per staged version, and `pending-notes.json`, which carries a
//! release's notes across the relaunch so the new build shows them once.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const TAG_PREFIX: &str = "fno-app-v";
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The version this binary was built as.
pub const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// How long the app waits between feed reads while no update is staged.
pub const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);

/// Waits for the old process to exit, swaps the bundle (the old one comes
/// back if the swap fails), and opens whichever bundle is in place.
/// `$1` is the running bundle, `$2` the staged one, `$3` the old pid.
const RELAUNCH: &str = r#"while kill -0 "$3" 2>/dev/null; do sleep 0.2; done; rm -rf "$1.old"; if mv "$1" "$1.old"; then if mv "$2" "$1"; then rm -rf "$1.old"; else mv "$1.old" "$1"; fi; fi; open "$1""#;

/// Why an update step failed: one line, ready to show.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UpdateError(String);

impl From<String> for UpdateError {
    fn from(why: String) -> Self {
        Self(why)
    }
}

impl From<&str> for UpdateError {
    fn from(why: &str) -> Self {
        Self(why.to_string())
    }
}

/// A build staged on disk and proven, waiting for the user's Restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub version: String,
    pub notes: String,
    pub staged: PathBuf,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Deserialize)]
struct TagRef {
    #[serde(rename = "ref")]
    git_ref: String,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    version: String,
    notes: String,
}

fn version_key(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let key = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(key)
}

fn repo_slug() -> &'static str {
    REPOSITORY
        .strip_prefix("https://github.com/")
        .unwrap_or(REPOSITORY)
}

fn update_dir() -> Option<PathBuf> {
    Some(crate::app_state::state_path()?.parent()?.join("update"))
}

/// The `.app` the running binary lives in; None for a dev run.
fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// `Command::output` without the bare call the clippy gate bans: the child
/// gets no stdin, and both pipes are read to the end.
trait Capture {
    fn capture(&mut self) -> std::io::Result<std::process::Output>;
}

impl Capture for Command {
    fn capture(&mut self) -> std::io::Result<std::process::Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
            .wait_with_output()
    }
}

fn curl(url: &str, out: Option<&Path>) -> Result<Vec<u8>, UpdateError> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--max-time", "300"]);
    // Only the API wants the header: a release download redirects to a
    // storage host that refuses it.
    if url.starts_with("https://api.github.com/") {
        cmd.args(["-H", "Accept: application/vnd.github+json"]);
    }
    if let Some(path) = out {
        cmd.arg("-o").arg(path);
    }
    let o = cmd
        .arg(url)
        .stdin(Stdio::null())
        .capture()
        .map_err(|e| format!("curl did not run: {e}"))?;
    if !o.status.success() {
        return Err(format!("curl exited {} for {url}", o.status.code().unwrap_or(-1)).into());
    }
    Ok(o.stdout)
}

fn sha256_of(path: &Path) -> Result<String, UpdateError> {
    let o = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .capture()
        .map_err(|e| format!("shasum did not run: {e}"))?;
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| UpdateError::from("shasum printed no digest"))
}

/// The bundle's Team ID; None for an ad hoc or unsigned bundle.
fn team_id(app: &Path) -> Option<String> {
    let o = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(app)
        .capture()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stderr);
    text.lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(|t| t.trim().to_string())
        .filter(|t| t != "not set")
}

fn prune(root: &Path, keep: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let keeps = keep.is_some_and(|k| name == k || name.starts_with(&format!("{k}.")));
        if e.path().is_dir() && !keeps && !name.ends_with(".partial") {
            std::fs::remove_dir_all(e.path()).ok();
        }
    }
}

/// The newest stable `fno-app-v*` release past `running`. The ref listing
/// is filtered by the tag prefix on the server, so releases of the other
/// products in this repository never crowd it out of the page.
fn newest_stable(running: &str) -> Result<Option<(String, String)>, UpdateError> {
    let api = format!("https://api.github.com/repos/{}", repo_slug());
    let refs = curl(&format!("{api}/git/matching-refs/tags/{TAG_PREFIX}"), None)?;
    let refs: Vec<TagRef> =
        serde_json::from_slice(&refs).map_err(|e| format!("tag listing: {e}"))?;
    let have = version_key(running).ok_or("the running version is not x.y.z")?;
    let mut newer: Vec<((u64, u64, u64), String)> = refs
        .iter()
        .filter_map(|r| {
            let v = r
                .git_ref
                .strip_prefix("refs/tags/")?
                .strip_prefix(TAG_PREFIX)?;
            let key = version_key(v)?;
            (key > have).then(|| (key, v.to_string()))
        })
        .collect();
    newer.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, version) in newer.into_iter().take(5) {
        let body = curl(&format!("{api}/releases/tags/{TAG_PREFIX}{version}"), None);
        let Ok(body) = body else { continue };
        let Ok(release) = serde_json::from_slice::<Release>(&body) else {
            continue;
        };
        if !release.draft && !release.prerelease {
            return Ok(Some((version, release.body.unwrap_or_default())));
        }
    }
    Ok(None)
}

/// Whether this process runs from an app bundle: a dev run never updates.
pub fn bundled() -> bool {
    running_bundle().is_some()
}

/// Read the feed and, when a newer release exists, stage and prove it.
/// Blocking: call it off the UI thread. `Ok(None)` means up to date.
pub fn check(running: &str) -> Result<Option<Ready>, UpdateError> {
    let bundle = running_bundle().ok_or("not running from an app bundle")?;
    if team_id(&bundle).is_none() {
        return Ok(None);
    }
    let root = update_dir().ok_or("no home directory")?;
    let Some((version, notes)) = newest_stable(running)? else {
        prune(&root, None);
        return Ok(None);
    };
    prune(&root, Some(&version));
    let dir = root.join(&version);
    let staged = dir.join("fno-app.app");
    if staged.is_dir() {
        return Ok(Some(Ready {
            version,
            notes,
            staged,
        }));
    }
    let work = root.join(format!("{version}.{}.partial", std::process::id()));
    std::fs::remove_dir_all(&work).ok();
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let proven = stage(&bundle, &version, &work);
    if let Err(e) = proven {
        std::fs::remove_dir_all(&work).ok();
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&work, &dir) {
        // Another instance staged the same version first: its copy stands.
        std::fs::remove_dir_all(&work).ok();
        if !staged.is_dir() {
            return Err(format!("{}: {e}", dir.display()).into());
        }
    }
    Ok(Some(Ready {
        version,
        notes,
        staged,
    }))
}

/// Download into `work`, unpack, and run the four proofs.
fn stage(bundle: &Path, version: &str, work: &Path) -> Result<(), UpdateError> {
    let base = format!(
        "https://github.com/{}/releases/download/{TAG_PREFIX}{version}",
        repo_slug()
    );
    let tarball = work.join("fno-app-macos.tar.gz");
    curl(&format!("{base}/fno-app-macos.tar.gz"), Some(&tarball))?;
    let sidecar = curl(&format!("{base}/fno-app-macos.tar.gz.sha256"), None)?;
    let want = String::from_utf8_lossy(&sidecar)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or("the sha256 sidecar is empty")?;
    let got = sha256_of(&tarball)?;
    if got != want {
        return Err(format!("sha256 mismatch: want {want}, got {got}").into());
    }
    let untar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(work)
        .status()
        .map_err(|e| format!("tar did not run: {e}"))?;
    if !untar.success() {
        return Err("the tarball did not unpack".into());
    }
    let app = work.join("fno-app.app");
    let signed = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .status()
        .map_err(|e| format!("codesign did not run: {e}"))?;
    if !signed.success() {
        return Err("the staged bundle failed codesign verification".into());
    }
    let mine = team_id(bundle).ok_or("the running app has no Team ID")?;
    if team_id(&app).as_deref() != Some(mine.as_str()) {
        return Err(format!("the staged bundle is not signed by team {mine}").into());
    }
    let said = Command::new(app.join("Contents/MacOS/fno-app"))
        .arg("--version")
        .capture()
        .map_err(|e| format!("the staged app did not run: {e}"))?;
    if String::from_utf8_lossy(&said.stdout).trim() != format!("fno-app {version}") {
        return Err(format!("the staged app does not name version {version}").into());
    }
    Ok(())
}

/// Refuse before quitting when the swap cannot work: a translocated
/// bundle is read-only, and a folder the user cannot write to blocks the
/// rename. The relaunch script cannot report either.
fn swappable(bundle: &Path) -> Result<(), UpdateError> {
    if bundle.to_string_lossy().contains("/AppTranslocation/") {
        return Err("the app runs from a quarantined copy: move fno-app.app to Applications, then reopen it".into());
    }
    let dir = bundle.parent().ok_or("the app bundle has no folder")?;
    let probe = dir.join(format!(".fno-app-update-{}", std::process::id()));
    std::fs::write(&probe, b"")
        .map_err(|e| format!("cannot replace the app in {}: {e}", dir.display()))?;
    std::fs::remove_file(&probe).ok();
    Ok(())
}

/// Record the notes for the next launch, then start the swap-and-open
/// script. The caller quits right after: the script waits for this pid.
pub fn install_and_relaunch(ready: &Ready) -> Result<(), UpdateError> {
    let bundle = running_bundle().ok_or("not running from an app bundle")?;
    let root = update_dir().ok_or("no home directory")?;
    swappable(&bundle)?;
    let pending = Pending {
        version: ready.version.clone(),
        notes: ready.notes.clone(),
    };
    let text = serde_json::to_string(&pending).map_err(|e| e.to_string())?;
    std::fs::write(root.join("pending-notes.json"), text)
        .map_err(|e| format!("write the release notes: {e}"))?;
    Command::new("sh")
        .args(["-c", RELAUNCH, "sh"])
        .arg(&bundle)
        .arg(&ready.staged)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the relaunch script did not start: {e}"))?;
    Ok(())
}

/// The notes of the release this launch just installed, once. A record for
/// an older build is dropped; one for a newer build stays (the swap failed).
pub fn take_pending_notes(running: &str) -> Option<(String, String)> {
    let path = update_dir()?.join("pending-notes.json");
    let pending: Pending = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let theirs = version_key(&pending.version)?;
    let mine = version_key(running)?;
    if theirs > mine {
        return None;
    }
    std::fs::remove_file(&path).ok();
    (theirs == mine).then_some((pending.version, pending.notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_numerically_and_reject_suffixes() {
        assert!(version_key("0.0.10") > version_key("0.0.9"));
        assert_eq!(version_key("0.0.2-rc1"), None);
    }
}
