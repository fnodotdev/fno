//! The one class-home resolver for both binaries: where rebuildable caches,
//! installed payloads and logs live outside the state root.
//!
//! The front door (`crates/fno`) and the daemon (`crates/fno-agent`) shell
//! each other at runtime and share no crate above this one, so the whole
//! answer lives here: the precedence (env carrier, config value, platform
//! default), the value treatment, and the platform table. A caller passes
//! the raw config string from whatever config walk it already runs, so this
//! crate stays dependency-free. The class homes themselves are machine-wide
//! (never per-project): a `{template}` value is no answer, the same rule the
//! shell twins and `fno config get` follow.
//!
//! One home per CLASS of file: rebuildable caches go to the OS cache dir,
//! binaries to the OS data dir, logs to the OS state dir; the state root
//! keeps config and irreplaceable data. The defaults follow the `dirs`
//! crate conventions in plain `std::env` code (the crate is not a
//! dependency), with two deliberate divergences: data home is
//! `~/.local/share/fno` on macOS too (the CLI convention, not Application
//! Support), and state home is `~/.local/state/fno` on every platform
//! (macOS `dirs` answers None there, but the logs family needs a real
//! answer). Cache is `~/Library/Caches/fno` on macOS, `$XDG_CACHE_HOME/fno`
//! else `~/.cache/fno`, and `%LOCALAPPDATA%\fno\cache` on Windows (written,
//! untested: no CI host).

use std::path::{Path, PathBuf};

/// The class of file a home holds. The env carrier and config key are the
/// caller's (each crate names its own); the precedence and platform table
/// are ours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassHome {
    /// Rebuildable caches (`FNO_CACHE_HOME` / `paths.cache_dir`).
    Cache,
    /// Binaries and installed release payloads (`FNO_DATA_HOME` / `paths.data_dir`).
    Data,
    /// Logs and the other state files that belong outside the config home
    /// (`FNO_STATE_HOME` / `paths.state_dir_local`).
    State,
}

/// One class home: the env carrier, then the caller's config value, then
/// the platform default. A config value carrying a glob or an unresolved
/// `{template}` is no answer, so a broken override can never move a class
/// home to a literal `*` directory.
pub fn class_home(
    cwd: &Path,
    env_key: &str,
    config_raw: Option<&str>,
    class: ClassHome,
) -> PathBuf {
    if let Some(carrier) = non_empty_env_os(env_key) {
        let text = expanduser(&carrier.to_string_lossy());
        let anchored = anchor_at(cwd, PathBuf::from(&text));
        return resolve_loose(&anchored);
    }
    if let Some(raw) = config_raw {
        if let Some(path) = resolve_config_value(cwd, raw) {
            return path;
        }
    }
    default_class_home(class)
}

/// [`class_home`] without the config read, for a PreToolUse path: env, then
/// the platform default. The twin of `fno_cache_home_fast` in
/// `scripts/lib/class-home.sh`.
pub fn class_home_fast(cwd: &Path, env_key: &str, class: ClassHome) -> PathBuf {
    class_home(cwd, env_key, None, class)
}

/// The platform default home of one class. The only place in the repo that
/// joins the porcelain name; the seam-crossings ratchet pins it as the one
/// resolver row.
pub fn default_class_home(class: ClassHome) -> PathBuf {
    let base = match class {
        ClassHome::Cache => {
            if cfg!(windows) {
                non_empty_env_os("LOCALAPPDATA").map_or_else(home_dir, PathBuf::from)
            } else if cfg!(target_os = "macos") {
                home_dir().join("Library").join("Caches")
            } else {
                non_empty_env_os("XDG_CACHE_HOME")
                    .map_or_else(|| home_dir().join(".cache"), PathBuf::from)
            }
        }
        ClassHome::Data => {
            if cfg!(windows) {
                non_empty_env_os("LOCALAPPDATA").map_or_else(home_dir, PathBuf::from)
            } else {
                non_empty_env_os("XDG_DATA_HOME")
                    .map_or_else(|| home_dir().join(".local").join("share"), PathBuf::from)
            }
        }
        ClassHome::State => non_empty_env_os("XDG_STATE_HOME")
            .map_or_else(|| home_dir().join(".local").join("state"), PathBuf::from),
    };
    let home = base.join("fno");
    match class {
        ClassHome::Cache if cfg!(windows) => home.join("cache"),
        _ => home,
    }
}

/// One config value as a path: a glob, an unresolved `{template}`, or a
/// `$VAR` that no environment answers is no answer (`None` falls to the
/// platform default); a set `$VAR` expands, a `~` goes `$HOME`-relative,
/// and a relative remainder anchors at `cwd`.
fn resolve_config_value(cwd: &Path, raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.contains(['*', '?', '[']) {
        return None;
    }
    let expanded = expandvars(&trimmed.trim_end_matches('/').to_string());
    if has_template(&expanded) || expanded.contains('$') {
        return None;
    }
    let text = expanduser(&expanded);
    Some(anchor_at(cwd, PathBuf::from(text)))
}

/// `std::env::var_os` but an empty value reads as unset.
fn non_empty_env_os(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|v| !v.is_empty())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `os.path.expandvars`: `$NAME` and `${NAME}` for set variables; anything
/// else stays as written.
fn expandvars(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let braced = chars.get(i + 1) == Some(&'{');
        let start = if braced { i + 2 } else { i + 1 };
        let mut end = start;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        let closed = !braced || chars.get(end) == Some(&'}');
        let name: String = chars[start..end].iter().collect();
        match (name.is_empty() || !closed, std::env::var(&name)) {
            (false, Ok(value)) => {
                out.push_str(&value);
                i = if braced { end + 1 } else { end };
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// `os.path.expanduser` for `~` and `~/...`.
fn expanduser(raw: &str) -> String {
    if raw == "~" {
        return home_dir().to_string_lossy().into_owned();
    }
    match raw.strip_prefix("~/") {
        Some(rest) => format!(
            "{}/{rest}",
            home_dir().to_string_lossy().trim_end_matches('/')
        ),
        None => raw.to_string(),
    }
}

/// A relative path anchors at `base`; an absolute one passes through.
fn anchor_at(base: &Path, p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// `Path.resolve()` for a path that may not exist: canonicalize the deepest
/// existing ancestor, re-append the missing tail.
fn resolve_loose(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        match std::fs::canonicalize(&cur) {
            Ok(existing) => {
                let mut out = existing;
                for comp in missing.iter().rev() {
                    out.push(comp);
                }
                return out;
            }
            Err(_) => match (cur.file_name().map(|f| f.to_os_string()), cur.parent()) {
                (Some(name), Some(parent)) => {
                    missing.push(name);
                    cur = parent.to_path_buf();
                }
                _ => return path.to_path_buf(),
            },
        }
    }
}

/// True when the raw value carries a `{template}` variable. A `{{` escape
/// never counts, mirroring the Python regex's lookarounds
/// (`(?<!\{)\{([^{}]+)\}(?!\})`).
fn has_template(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' || (i > 0 && chars[i - 1] == '{') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '{' && chars[j] != '}' {
            j += 1;
        }
        let closed = j > i + 1 && chars.get(j) == Some(&'}') && chars.get(j + 1) != Some(&'}');
        if closed {
            return true;
        }
        i += 1;
    }
    false
}
