//! The worker-only `gh` proxy installer. The proxy is a two-line shim,
//! `exec <fno-agents> gh "$@"`, and `fno doctor update` writes it over the
//! machine's first-PATH `gh` (the real binary backed up beside it as
//! `gh.pre-fno`), so plain `gh` on every caller's PATH runs the quota
//! broker: reads carry the GitHub App token, a stale answer costs an ETag
//! revalidation instead of core quota, and the fleet budget stays the only
//! admission layer. A dir the installer cannot write falls back to the
//! state root's `install/bin/github-cli`, and doctor names the shadow.
//! The shim names fno-agents by absolute path: a reinstall that dropped
//! the old `fno-gh-proxy` helper once broke gh in every session.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const PROXY_DIR_ENV: &str = "FNO_GH_PROXY_DIR";
/// The proxy stamps this into the env of its own successor. It is good for
/// that one hop only: a descendant that inherits it has its first gh refused.
pub const PROXY_DEPTH_ENV: &str = "FNO_GH_PROXY_DEPTH";
const LEGACY_SHIM: &str = "#!/bin/sh\nexec fno-gh-proxy \"$@\"\n";

/// The state root's `install/bin/github-cli` dir: `$FNO_STATE_DIR` or `$HOME/.fno`,
/// never a settings load. The quota broker reads the same dir.
pub fn proxy_dir() -> Option<PathBuf> {
    let root = std::env::var_os("FNO_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))?;
    Some(crate::state_layout::place(&root, "bin").join("github-cli"))
}

/// `shlex.quote`: bare when every byte is shell-safe, else single-quoted.
pub fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// The inverse of [`shell_quote`] for one word.
fn shell_unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    c => out.push(c),
                }
            },
            c => out.push(c),
        }
    }
    Some(out)
}

pub fn wrapper(binary: &Path) -> String {
    format!(
        "#!/bin/sh\nexec {} gh \"$@\"\n",
        shell_quote(&binary.to_string_lossy())
    )
}

/// Any shim this installer wrote, current or from an older install.
pub fn is_wrapper(path: &Path) -> bool {
    if std::fs::read_to_string(path).ok().as_deref() == Some(LEGACY_SHIM) {
        return true;
    }
    wrapper_target(path).is_some()
}

/// The binary a wrapper shim execs, when the file reads as one of ours.
pub fn wrapper_target(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    if text == LEGACY_SHIM {
        return None;
    }
    let line = text
        .strip_prefix("#!/bin/sh\n")
        .and_then(|rest| rest.lines().next())?;
    let binary = line.strip_prefix("exec ")?.strip_suffix(" gh \"$@\"")?;
    shell_unquote(binary).map(PathBuf::from)
}

/// A wrapper whose exec target is gone: every plain gh call on the machine
/// fails until doctor update re-points or removes the shim.
pub fn is_dangling(path: &Path) -> bool {
    wrapper_target(path).is_some_and(|target| !is_executable(&target))
}

