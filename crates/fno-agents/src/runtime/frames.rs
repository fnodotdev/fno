//! Frame fingerprints at a head: the look carry's evidence.
//!
//! The carry used to compare the SET of paint files the approved commit and
//! the head touched, so a later commit that edited a paint file the approved
//! patch never showed voided the user's look even when no pixel moved. The
//! carry now renders the node's views at the approved sha and at the PR head
//! through the headless snapshot pipeline (a detached worktree, a debug
//! build, then the demo snapshot script per view), fingerprints the decoded
//! pixels, and carries on a match. A mismatch hands the frames to the design
//! critique; only a material change asks the user again. Frames that cannot
//! be produced or read answer [`FramesVerdict::Unprovable`], which the carry
//! turns into a warning that never holds.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One rendered view: a snapshot view name and its theme.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FrameView {
    pub view: String,
    pub theme: String,
}

impl FrameView {
    /// The shot stem the rendered file and the cache key share. An empty
    /// view is the demo board's own default frame, keyed on the theme.
    pub(crate) fn key(&self) -> String {
        if self.view.is_empty() {
            self.theme.clone()
        } else {
            format!("{}-{}", self.view, self.theme)
        }
    }
}

/// What the frames read answers for one approved..head pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FramesVerdict {
    /// Every view's fingerprint matches the approved sha's.
    Identical,
    /// At least one view differs; the keys name which.
    Differ(Vec<String>),
    /// The frames cannot be produced or read; the carry warns instead.
    Unprovable(String),
}

/// The render seam. The real probe runs the worktree pipeline; tests serve
/// fixture pngs from a table, so the compare logic needs no build.
pub(crate) trait FramesProbe {
    /// Render every view at `sha` into `out_dir` as `<view>-<theme>.png`.
    fn render_sha(
        &self,
        repo: &Path,
        sha: &str,
        views: &[FrameView],
        out_dir: &Path,
    ) -> Result<(), String>;
}

/// The views a node's carry judges: the critiqued views when a critique file
/// lists them, else the mux board in both themes.
pub(crate) fn views_for_node(dir: &Path, node: &str) -> Vec<FrameView> {
    let mut out: Vec<FrameView> = Vec::new();
    let path = dir.join(format!("{node}-critique.json"));
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let empty = Vec::new();
            for e in v.get("views").and_then(|x| x.as_array()).unwrap_or(&empty) {
                let view = e
                    .get("view")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                if view.is_empty() || view.contains("-carry-") {
                    // A staged carry critique names its paired shots, not
                    // snapshot views: feeding one back into the snapshot
                    // renderer fails the shot, so the pair names never count
                    // as views here.
                    continue;
                }
                let mut theme = e
                    .get("theme")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                if theme.is_empty() {
                    theme = crate::visual_screen::theme_of(&view);
                }
                let fv = FrameView { view, theme };
                if !out.contains(&fv) {
                    out.push(fv);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(FrameView {
            view: String::new(),
            theme: "dark".to_string(),
        });
        out.push(FrameView {
            view: String::new(),
            theme: "light".to_string(),
        });
    }
    out
}

/// The per-sha fingerprint cache: `<node>-frames.json` beside the node's
/// shots. A missing or corrupt cache re-renders; a failed write never fails
/// a read.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FramesCache {
    shas: BTreeMap<String, BTreeMap<String, String>>,
}

fn cache_path(dir: &Path, node: &str) -> PathBuf {
    dir.join(format!("{node}-frames.json"))
}

fn load_cache(dir: &Path, node: &str) -> FramesCache {
    std::fs::read_to_string(cache_path(dir, node))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_cache(dir: &Path, node: &str, cache: &FramesCache) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(cache_path(dir, node), json);
    }
}

/// The blake3 digest of a shot's decoded pixels and size, not its file bytes:
/// png encoding and metadata chunks vary, pixels do not.
fn fingerprint(path: &Path) -> Result<String, String> {
    let img = crate::visual_screen::decode(path)?;
    let mut h = blake3::Hasher::new();
    h.update(&img.w.to_le_bytes());
    h.update(&img.h.to_le_bytes());
    h.update(&img.px);
    Ok(h.finalize().to_hex()[..16].to_string())
}

/// Where one sha's rendered frames live: `<mux dir>/<node>-frames/<sha12>/`.
pub(crate) fn sha_dir(dir: &Path, node: &str, sha: &str) -> PathBuf {
    dir.join(format!("{node}-frames"))
        .join(&sha[..sha.len().min(12)])
}

