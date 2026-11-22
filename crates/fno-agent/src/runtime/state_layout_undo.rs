//! How is a state-root move undone, and how does the undo hold?
//!
//! A pass that changes something writes `manifest.json` into its stamp: one
//! row per step. A reverse reads the rows newest first. It records the names
//! it carried home in `state/layout-hold.json`, and every forward pass
//! leaves those names alone until an operator releases them.

use crate::state_layout::{backup_dir, Entry, Receipt, Status, MANIFEST_NAME};
use crate::state_layout_move::{move_path, occupied, MoveError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const HOLD_FILE: &str = "state/layout-hold.json";

/// What one step of a pass did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Moved,
    Parked,
    /// Lines of `from` joined `to`; `from` has its own parked row.
    Merged,
    /// A store left through the backup protocol. It rolls back by hand.
    Published,
    /// A verify record left after the parked copy matched it.
    Verified,
    /// An emptied lock or a page the daemon rebuilds.
    Deleted,
    /// An emptied folder shell.
    EmptiedDir,
    /// `from` became a link to `to`.
    Linked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Step {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub action: Action,
    /// Octal permission bits.
    pub mode: Option<String>,
    pub time: String,
}

fn mode_of(path: &Path) -> Option<u32> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(meta.permissions().mode() & 0o7777)
}

/// Record a step that left something at `to`, which is read for its mode.
pub(crate) fn record(root: &Path, stamp: &str, action: Action, from: &Path, to: &Path) {
    write_step(root, stamp, action, from, Some(to), mode_of(to));
}

fn write_step(
    root: &Path,
    stamp: &str,
    action: Action,
    from: &Path,
    to: Option<&Path>,
    mode: Option<u32>,
) {
    let step = Step {
        from: from.to_path_buf(),
        to: to.map(Path::to_path_buf),
        action,
        mode: mode.map(|bits| format!("{bits:o}")),
        time: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = append_step(&backup_dir(root, stamp), &step) {
        eprintln!(
            "state-root manifest: step for {} was not recorded: {e}",
            from.display()
        );
    }
}

/// Splice one row before the closing bracket. A class fold can carry
/// thousands of rows, so the file is never rewritten whole, and a pass that
/// dies keeps the rows of what it already moved.
fn append_step(dir: &Path, step: &Step) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let row = serde_json::to_string(step)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(MANIFEST_NAME))?;
    file.lock()?;
    let end = file.seek(SeekFrom::End(0))?;
    let text = if end == 0 {
        format!("[\n{row}\n]\n")
    } else {
        file.seek(SeekFrom::Start(end.saturating_sub("\n]\n".len() as u64)))?;
        format!(",\n{row}\n]\n")
    };
    file.write_all(text.as_bytes())
}

/// Move and record.
pub(crate) fn carry(root: &Path, stamp: &str, from: &Path, to: &Path) -> Result<(), MoveError> {
    move_path(from, to)?;
    record(root, stamp, Action::Moved, from, to);
    Ok(())
}

/// Remove an emptied shell and record its mode, so a reverse restores it.
pub(crate) fn remove_shell(root: &Path, stamp: &str, dir: &Path) -> std::io::Result<()> {
    let mode = mode_of(dir);
    std::fs::remove_dir(dir)?;
    write_step(root, stamp, Action::EmptiedDir, dir, None, mode);
    Ok(())
}

/// [`remove_shell`] when `dir` is empty. True when the shell left.
pub(crate) fn remove_if_empty(root: &Path, stamp: &str, dir: &Path) -> bool {
    let empty = std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
    if !empty {
        return false;
    }
    match remove_shell(root, stamp, dir) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("state-root move: shell {} stays ({e})", dir.display());
            false
        }
    }
}

/// Remove a file the law lets a pass delete, and record it.
pub(crate) fn remove_file(root: &Path, stamp: &str, path: &Path) -> std::io::Result<()> {
    let mode = mode_of(path);
    std::fs::remove_file(path)?;
    let verified = path.file_name().is_some_and(|name| name == "verify.tsv");
    let action = if verified {
        Action::Verified
    } else {
        Action::Deleted
    };
    write_step(root, stamp, action, path, None, mode);
    Ok(())
}

/// One pass at a time per root, so a forward pass already running cannot
/// fold what a reverse is carrying home. The lock is on the root itself: a
/// dry run takes none, and no lock file appears.
pub(crate) fn lock_root(root: &Path) -> Option<std::fs::File> {
    let dir = std::fs::File::open(root).ok()?; // an absent root has nothing to race
    match dir.lock() {
        Ok(()) => Some(dir),
        Err(e) => {
            eprintln!("state-root move: {} runs unlocked ({e})", root.display());
            None
        }
    }
}