pub(crate) fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn which(name: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Temp roots a fixture or a fallback shim can live under, fixed per process
/// so the verdict cannot drift with the caller's env: a scrubbed-env driver
/// floors the temp dir to /tmp while the fixture sits under the OS temp root,
/// and one misread wrote the durable shim a sweep then deletes.
fn temp_bases() -> Vec<PathBuf> {
    let env = std::env::temp_dir();
    let mut bases = vec![resolve(&env), env];
    for base in [
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
    ] {
        let base = PathBuf::from(base);
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    bases
}

/// The path's own spellings, raw and resolved, strictly under a base: the
/// caller's tempdir need not be the binary's, and a sibling sharing the
/// prefix string is not under it.
pub(crate) fn in_temp(path: &Path) -> bool {
    let raw = path.to_path_buf();
    let resolved = resolve(path);
    temp_bases().iter().any(|base| {
        [raw.as_path(), resolved.as_path()]
            .into_iter()
            .any(|spelling| spelling != base && spelling.starts_with(base))
    })
}

/// The fno-agents a shim names. A cargo build dir is swept with its
/// worktree, so an installed copy on PATH wins over it.
fn shim_binary(path: &str) -> Option<PathBuf> {
    let binary = crate::digest_overlay::fno_agents_bin_auto()?;
    let in_cargo_target = binary
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|n| n == "target");
    if in_cargo_target {
        if let Some(installed) = which("fno-agents", path) {
            return Some(installed);
        }
    }
    Some(binary)
}

/// Write the shim into `root`, atomically, backing a foreign `gh` up to
/// `gh.pre-fno` once. Refuses when the exec target is the proxy file
/// itself, which would loop.
pub fn install_shim(root: &Path, binary: &Path) -> Result<(), String> {
    let wrapper = wrapper(&std::path::absolute(binary).unwrap_or_else(|_| binary.to_path_buf()));
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    // Retired: the old shim exec'd this link by PATH name.
    let _ = std::fs::remove_file(root.join("fno-gh-proxy"));
    let proxy = root.join("gh");
    if resolve(binary) == resolve(&proxy) {
        return Err("the shim would exec the proxy itself".into());
    }
    if std::fs::read_to_string(&proxy).ok().as_deref() == Some(wrapper.as_str()) {
        return Ok(());
    }
    if proxy.exists() && !is_wrapper(&proxy) {
        let backup = root.join("gh.pre-fno");
        if !backup.exists() {
            std::fs::copy(&proxy, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
        }
    }
    let mut tmp = tempfile::Builder::new()
        .prefix(".gh.")
        .tempfile_in(root)
        .map_err(|e| format!("{}: {e}", root.display()))?;
    tmp.write_all(wrapper.as_bytes())
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| format!("{}: {e}", proxy.display()))?;
    let mut mode = tmp
        .as_file()
        .metadata()
        .map_err(|e| e.to_string())?
        .permissions();
    mode.set_mode(mode.mode() | 0o111);
    std::fs::set_permissions(tmp.path(), mode).map_err(|e| e.to_string())?;
    tmp.persist(&proxy).map_err(|e| e.error.to_string())?;
    Ok(())
}

/// Install the shim in `root` (the state-root dir when `None`). `Ok(None)`
/// is the degrade: no fno-agents to exec, or a temp-dir binary a durable
/// shim must not pin, so gh stays unwrapped and the quota policy stands down.
fn ensure_proxy(root: Option<&Path>, path: &str) -> Result<Option<PathBuf>, String> {
    let Some(binary) = shim_binary(path) else {
        return Ok(None);
    };
    if !is_executable(&binary) {
        // shim_binary checked runnable, but a sweep can delete the file
        // between resolve and install, and a shim that execs a missing
        // binary breaks every gh call on the machine.
        return Ok(None);
    }
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => proxy_dir().ok_or("no HOME and no FNO_STATE_DIR")?,
    };
    if in_temp(&binary) && !in_temp(&root) {
        return Ok(None);
    }
    install_shim(&root, &binary)?;
    Ok(Some(root))
}

/// Why the one install entry refused. Typed because the crate's string
/// error count is shrink-only (docs/rust-style.md R6); the doctor and
/// update callers print the Display text.
#[derive(Debug)]
pub enum InstallRefusal {
    NoGh,
    NoAgents,
    NoParentDir,
}

impl std::fmt::Display for InstallRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::NoGh => "no gh on PATH",
            Self::NoAgents => "no fno-agents binary to exec",
            Self::NoParentDir => "gh on PATH has no parent dir",
        };
        f.write_str(text)
    }
}

/// The one install entry: shadow the machine's first-PATH `gh` with the
/// proxy shim, so plain `gh` on every caller's PATH runs the quota broker.
/// A dir the installer cannot write falls back to the state root's
/// `install/bin/github-cli`; the returned line names where the shim sits
/// and whether a PATH export still stands between it and plain gh. An
/// `Err` names the blocker (no gh on PATH, no fno-agents to exec).
pub fn install_for_path() -> Result<String, InstallRefusal> {
    let path = std::env::var("PATH").unwrap_or_default();
    let Some(delegate) = which("gh", &path) else {
        return Err(InstallRefusal::NoGh);
    };
    let Some(binary) = shim_binary(&path).filter(|p| is_executable(p)) else {
        return Err(InstallRefusal::NoAgents);
    };
    let dir = delegate
        .parent()
        .ok_or(InstallRefusal::NoParentDir)?
        .to_path_buf();
    let action = if is_wrapper(&delegate) {
        "refreshed"
    } else {
        "installed"
    };
    if install_shim(&dir, &binary).is_ok() {
        return Ok(format!(
            "gh proxy shim {action} at {}; plain gh runs the proxy",
            dir.display()
        ));
    }
    // The first-PATH dir refused the write (unwritable, or the loop
    // guard): fall back to the state root's durable home and report the
    // shadow honestly. Doctor's report reads the same verdict.
    let root = ensure_proxy(None, &path)?.ok_or(InstallRefusal::NoAgents)?;
    Ok(format!(
        "gh proxy shim at {}; {} still resolves first on PATH; put the shim dir ahead of it to activate",
        root.display(),
        delegate.display()
    ))
}
