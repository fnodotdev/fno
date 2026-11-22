//! When may a cache or log child leave the state root for its class home?
//!
//! Both passes run for the default root only. A cache child moves or parks
//! only after its legacy path has been quiet, because an older binary can
//! still write there. Writers on this build always write the class home, so
//! the window closes.

use crate::state_layout::{park_file, Status};
use crate::state_layout_home::{cache_dir, is_default_root, logs_dir, same_path};
use crate::state_layout_move::{ensure_dir_like, occupied, real_dir, MoveError};
use crate::state_layout_undo::{carry, record, remove_if_empty, Action, Holds};
use std::path::{Path, PathBuf};

/// How long a legacy cache child must sit unwritten: a live cargo build
/// holds absolute paths into its tree, and an older binary rewrites its own
/// cache files.
const QUIET_SECS: u64 = 6 * 3600;

/// Depth 4 reaches a live build's deps dir; depth 2 read only the profile
/// and hash levels and renamed trees under running builds.
const QUIET_DEPTH: u32 = 4;

/// The wait a recently written path still owes, as a pending status.
pub(crate) fn waits_for_quiet(path: &Path) -> Option<Status> {
    let newest = newest_mtime_within(path, QUIET_DEPTH)?;
    // An mtime ahead of the clock is a fresh write.
    let age = newest.elapsed().map_or(0, |age| age.as_secs());
    (age < QUIET_SECS)
        .then(|| Status::Pending(format!("written {age}s ago, waits for {QUIET_SECS}s quiet")))
}

/// Newest mtime within `depth` levels. A link inside the tree says nothing
/// about a writer, and creating one already touches its parent.
fn newest_mtime_within(path: &Path, depth: u32) -> Option<std::time::SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok();
    if depth > 0 && meta.is_dir() {
        for child in std::fs::read_dir(path).into_iter().flatten().flatten() {
            if child.file_type().is_ok_and(|kind| kind.is_symlink()) {
                continue;
            }
            newest = newest.max(newest_mtime_within(&child.path(), depth - 1));
        }
    }
    newest
}

/// The legacy folder and its home, when this root owes the class a move. A
/// linked folder, or a home the user pinned inside it, owes nothing.
fn class_move(root: &Path, name: &str, home: fn(&Path) -> PathBuf) -> Option<(PathBuf, PathBuf)> {
    let source = root.join(name);
    if !is_default_root(root) || !real_dir(&source) {
        return None;
    }
    let dest = home(root);
    (!same_path(&source, &dest) && !dest.starts_with(&source)).then_some((source, dest))
}

/// Each child of `<root>/cache` into the cache home.
pub(crate) fn cache_pass(
    root: &Path,
    stamp: &str,
    apply: bool,
    holds: &Holds,
) -> Vec<(String, Status)> {
    let Some((source, home)) = class_move(root, "cache", cache_dir) else {
        return Vec::new();
    };
    let children = match std::fs::read_dir(&source) {
        Ok(children) => children,
        Err(e) => {
            let why = MoveError::io(&source, e).to_string();
            return vec![("cache".to_string(), Status::Refused(why))];
        }
    };
    let mut out = Vec::new();
    for child in children.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        let from = child.path();
        let status = if holds.holds(&format!("cache/{name}")) {
            Status::Held
        } else if let Some(wait) = waits_for_quiet(&from) {
            wait
        } else {
            let dest = home.join(child.file_name());
            move_cache_child(root, stamp, &from, &dest, apply)
        };
        out.push((name, status));
    }
    if apply {
        remove_if_empty(root, stamp, &source);
    }
    out
}

/// A free name moves whole, two folders fold one level, anything else parks
/// the legacy copy: the writer is at the home, so the home wins.
fn move_cache_child(root: &Path, stamp: &str, from: &Path, dest: &Path, apply: bool) -> Status {
    let free = !occupied(dest);
    let folds = real_dir(from) && real_dir(dest);
    if !apply {
        let what = if free {
            "move"
        } else if folds {
            "fold"
        } else {
            "park"
        };
        return Status::Pending(format!("would {what} to the cache home"));
    }
    if free {
        match carry(root, stamp, from, dest) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(e.to_string()),
        }
    } else if folds {
        fold_children(root, stamp, from, dest)
    } else {
        park_file(root, stamp, from)
    }
}

/// Children with a free name move in and a clash parks whole. One level
/// only: merging two build trees of one hash would mix their artifacts.
fn fold_children(root: &Path, stamp: &str, from: &Path, dest: &Path) -> Status {
    let children = match std::fs::read_dir(from) {
        Ok(children) => children,
        Err(e) => return Status::Refused(MoveError::io(from, e).to_string()),
    };
    let mut refusal = None;
    for child in children.flatten() {
        let target = dest.join(child.file_name());
        let failed = if occupied(&target) {
            match park_file(root, stamp, &child.path()) {
                Status::Refused(why) => Some(why),
                _ => None,
            }
        } else {
            carry(root, stamp, &child.path(), &target)
                .err()
                .map(|e| e.to_string())
        };
        refusal = refusal.or(failed);
    }
    if let Some(why) = refusal {
        return Status::Refused(why);
    }
    if remove_if_empty(root, stamp, from) {
        Status::Moved
    } else {
        Status::Pending("a child arrived during the fold".to_string())
    }
}

/// `<root>/logs` into the state home, then a link at the legacy spelling.
/// A rename keeps launchd's open descriptors on their inodes, and a plist
/// installed before the move keeps working through the link.
pub(crate) fn logs_pass(
    root: &Path,
    stamp: &str,
    apply: bool,
    holds: &Holds,
) -> Vec<(String, Status)> {
    let Some((source, dest)) = class_move(root, "logs", logs_dir) else {
        return Vec::new();
    };
    let status = if holds.holds("logs") {
        Status::Held
    } else if !apply {
        Status::Pending("would move to the state home".to_string())
    } else {
        move_logs(root, stamp, &source, &dest)
    };
    vec![("logs".to_string(), status)]
}

fn move_logs(root: &Path, stamp: &str, source: &Path, dest: &Path) -> Status {
    let moved = if !occupied(dest) {
        carry(root, stamp, source, dest)
    } else {
        // A new plist wrote the home first, so fold instead of parking whole.
        if let Err(e) = ensure_dir_like(source, dest) {
            return Status::Refused(e.to_string());
        }
        match fold_children(root, stamp, source, dest) {
            Status::Moved => Ok(()),
            other => return other,
        }
    };
    if let Err(e) = moved {
        return Status::Refused(e.to_string());
    }
    if let Err(e) = std::os::unix::fs::symlink(dest, source) {
        return Status::Refused(format!("logs link failed: {e}"));
    }
    record(root, stamp, Action::Linked, source, dest);
    Status::Moved
}