/// The carry's frames read: fingerprint every view at the approved sha and
/// at the head, from the cache when both are warm, and compare.
pub(crate) fn compare_frames<P: FramesProbe>(
    probe: &P,
    dir: &Path,
    repo: &Path,
    node: &str,
    approved: &str,
    head: &str,
) -> FramesVerdict {
    let views = views_for_node(dir, node);
    let mut cache = load_cache(dir, node);
    let mut at = |sha: &str| -> Result<BTreeMap<String, String>, String> {
        if let Some(map) = cache.shas.get(sha) {
            return Ok(map.clone());
        }
        let out = sha_dir(dir, node, sha);
        probe.render_sha(repo, sha, &views, &out)?;
        let mut map = BTreeMap::new();
        for v in &views {
            let path = out.join(format!("{}.png", v.key()));
            map.insert(v.key(), fingerprint(&path)?);
        }
        cache.shas.insert(sha.to_string(), map.clone());
        save_cache(dir, node, &cache);
        Ok(map)
    };
    let (approved_map, head_map) = match (at(approved), at(head)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return FramesVerdict::Unprovable(e),
    };
    let differ: Vec<String> = views
        .iter()
        .map(|v| v.key())
        .filter(|k| approved_map.get(k) != head_map.get(k))
        .collect();
    if differ.is_empty() {
        FramesVerdict::Identical
    } else {
        FramesVerdict::Differ(differ)
    }
}

/// The real pipeline: a detached worktree at the sha, a debug build sharing
/// the checkout's target dir, then the demo snapshot script per view. The
/// worktree goes away after the shots land; a failed cleanup is not a failed
/// render.
pub(crate) struct RealFramesProbe;

