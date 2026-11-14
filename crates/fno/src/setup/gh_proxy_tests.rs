//! Contracts for the gh proxy installer: the shim names fno-agents by
//! absolute path and reads back as a shim, a foreign gh is backed up once,
//! a binary that is the proxy file itself refuses, and install_for_path
//! shadows the first-PATH gh, falling back to the state root when the dir
//! refuses the write.

use crate::gh_proxy::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// An executable fixture binary in dir, content irrelevant.
fn fixture(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    let mut mode = std::fs::metadata(&p).unwrap().permissions();
    mode.set_mode(mode.mode() | 0o111);
    std::fs::set_permissions(&p, mode).unwrap();
    p
}

/// Pin the env inputs install_for_path_in reads; restores on drop. PATH is
/// passed as an argument instead, so parallel tests keep their spawns.
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(agents: &Path, state: &Path) -> Self {
        let lock = crate::model_catalog::state_env_lock();
        let saved = vec![
            ("FNO_AGENTS_BIN", std::env::var_os("FNO_AGENTS_BIN")),
            ("FNO_STATE_DIR", std::env::var_os("FNO_STATE_DIR")),
        ];
        std::env::set_var("FNO_AGENTS_BIN", agents);
        std::env::set_var("FNO_STATE_DIR", state);
        EnvGuard { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[test]
fn wrapper_reads_back_as_a_shim_with_or_without_quoting() {
    let dir = tempfile::tempdir().unwrap();
    for binary in ["/opt/fno/bin/fno-agents", "/Users/a b/it's/fno-agents"] {
        let shim = dir.path().join("gh");
        std::fs::write(&shim, wrapper(Path::new(binary))).unwrap();
        assert!(is_wrapper(&shim), "{binary}");
    }
    assert_eq!(shell_quote("/a/b"), "/a/b");
    assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    let foreign = dir.path().join("other");
    std::fs::write(&foreign, "#!/bin/sh\nexec /usr/bin/gh \"$@\"\n").unwrap();
    assert!(!is_wrapper(&foreign));
}

#[test]
fn install_backs_up_a_foreign_gh_once_and_drops_the_legacy_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("github-cli");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("gh"), "real gh bytes").unwrap();
    std::fs::write(root.join("fno-gh-proxy"), "old").unwrap();
    let binary = Path::new("/opt/fno/bin/fno-agents");

    install_shim(&root, binary).unwrap();
    let shim = root.join("gh");
    assert_eq!(std::fs::read_to_string(&shim).unwrap(), wrapper(binary));
    assert!(std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111 != 0);
    assert_eq!(
        std::fs::read_to_string(root.join("gh.pre-fno")).unwrap(),
        "real gh bytes"
    );
    assert!(!root.join("fno-gh-proxy").exists());

    // A rerun over its own shim backs nothing up again.
    install_shim(&root, binary).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("gh.pre-fno")).unwrap(),
        "real gh bytes"
    );
}

#[test]
fn install_refuses_a_binary_that_is_the_shim() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let selfgh = root.join("gh");
    std::fs::write(&selfgh, "x").unwrap();
    let err = install_shim(&root, &selfgh).unwrap_err();
    assert!(err.contains("exec the proxy itself"), "{err}");
}

#[test]
fn install_for_path_shadows_the_first_real_gh() {
    let dir = tempfile::tempdir().unwrap();
    let bindir = dir.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let real = fixture(&bindir, "gh", "real gh bytes");
    let agents = fixture(dir.path(), "fno-agents", "#!/bin/sh");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let _g = EnvGuard::set(&agents, &state);
    let out = install_for_path_in(&bindir.to_string_lossy()).unwrap();
    assert!(out.contains("installed"), "{out}");
    assert!(is_wrapper(&real), "shadowed in place");
    assert_eq!(
        std::fs::read_to_string(bindir.join("gh.pre-fno")).unwrap(),
        "real gh bytes"
    );
}

#[test]
fn install_for_path_falls_back_when_the_dir_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let bindir = dir.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let real = fixture(&bindir, "gh", "real gh bytes");
    let agents = fixture(dir.path(), "fno-agents", "#!/bin/sh");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let _g = EnvGuard::set(&agents, &state);
    let mut mode = std::fs::metadata(&bindir).unwrap().permissions();
    mode.set_mode(0o555);
    std::fs::set_permissions(&bindir, mode).unwrap();
    let out = install_for_path_in(&bindir.to_string_lossy()).unwrap();
    assert!(out.contains("still resolves first"), "{out}");
    assert!(!is_wrapper(&real), "the real gh stays raw");
    let shim = state.join("install/bin/github-cli/gh");
    assert!(is_wrapper(&shim), "fallback shim at the state root");
}

#[test]
fn install_for_path_without_gh_errs() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let agents = fixture(dir.path(), "fno-agents", "#!/bin/sh");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let _g = EnvGuard::set(&agents, &state);
    let err = install_for_path_in(&empty.to_string_lossy()).unwrap_err();
    assert!(err.to_string().contains("no gh on PATH"), "{err}");
}

#[test]
fn a_wrapper_whose_exec_target_vanished_reads_dangling() {
    let dir = tempfile::tempdir().unwrap();
    let bindir = dir.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let real = fixture(&bindir, "gh", "real gh bytes");
    let agents = fixture(dir.path(), "fno-agents", "#!/bin/sh");
    install_shim(&bindir, &agents).unwrap();
    assert_eq!(
        wrapper_target(&real)
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        "fno-agents"
    );
    assert!(!is_dangling(&real));
    std::fs::remove_file(&agents).unwrap();
    assert!(is_dangling(&real));
    assert!(is_wrapper(&real), "still ours, just dangling");
}
