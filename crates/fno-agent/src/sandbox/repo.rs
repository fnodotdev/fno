//! The clone inside and the commits back. No bind: committed HEAD crosses
//! in as a bundle, and the worker's commits cross out as a bundle the host
//! fetches into `refs/sandbox/<id>/head` with hooks off and object checks on.
//! No host branch moves. Dirty and untracked work is stored as a patch and
//! an archive the host never applies or extracts.

use super::{Refusal, Res};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::engine::Engine;
use super::CONTAINER_WORK;

const BUNDLE_IN: &str = "/tmp/fno-in.bundle";
const OUT_DIR: &str = "/tmp/fno-out";
/// The most one export file may be once it is on the host.
pub const EXPORT_CAP: u64 = 256 * 1024 * 1024;

fn git(dir: &Path, args: &[&str]) -> Res<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    let out = crate::bounded_cmd::output_with_timeout_result(c, 300)
        .map_err(|e| format!("git {}: {e}", args.first().copied().unwrap_or("")))?;
    if !out.status.success() {
        return Err(Refusal(format!(
            "git {} exited {}: {}",
            args.first().copied().unwrap_or(""),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What crossed in: the base commit and the dirty host paths left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intake {
    pub base: String,
    pub excluded_dirty: Vec<String>,
}

/// Bundle committed HEAD of `worktree`, copy it in, and clone it at the work
/// path. The receipt names the dirty host paths the clone does not carry.
pub fn clone_in(engine: &Engine, name: &str, worktree: &Path, scratch: &Path) -> Res<Intake> {
    let base = git(worktree, &["rev-parse", "HEAD"])?;
    let status = git(
        worktree,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    let excluded_dirty = status
        .lines()
        .filter_map(|l| l.get(3..))
        .map(|p| super::escape(p, 200))
        .collect();
    let bundle = scratch.join("in.bundle");
    git(
        worktree,
        &["bundle", "create", &bundle.display().to_string(), "HEAD"],
    )?;
    engine.copy_in(name, &bundle, BUNDLE_IN)?;
    let script = format!(
        "set -e; git clone -q {BUNDLE_IN} {CONTAINER_WORK} && \
         cd {CONTAINER_WORK} && git checkout -q -f -B sandbox {base} && \
         git config user.name 'fno sandbox' && git config user.email sandbox@fno.invalid"
    );
    engine.exec_out(name, &["sh", "-c", &script])?;
    Ok(Intake {
        base,
        excluded_dirty,
    })
}

/// What came back out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Export {
    /// The fetched head under `refs/sandbox/<id>/head`, when the worker
    /// committed anything.
    pub head: Option<String>,
    pub patch: Option<PathBuf>,
    pub untracked: Option<PathBuf>,
    pub notes: Vec<String>,
}

/// Export the worker's commits, dirty patch and untracked archive, then
/// fetch the commits into the sandbox ref. `Err` keeps the container: no
/// work is deleted when the export fails.
pub fn export(
    engine: &Engine,
    name: &str,
    id: &str,
    base: &str,
    worktree: &Path,
    store: &Path,
) -> Res<Export> {
    if !base.bytes().all(|b| b.is_ascii_hexdigit()) || !super::valid_id(id) {
        return Err("refusing an export with an unchecked base or id".into());
    }
    let script = format!(
        "set -e; cd {CONTAINER_WORK}; mkdir -p {OUT_DIR}; rm -f {OUT_DIR}/*; \
         if [ \"$(git rev-parse HEAD)\" != {base} ]; then git bundle create -q {OUT_DIR}/out.bundle {base}..HEAD; fi; \
         git diff --binary HEAD > {OUT_DIR}/dirty.patch; \
         git ls-files -o --exclude-standard -z | tar --null -T - -cf {OUT_DIR}/untracked.tar; \
         ls {OUT_DIR}"
    );
    let listed = engine.exec_out(name, &["sh", "-c", &script])?;
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    std::fs::create_dir_all(store).map_err(|e| format!("{}: {e}", store.display()))?;
    let mut out = Export::default();
    let take = |file: &str| -> Res<Option<PathBuf>> {
        if !listed.lines().any(|l| l.trim() == file) {
            return Ok(None);
        }
        let host = store.join(file);
        engine.copy_out(name, &format!("{OUT_DIR}/{file}"), &host)?;
        // The container wrote this file: a link or a folder in its place
        // never stays in the host store.
        let meta = std::fs::symlink_metadata(&host).map_err(|e| format!("{file}: {e}"))?;
        if !meta.file_type().is_file() {
            if meta.is_dir() {
                std::fs::remove_dir_all(&host).ok();
            } else {
                std::fs::remove_file(&host).ok();
            }
            return Err(Refusal(format!(
                "{file} is not a plain file; the container is kept"
            )));
        }
        let len = meta.len();
        if len > EXPORT_CAP {
            std::fs::remove_file(&host).ok();
            return Err(Refusal(format!("{file} is {len} bytes, over the {EXPORT_CAP}-byte export cap; the container is kept")));
        }
        Ok((len > 0).then_some(host))
    };
    let bundle = take("out.bundle")?;
    out.patch = take("dirty.patch")?;
    out.untracked = take("untracked.tar")?;
    if let Some(bundle) = bundle {
        let b = bundle.display().to_string();
        git(worktree, &["bundle", "verify", "-q", &b])?;
        let refname = format!("refs/sandbox/{id}/head");
        git(
            worktree,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "transfer.fsckObjects=true",
                "-c",
                "fetch.fsckObjects=true",
                "fetch",
                "-q",
                "--no-tags",
                "--no-write-fetch-head",
                &b,
                &format!("+HEAD:{refname}"),
            ],
        )?;
        out.head = Some(git(worktree, &["rev-parse", &refname])?);
        std::fs::remove_file(&bundle).ok();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker's commits fetch into `refs/sandbox/<id>/head` only: no host
    /// branch moves, and a planted hook in the bundle's repo never runs.
    #[test]
    fn intake_writes_only_the_sandbox_ref_with_hooks_off() {
        let host = tempfile::tempdir().unwrap();
        let guest = tempfile::tempdir().unwrap();
        let sh = |dir: &Path, s: &str| {
            let ok = Command::new("sh")
                .arg("-c")
                .arg(s)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(ok.success(), "{s}");
        };
        sh(host.path(), "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m base");
        let base = git(host.path(), &["rev-parse", "HEAD"]).unwrap();
        let main_before = git(host.path(), &["rev-parse", "refs/heads/main"]).unwrap();
        sh(guest.path(), &format!("git clone -q {} w && cd w && echo x > f && git add f && git -c user.name=t -c user.email=t@t commit -q -m work && git bundle create -q ../out.bundle {base}..HEAD", host.path().display()));
        let marker = host.path().join("hook-ran");
        sh(host.path(), &format!("mkdir -p .git/hooks && printf '#!/bin/sh\\ntouch {}\\n' > .git/hooks/reference-transaction && chmod +x .git/hooks/reference-transaction", marker.display()));
        let b = guest.path().join("out.bundle").display().to_string();
        git(
            host.path(),
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "fetch",
                "-q",
                "--no-tags",
                "--no-write-fetch-head",
                &b,
                "+HEAD:refs/sandbox/w-1/head",
            ],
        )
        .unwrap();
        assert_ne!(
            git(host.path(), &["rev-parse", "refs/sandbox/w-1/head"]).unwrap(),
            base
        );
        assert_eq!(
            git(host.path(), &["rev-parse", "refs/heads/main"]).unwrap(),
            main_before
        );
        assert!(!marker.exists(), "no host hook runs on intake");
        assert!(export(
            &Engine {
                bin: "docker",
                host: None
            },
            "w",
            "w-1",
            "not-hex",
            host.path(),
            host.path()
        )
        .is_err());
    }
}
