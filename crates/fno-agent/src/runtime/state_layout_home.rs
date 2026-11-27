//! Where does a cache or log path of a state root live?
//!
//! In a table destination the first segment `cache` or `logs` names a class.
//! The default root (`$HOME/.fno`) keeps those two in the machine's cache
//! home and state home. Any other root keeps them inside itself, so one root
//! never takes another's files. The class homes themselves are machine-wide
//! (`paths_cli`, and the shell twins): the root scoping happens here only.

use std::path::{Path, PathBuf};

fn default_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".fno"))
}

/// Whether `root` is this HOME's default state root.
pub(crate) fn is_default_root(root: &Path) -> bool {
    default_root().is_some_and(|default| same_path(root, &default))
}

/// One path under two spellings (a symlinked HOME, `/var` and `/private/var`).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

fn class_cwd(root: &Path) -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| root.to_path_buf())
}

/// The cache folder of `root`.
pub fn cache_dir(root: &Path) -> PathBuf {
    if is_default_root(root) {
        crate::paths_cli::cache_home(&class_cwd(root))
    } else {
        root.join("cache")
    }
}

/// [`cache_dir`] for a PreToolUse path: env, then the platform default. It
/// reads no config, the same trade `fno_cache_home_fast` makes in shell.
pub(crate) fn cache_dir_fast(root: &Path) -> PathBuf {
    if is_default_root(root) {
        crate::paths_cli::cache_home_fast(&class_cwd(root))
    } else {
        root.join("cache")
    }
}

/// The logs folder of `root`.
pub fn logs_dir(root: &Path) -> PathBuf {
    if is_default_root(root) {
        crate::paths_cli::state_home(&class_cwd(root)).join("logs")
    } else {
        root.join("logs")
    }
}

/// A root-relative table destination as a path. Only a class name reads the
/// environment or config, so every other name costs one join.
pub fn home_of(root: &Path, rel: &str) -> PathBuf {
    let (head, rest) = rel.split_once('/').unwrap_or((rel, ""));
    let base = match head {
        "cache" => cache_dir(root),
        "logs" => logs_dir(root),
        _ => return root.join(rel),
    };
    if rest.is_empty() {
        base
    } else {
        base.join(rest)
    }
}

/// The read ladder for a root-relative name: its home when that exists, else
/// the spelling inside the root, else the home.
pub(crate) fn read_home(root: &Path, rel: &str) -> PathBuf {
    let home = home_of(root, rel);
    let inside = root.join(rel);
    if !home.exists() && inside.exists() {
        inside
    } else {
        home
    }
}

/// The pre-move spelling of a path under the default root's cache home,
/// when something still sits there.
fn cache_twin(path: &Path) -> Option<PathBuf> {
    let root = default_root()?;
    let rel = path.strip_prefix(cache_dir(&root)).ok()?;
    let twin = root.join("cache").join(rel);
    (twin != path && twin.exists()).then_some(twin)
}

/// Where to read one cache file: `path`, else its pre-move spelling. Writes
/// always go to `path`; a writer that followed this fallback would keep the
/// legacy copy fresh, and the quiet window would never close.
pub(crate) fn cache_read(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    cache_twin(path).unwrap_or_else(|| path.to_path_buf())
}

/// The folders a cache lister reads: `dir`, then its pre-move spelling.
pub(crate) fn cache_read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(cache_twin(dir));
    dirs
}