/// The names a reverse keeps in place.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Holds {
    held: BTreeSet<String>,
}

impl Holds {
    pub(crate) fn load(root: &Path) -> Result<Self, MoveError> {
        let path = root.join(HOLD_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(MoveError::io(&path, e)),
        };
        serde_json::from_str(&text).map_err(|e| MoveError::io(&path, e.into()))
    }

    fn save(&self, root: &Path) -> Result<(), MoveError> {
        let path = root.join(HOLD_FILE);
        let text =
            serde_json::to_string_pretty(self).map_err(|e| MoveError::io(&path, e.into()))?;
        let state = root.join("state");
        std::fs::create_dir_all(&state).map_err(|e| MoveError::io(&state, e))?;
        let temp = state.join(format!(".layout-hold.{}", std::process::id()));
        std::fs::write(&temp, text + "\n").map_err(|e| MoveError::io(&temp, e))?;
        std::fs::rename(&temp, &path).map_err(|e| MoveError::io(&path, e))
    }

    /// A held name also holds what sits below it: `cache` holds `cache/pages`.
    pub(crate) fn holds(&self, name: &str) -> bool {
        self.held.iter().any(|held| {
            name.strip_prefix(held.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    /// Add `names`. Returns the ones that were not held before.
    pub(crate) fn add(
        &mut self,
        root: &Path,
        names: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, MoveError> {
        let mut added = Vec::new();
        for name in names {
            if self.held.insert(name.clone()) {
                added.push(name);
            }
        }
        if !added.is_empty() {
            self.save(root)?;
        }
        Ok(added)
    }

    pub(crate) fn drop_names(&mut self, root: &Path, names: &[String]) -> Result<(), MoveError> {
        for name in names {
            self.held.remove(name);
        }
        self.save(root)
    }
}

/// Clear holds: `state migrate --apply <name>`. Checks every name first.
pub(crate) fn release(root: &Path, names: &[String]) -> Result<(), MoveError> {
    let _pass = lock_root(root);
    let mut holds = Holds::load(root)?;
    let mut cleared = Vec::new();
    for name in names {
        let below = format!("{name}/");
        let matched: Vec<String> = holds
            .held
            .iter()
            .filter(|held| *held == name || held.starts_with(&below))
            .cloned()
            .collect();
        if matched.is_empty() {
            let why = format!("no hold named {name}");
            return Err(MoveError::io(
                &root.join(HOLD_FILE),
                std::io::Error::new(std::io::ErrorKind::NotFound, why),
            ));
        }
        cleared.extend(matched);
    }
    holds.drop_names(root, &cleared)
}

/// The name a forward pass checks before it moves `from` again.
fn hold_name(root: &Path, from: &Path) -> Option<String> {
    let rel = from.strip_prefix(root).ok()?.to_str()?;
    let mut parts = rel.split('/');
    let first = parts.next().filter(|part| !part.is_empty())?;
    match (first, parts.next()) {
        ("logs", _) => return Some(first.to_string()),
        ("cache" | "backups", Some(child)) => return Some(format!("{first}/{child}")),
        _ => {}
    }
    let rel = Path::new(rel);
    let rows = crate::state_layout::rows();
    let by_old = rows.iter().find(|row| rel.starts_with(&row.legacy));
    let row = by_old.or_else(|| {
        rows.iter()
            .filter(|row| rel.starts_with(&row.new))
            .max_by_key(|row| row.new.len())
    });
    Some(row.map_or(first, |row| row.legacy.as_str()).to_string())
}

/// Whether `name` is a stamp that carries a manifest.
pub(crate) fn is_stamp(root: &Path, name: &str) -> bool {
    !name.contains('/')
        && !name.starts_with('.')
        && backup_dir(root, name).join(MANIFEST_NAME).is_file()
}

/// How one recorded step reverses.
enum Undo {
    MoveBack { now: PathBuf, home: PathBuf },
    Unlink { link: PathBuf },
    Shell { dir: PathBuf, mode: Option<u32> },
    Nothing,
    Refuse(String),
}

/// Decide each step, newest first. `vacated` is what the earlier undos free,
/// so a dry run answers what an apply would do.
fn plan(steps: Vec<Step>) -> Vec<(Step, Undo)> {
    let mut vacated: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for step in steps.into_iter().rev() {
        let undo = undo_of(&step, &vacated);
        match &undo {
            Undo::MoveBack { now, .. } => vacated.push(now.clone()),
            Undo::Unlink { link } => vacated.push(link.clone()),
            _ => {}
        }
        out.push((step, undo));
    }
    out
}

fn undo_of(step: &Step, vacated: &[PathBuf]) -> Undo {
    let from = &step.from;
    match (step.action, &step.to) {
        (Action::Moved | Action::Parked, Some(to)) => {
            if !occupied(to) {
                Undo::Refuse(format!("nothing at {}", to.display()))
            } else if occupied(from) && !vacated.iter().any(|v| from.starts_with(v)) {
                let (from, to) = (from.clone(), to.clone());
                Undo::Refuse(MoveError::Occupied { from, to }.to_string())
            } else {
                Undo::MoveBack {
                    now: to.clone(),
                    home: from.clone(),
                }
            }
        }
        (Action::Linked, Some(to)) => {
            if std::fs::read_link(from).is_ok_and(|target| target == *to) {
                Undo::Unlink { link: from.clone() }
            } else {
                Undo::Refuse(format!(
                    "{} no longer links {}",
                    from.display(),
                    to.display()
                ))
            }
        }
        (Action::EmptiedDir, _) => Undo::Shell {
            dir: from.clone(),
            mode: step
                .mode
                .as_deref()
                .and_then(|bits| u32::from_str_radix(bits, 8).ok()),
        },
        (Action::Published, _) => Undo::Refuse(format!(
            "{} left through the store protocol; a store rolls back by hand",
            from.display()
        )),
        (Action::Merged | Action::Verified | Action::Deleted, _) => Undo::Nothing,
        (_, None) => Undo::Refuse("the manifest row names no destination".to_string()),
    }
}

/// `state migrate --reverse <stamp>`: the restore, printed or run.
pub(crate) fn reverse_stamp(
    root: &Path,
    from_stamp: &str,
    stamp: &str,
    apply: bool,
) -> Result<Receipt, MoveError> {
    let path = backup_dir(root, from_stamp).join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).map_err(|e| MoveError::io(&path, e))?;
    let steps: Vec<Step> =
        serde_json::from_str(&text).map_err(|e| MoveError::io(&path, e.into()))?;
    let planned = plan(steps);
    if apply {
        // The hold lands before the first move.
        let names: Vec<String> = planned
            .iter()
            .filter(|(_, undo)| matches!(undo, Undo::MoveBack { .. } | Undo::Unlink { .. }))
            .filter_map(|(step, _)| hold_name(root, &step.from))
            .collect();
        Holds::load(root)?.add(root, names)?;
    }
    let mut receipt = Receipt::default();
    for (step, undo) in planned {
        let shown = step.from.strip_prefix(root).unwrap_or(&step.from);
        receipt.entries.push(Entry {
            legacy: shown.display().to_string(),
            status: run(root, stamp, undo, apply),
        });
    }
    Ok(receipt)
}

fn run(root: &Path, stamp: &str, undo: Undo, apply: bool) -> Status {
    let done = match undo {
        Undo::Nothing => return Status::Absent,
        Undo::Refuse(why) => return Status::Refused(why),
        Undo::MoveBack { now, home } if !apply => {
            return Status::Pending(format!(
                "would restore {} from {}",
                home.display(),
                now.display()
            ))
        }
        Undo::Unlink { link } if !apply => {
            return Status::Pending(format!("would remove the link {}", link.display()))
        }
        Undo::Shell { .. } if !apply => return Status::Absent,
        Undo::MoveBack { now, home } => carry(root, stamp, &now, &home),
        Undo::Unlink { link } => {
            remove_file(root, stamp, &link).map_err(|e| MoveError::io(&link, e))
        }
        Undo::Shell { dir, .. } if occupied(&dir) => return Status::Absent,
        Undo::Shell { dir, mode } => restore_shell(&dir, mode),
    };
    match done {
        Ok(()) => Status::Moved,
        Err(e) => Status::Refused(e.to_string()),
    }
}

fn restore_shell(dir: &Path, mode: Option<u32>) -> Result<(), MoveError> {
    std::fs::create_dir_all(dir).map_err(|e| MoveError::io(dir, e))?;
    let Some(bits) = mode else {
        return Ok(());
    };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits))
        .map_err(|e| MoveError::io(dir, e))
}
