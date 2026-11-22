//! Self-update. The app reads the `fno-app-v*` release feed, stages a signed
//! build in the background, and offers a relaunch. Nothing here touches the
//! running bundle until the user clicks Restart.
//!
//! The staged build must pass four proofs before it is offered: the sha256
//! sidecar matches the tarball, `codesign --verify --deep --strict` passes,
//! the bundle's Team ID equals the running bundle's (when the running one
//! has a Team ID), and the staged binary names the release version. A dev
//! run outside a `.app` never updates.
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
const RELAUNCH: &str = r#"while kill -0 "$3" 2>/dev/null; do sleep 0.2; done; if mv "$1" "$1.old"; then if mv "$2" "$1"; then rm -rf "$1.old"; else mv "$1.old" "$1"; fi; fi; open "$1""#;

/// A build staged on disk and proven, waiting for the user's Restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub version: String,
    pub notes: String,
    pub staged: PathBuf,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
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

/// The newest stable `fno-app-v*` release past `running`: its version and
/// its notes.
fn newest(releases: Vec<Release>, running: &str) -> Option<(String, String)> {
    let have = version_key(running)?;
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG_PREFIX)?.to_string();
            let key = version_key(&version)?;
            (key > have).then(|| (key, version, r.body.unwrap_or_default()))
        })
        .max_by_key(|(key, _, _)| *key)
        .map(|(_, version, notes)| (version, notes))
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

fn curl(url: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
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
        .output()
        .map_err(|e| format!("curl did not run: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "curl exited {} for {url}",
            o.status.code().unwrap_or(-1)
        ));
    }
    Ok(o.stdout)
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let o = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| format!("shasum did not run: {e}"))?;
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "shasum printed no digest".to_string())
}

/// The bundle's Team ID; None for an ad hoc or unsigned bundle.
fn team_id(app: &Path) -> Option<String> {
    let o = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(app)
        .output()
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
        let keeps = keep.is_some_and(|k| e.file_name() == k);
        if e.path().is_dir() && !keeps {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Read the feed and, when a newer release exists, stage and prove it.
/// Blocking: call it off the UI thread. `Ok(None)` means up to date.
pub fn check(running: &str) -> Result<Option<Ready>, String> {
    let bundle = running_bundle().ok_or("not running from an app bundle")?;
    let root = update_dir().ok_or("no home directory")?;
    let feed = curl(
        &format!(
            "https://api.github.com/repos/{}/releases?per_page=100",
            repo_slug()
        ),
        None,
    )?;
    let releases: Vec<Release> =
        serde_json::from_slice(&feed).map_err(|e| format!("release feed: {e}"))?;
    let Some((version, notes)) = newest(releases, running) else {
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
    let work = root.join(format!("{version}.partial"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let proven = stage(&bundle, &version, &work);
    if let Err(e) = proven {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }
    std::fs::rename(&work, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(Some(Ready {
        version,
        notes,
        staged,
    }))
}

/// Download into `work`, unpack, and run the four proofs.
fn stage(bundle: &Path, version: &str, work: &Path) -> Result<(), String> {
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
        return Err(format!("sha256 mismatch: want {want}, got {got}"));
    }
    let untar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(work)
        .status()
        .map_err(|e| format!("tar did not run: {e}"))?;
    if !untar.success() {
        return Err("the tarball did not unpack".to_string());
    }
    let app = work.join("fno-app.app");
    let signed = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .status()
        .map_err(|e| format!("codesign did not run: {e}"))?;
    if !signed.success() {
        return Err("the staged bundle failed codesign verification".to_string());
    }
    if let Some(mine) = team_id(bundle) {
        if team_id(&app).as_deref() != Some(mine.as_str()) {
            return Err(format!("the staged bundle is not signed by team {mine}"));
        }
    }
    let said = Command::new(app.join("Contents/MacOS/fno-app"))
        .arg("--version")
        .output()
        .map_err(|e| format!("the staged app did not run: {e}"))?;
    if !String::from_utf8_lossy(&said.stdout).contains(version) {
        return Err(format!("the staged app does not name version {version}"));
    }
    Ok(())
}

/// Record the notes for the next launch, then start the swap-and-open
/// script. The caller quits right after: the script waits for this pid.
pub fn install_and_relaunch(ready: &Ready) -> Result<(), String> {
    let bundle = running_bundle().ok_or("not running from an app bundle")?;
    let root = update_dir().ok_or("no home directory")?;
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
    let _ = std::fs::remove_file(&path);
    (theirs == mine).then_some((pending.version, pending.notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.to_string(),
            body: Some(format!("notes for {tag}")),
            draft: false,
            prerelease,
        }
    }

    #[test]
    fn newest_takes_the_highest_stable_app_release_past_the_running_one() {
        let feed = vec![
            release("v9.9.9", false),
            release("fno-app-v0.0.9", false),
            release("fno-app-v0.0.10", false),
            release("fno-app-v0.1.0", true),
            release("fno-app-v0.0.1", false),
        ];
        assert_eq!(
            newest(feed, "0.0.2"),
            Some((
                "0.0.10".to_string(),
                "notes for fno-app-v0.0.10".to_string()
            ))
        );
        assert_eq!(
            newest(vec![release("fno-app-v0.0.2", false)], "0.0.2"),
            None
        );
    }
}
