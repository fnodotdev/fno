//! How does one path move without loss?
//!
//! On one volume a move is a rename, so open descriptors, modes and links
//! survive. Across volumes it copies into a temp sibling, renames that into
//! place, and only then removes the source. A failed copy removes its temp,
//! so the next pass never reads a partial copy as a destination.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The ways a move fails. Each leaves every byte on disk.
#[derive(Debug)]
pub enum MoveError {
    /// The destination holds content. A move never overwrites.
    Occupied { from: PathBuf, to: PathBuf },
    /// A writer changed the source while it was being copied.
    Changed { from: PathBuf },
    /// The copy is in place and the source could not be removed.
    SourceLeft {
        from: PathBuf,
        source: std::io::Error,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl MoveError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Occupied { from, to } => write!(
                f,
                "both {} and {} hold content",
                from.display(),
                to.display()
            ),
            Self::Changed { from } => write!(f, "{} changed during the copy", from.display()),
            Self::SourceLeft { from, source } => write!(
                f,
                "copied, and {} could not be removed: {source}",
                from.display()
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for MoveError {}

/// Whether anything sits at `path`. A dangling link counts.
pub(crate) fn occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A folder that is not a link to one.
pub(crate) fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Create `dir`, and when it is new give it `like`'s mode, so a fold never
/// widens a private folder's children by re-homing them.
pub(crate) fn ensure_dir_like(like: &Path, dir: &Path) -> Result<(), MoveError> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| MoveError::io(dir, e))?;
    let mode = std::fs::metadata(like).map_err(|e| MoveError::io(like, e))?;
    std::fs::set_permissions(dir, mode.permissions()).map_err(|e| MoveError::io(dir, e))
}

/// Move `from` to `to`, which must be free.
pub(crate) fn move_path(from: &Path, to: &Path) -> Result<(), MoveError> {
    if occupied(to) {
        return Err(MoveError::Occupied {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MoveError::io(parent, e))?;
    }
    match rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => copy_across(from, to),
        Err(e) => Err(MoveError::io(from, e)),
    }
}

fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FORCE_COPY.with(|force| force.get()) {
        return Err(std::io::Error::from_raw_os_error(libc::EXDEV));
    }
    std::fs::rename(from, to)
}

fn copy_across(from: &Path, to: &Path) -> Result<(), MoveError> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name().and_then(|n| n.to_str())) else {
        return Err(MoveError::io(to, std::io::ErrorKind::InvalidInput.into()));
    };
    let prefix = format!(".{name}.moving.");
    discard_dead_temps(parent, &prefix);
    let temp = parent.join(format!("{prefix}{}", std::process::id()));
    if let Err(e) = copy_whole(from, &temp, to) {
        discard(&temp);
        return Err(e);
    }
    let removed = if real_dir(from) {
        std::fs::remove_dir_all(from)
    } else {
        std::fs::remove_file(from)
    };
    removed.map_err(|source| MoveError::SourceLeft {
        from: from.to_path_buf(),
        source,
    })
}

/// The copy, the no-change proof and the publish.
fn copy_whole(from: &Path, temp: &Path, to: &Path) -> Result<(), MoveError> {
    let before = fingerprint(from)?;
    copy_tree(from, temp)?;
    if fingerprint(from)? != before {
        return Err(MoveError::Changed {
            from: from.to_path_buf(),
        });
    }
    if occupied(to) {
        return Err(MoveError::Occupied {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
    }
    std::fs::rename(temp, to).map_err(|e| MoveError::io(to, e))
}

/// What a write to a tree changes.
#[derive(PartialEq, Eq)]
struct Fingerprint {
    entries: u64,
    bytes: u64,
    newest: SystemTime,
}

fn fingerprint(path: &Path) -> Result<Fingerprint, MoveError> {
    let at = |e| MoveError::io(path, e);
    let meta = std::fs::symlink_metadata(path).map_err(at)?;
    let mut out = Fingerprint {
        entries: 1,
        bytes: if meta.is_dir() { 0 } else { meta.len() },
        newest: meta.modified().map_err(at)?,
    };
    if meta.is_dir() {
        for child in std::fs::read_dir(path).map_err(at)? {
            let inner = fingerprint(&child.map_err(at)?.path())?;
            out.entries += inner.entries;
            out.bytes += inner.bytes;
            out.newest = out.newest.max(inner.newest);
        }
    }
    Ok(out)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), MoveError> {
    let meta = std::fs::symlink_metadata(from).map_err(|e| MoveError::io(from, e))?;
    let kind = meta.file_type();
    let write = |e| MoveError::io(to, e);
    if kind.is_symlink() {
        let target = std::fs::read_link(from).map_err(|e| MoveError::io(from, e))?;
        return std::os::unix::fs::symlink(target, to).map_err(write);
    }
    if kind.is_file() {
        std::fs::copy(from, to).map_err(|e| MoveError::io(from, e))?;
        std::fs::set_permissions(to, meta.permissions()).map_err(write)?;
    } else if kind.is_dir() {
        // Private until filled; the source's own mode lands last.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(to)
            .map_err(write)?;
        for child in std::fs::read_dir(from).map_err(|e| MoveError::io(from, e))? {
            let child = child.map_err(|e| MoveError::io(from, e))?;
            copy_tree(&child.path(), &to.join(child.file_name()))?;
        }
    } else {
        // Reading a fifo or a socket never ends, so neither is copied.
        return Err(MoveError::io(from, std::io::ErrorKind::Unsupported.into()));
    }
    keep_mtime(&meta, to);
    if kind.is_dir() {
        std::fs::set_permissions(to, meta.permissions()).map_err(write)?;
    }
    Ok(())
}

fn keep_mtime(meta: &std::fs::Metadata, to: &Path) {
    let kept = meta
        .modified()
        .and_then(|at| std::fs::File::open(to)?.set_modified(at));
    if let Err(e) = kept {
        eprintln!("state-root move: {} lost its mtime ({e})", to.display());
    }
}

/// Remove this mover's own partial copy, whose folders may be read-only.
fn discard(temp: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(temp) else {
        return;
    };
    let removed = if meta.is_dir() {
        // A failed chmod or listing shows up as the failed removal below.
        if crate::paths::set_dir_mode_0700(temp).is_ok() {
            for child in std::fs::read_dir(temp).into_iter().flatten().flatten() {
                discard(&child.path());
            }
        }
        std::fs::remove_dir(temp)
    } else {
        std::fs::remove_file(temp)
    };
    if let Err(e) = removed {
        eprintln!("state-root move: temp {} stays ({e})", temp.display());
    }
}

/// A killed pass leaves its temp. Only a temp whose writer is gone goes.
fn discard_dead_temps(parent: &Path, prefix: &str) {
    for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let dead = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| !crate::pid::Pid::new(pid).is_some_and(|p| p.exists()));
        if dead {
            discard(&entry.path());
        }
    }
}

#[cfg(test)]
thread_local! {
    static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While alive, every move on this thread takes the cross-volume path.
#[cfg(test)]
pub(crate) struct ForceCopy;

#[cfg(test)]
impl ForceCopy {
    pub(crate) fn new() -> Self {
        FORCE_COPY.with(|force| force.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for ForceCopy {
    fn drop(&mut self) {
        FORCE_COPY.with(|force| force.set(false));
    }
}