impl FramesProbe for RealFramesProbe {
    fn render_sha(
        &self,
        repo: &Path,
        sha: &str,
        views: &[FrameView],
        out_dir: &Path,
    ) -> Result<(), String> {
        if out_dir.exists() {
            return Ok(());
        }
        let tmp = std::env::temp_dir().join(format!(
            "fno-frames-{}-{}",
            &sha[..sha.len().min(12)],
            std::process::id()
        ));
        let git = |args: &[&str]| -> Result<(), String> {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(repo);
            let out = crate::bounded_cmd::output_with_timeout(cmd, 120)
                .ok_or_else(|| format!("git {:?}: spawn failed", args))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "git {:?}: {}",
                    args,
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        // Render into a staging sibling and publish with one rename: a
        // concurrent read must never fingerprint a half-rendered dir, or a
        // wrong fingerprint lands in the cache under this sha for good.
        let staging = out_dir.with_extension(format!("tmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        let _ = std::fs::remove_dir_all(&tmp);
        // A crashed render leaves worktree metadata for a dir that is gone;
        // prune drops it so this add cannot refuse a re-used path.
        let _ = git(&["worktree", "prune"]);
        git(&[
            "worktree",
            "add",
            "--detach",
            tmp.to_str().unwrap_or(""),
            sha,
        ])?;
        let built = (|| -> Result<PathBuf, String> {
            // The repo has no root manifest: point cargo at the crate's own
            // Cargo.toml inside the worktree.
            let status = Command::new("cargo")
                .arg("build")
                .arg("--manifest-path")
                .arg(tmp.join("crates/fno/Cargo.toml"))
                .arg("--quiet")
                .current_dir(&tmp)
                .env("CARGO_TARGET_DIR", repo.join("target"))
                .status()
                .map_err(|e| format!("cargo build: {e}"))?;
            if !status.success() {
                return Err(format!("cargo build at {sha} failed"));
            }
            let bin = repo.join("target/debug/fno");
            if !bin.exists() {
                return Err("the built fno binary is missing".to_string());
            }
            Ok(bin)
        })();
        let mut shot_err = None;
        if let Ok(bin) = &built {
            let script = tmp.join("scripts/ops/mux-demo-snapshot.sh");
            for v in views {
                let out = staging.join(format!("{}.png", v.key()));
                let mut cmd = Command::new("bash");
                cmd.arg(&script).arg("--out").arg(&out);
                if !v.view.is_empty() {
                    cmd.arg("--view").arg(&v.view);
                }
                cmd.arg("--theme").arg(&v.theme).arg("--format").arg("png");
                let status = cmd.env("FNO_BIN", bin).current_dir(&tmp).status();
                match status {
                    Ok(s) if s.success() => {}
                    Ok(s) => {
                        shot_err = Some(format!(
                            "the demo snapshot for {} at {sha} exited {}",
                            v.key(),
                            s.code().unwrap_or(-1)
                        ));
                        break;
                    }
                    Err(e) => {
                        shot_err = Some(format!("the demo snapshot for {}: {e}", v.key()));
                        break;
                    }
                }
            }
        }
        let _ = git(&["worktree", "remove", "--force", tmp.to_str().unwrap_or("")]);
        let _ = std::fs::remove_dir_all(&tmp);
        match (built, shot_err) {
            (Ok(_), None) => {
                if std::fs::rename(&staging, out_dir).is_err() {
                    // Another read published this sha first; either dir
                    // fingerprints the same, so the loser just cleans up.
                    let _ = std::fs::remove_dir_all(&staging);
                }
                Ok(())
            }
            (Err(e), _) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
            }
            (Ok(_), Some(e)) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
            }
        }
    }
}

/// `fno-agents visual frames <node> --approved <sha> [--head <sha>] [--repo <path>]`: render the node's views at both shas, fingerprint each, and print the verdict. Exit 0 identical, 1 differ, 2 unprovable. A manual replay driver: the approval carry no longer calls it, the paint diff decides the carry.
pub fn run_frames(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("frames") || args.len() < 3 {
        eprintln!(
            "usage: fno-agents visual frames <node> --approved <sha> [--head <sha>] [--repo <path>]"
        );
        return 2;
    }
    let node = args
        .get(1)
        .cloned()
        .ok_or_else(|| eprintln!("a node argument is required"))
        .ok()
        .unwrap_or_default();
    let mut approved = None;
    let mut head = None;
    let mut repo = None;
    let mut rest = args.iter().skip(2);
    while let Some(flag) = rest.next().map(String::as_str) {
        match flag {
            "--approved" => approved = rest.next().cloned(),
            "--head" => head = rest.next().cloned(),
            "--repo" => repo = rest.next().cloned(),
            _ => {}
        }
    }
    let Some(approved) = approved else {
        eprintln!("--approved <sha> is required");
        return 2;
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let repo = repo
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::canonical_repo_root(&cwd).unwrap_or(cwd));
    let head = match head {
        Some(h) => h,
        None => match crate::bounded_cmd::output_with_timeout(
            {
                let mut c = Command::new("git");
                c.args(["rev-parse", "HEAD"]).current_dir(&repo);
                c
            },
            30,
        ) {
            Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => {
                eprintln!("cannot read HEAD; pass --head <sha>");
                return 2;
            }
        },
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = crate::escalation::vault_dir_with_home(&repo, home.as_deref(), "mux");
    match compare_frames(&RealFramesProbe, &dir, &repo, &node, &approved, &head) {
        FramesVerdict::Identical => {
            println!("identical: {approved} and {head} render the same frames");
            0
        }
        FramesVerdict::Differ(keys) => {
            for k in keys {
                println!("differ: {k}");
            }
            1
        }
        FramesVerdict::Unprovable(e) => {
            eprintln!("unprovable: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;

    /// One png per (sha, view-theme): dark views at `head` flip one pixel so
    /// the differ path has a real case.
    struct FakeProbe(Map<String, Map<String, String>>);

    impl FramesProbe for FakeProbe {
        fn render_sha(
            &self,
            _repo: &Path,
            sha: &str,
            views: &[FrameView],
            out_dir: &Path,
        ) -> Result<(), String> {
            let served = self
                .0
                .get(sha)
                .ok_or_else(|| format!("no frames for {sha}"))?;
            std::fs::create_dir_all(out_dir).unwrap();
            for v in views {
                let key = v.key();
                let digest = served
                    .get(&key)
                    .ok_or_else(|| format!("no frame for {key} at {sha}"))?;
                let path = out_dir.join(format!("{key}.png"));
                write_png(&path, digest);
            }
            Ok(())
        }
    }

    /// A tiny valid png whose decoded pixels encode the digest text, so two
    /// digests never collide and one flipped pixel reads as a differ.
    fn write_png(path: &Path, digest: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 4, 1);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        let bytes = digest.as_bytes();
        let px: Vec<u8> = bytes
            .chunks_exact(4)
            .flat_map(|c| c.iter().copied())
            .collect();
        w.write_image_data(&px).unwrap();
    }

    fn served(values: &[&str]) -> Map<String, String> {
        let keys = ["dark", "light"];
        keys.iter()
            .zip(values.iter())
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn identical_frames_carry_and_a_flipped_view_names_itself() {
        let dir = std::env::temp_dir().join(format!("fno-frames-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let node = "test-frames";
        let mut fake = Map::new();
        fake.insert(
            "aaaaaaaaaa1".to_string(),
            served(&["p1p1p1p1p1p1p1p1", "q1q1q1q1q1q1q1q1"]),
        );
        fake.insert(
            "aaaaaaaaaa2".to_string(),
            served(&["p1p1p1p1p1p1p1p1", "q1q1q1q1q1q1q1q1"]),
        );
        fake.insert(
            "aaaaaaaaaa3".to_string(),
            served(&["p1p1p1p1p1p1p1p2", "q1q1q1q1q1q1q1q1"]),
        );
        let probe = FakeProbe(fake);
        let repo = std::path::Path::new("/nonexistent");
        assert_eq!(
            compare_frames(&probe, &dir, repo, node, "aaaaaaaaaa1", "aaaaaaaaaa2"),
            FramesVerdict::Identical
        );
        assert_eq!(
            compare_frames(&probe, &dir, repo, node, "aaaaaaaaaa1", "aaaaaaaaaa3"),
            FramesVerdict::Differ(vec!["dark".to_string()])
        );
        assert!(matches!(
            compare_frames(&probe, &dir, repo, node, "aaaaaaaaaa9", "aaaaaaaaaa1"),
            FramesVerdict::Unprovable(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
