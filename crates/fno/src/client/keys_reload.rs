//! Reload the keymap when a file it is read from changes on disk.
//!
//! `keys::install` stays first-wins so a re-attach re-read cannot swap the
//! keyboard mid-session. A changed mtime is different: the operator edited
//! the file, which is direct intent, so the poll calls `keys::reinstall`.
//! A reload that raises a NEW warning keeps the last good map live and shows
//! the warning instead. A warning the last good load already carried (an
//! unknown preset, a leftover legacy table) blocks nothing, or one standing
//! notice would freeze every later edit.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::keys::{Keymap, KeymapWarning};

/// How often the client stats the key files. A stat is cheap; two seconds
/// is quick enough that a saved edit reads as live.
const POLL_EVERY: Duration = Duration::from_secs(2);

pub(super) struct KeysWatch {
    /// The mtimes of the files last time, `None` until the first poll sets
    /// the baseline. A missing file stats as `None` in its slot.
    stamp: Option<Vec<Option<SystemTime>>>,
    /// The warnings the last good load carried.
    known: Vec<KeymapWarning>,
    pub(super) next: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reload {
    Unchanged,
    Reloaded(Keymap),
    Kept(KeymapWarning),
}

impl KeysWatch {
    pub(super) fn new(now: Instant) -> Self {
        KeysWatch {
            stamp: None,
            known: Vec::new(),
            next: now + POLL_EVERY,
        }
    }

    /// Stat `files`; on a change since the last poll, run `load` and judge
    /// the result. The first poll only records the baseline: the stamp and
    /// the warnings the map in force was installed with.
    pub(super) fn poll(
        &mut self,
        now: Instant,
        files: &[PathBuf],
        load: impl FnOnce() -> (Keymap, Vec<KeymapWarning>),
    ) -> Reload {
        self.next = now + POLL_EVERY;
        let stamp: Vec<_> = files
            .iter()
            .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
            .collect();
        let Some(old) = self.stamp.replace(stamp.clone()) else {
            self.known = load().1;
            return Reload::Unchanged;
        };
        if old == stamp {
            return Reload::Unchanged;
        }
        let (map, warnings) = load();
        match warnings.iter().find(|w| !self.known.contains(w)) {
            Some(new) => Reload::Kept(new.clone()),
            None => {
                self.known = warnings;
                Reload::Reloaded(map)
            }
        }
    }
}

/// The client loop's hook: poll the live config files and apply the verdict.
/// True when the notice changed and the screen needs a repaint.
pub(super) fn tick(view: &mut super::View, now: Instant) -> bool {
    if now < view.keys_watch.next {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let files = crate::digest_overlay::keymap_sources(&cwd);
    match view
        .keys_watch
        .poll(now, &files, || crate::digest_overlay::keymap(&cwd))
    {
        Reload::Unchanged => false,
        Reload::Reloaded(map) => {
            crate::keys::reinstall(map);
            view.set_notice("keys reloaded from config".into());
            true
        }
        Reload::Kept(warning) => {
            view.set_notice(format!("{} (keeping the last good keys)", warning.0));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_edit_keeps_the_last_good_map_and_names_the_key() {
        // AC3-ERR. The first poll is the baseline, and its digit-prefix
        // warning stands through every later load. An edit that adds a
        // warning is kept out; an edit that adds none reloads.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keybindings.json");
        std::fs::write(&file, "{}").unwrap();
        let files = [file.clone()];
        let now = Instant::now();
        let mut watch = KeysWatch::new(now);
        let base = || crate::keys::resolve_keymap(Some("5"), &[]);
        let bad =
            || crate::keys::resolve_keymap(Some("5"), &[("split-right".into(), "g s".into())]);
        assert_eq!(watch.poll(now, &files, base), Reload::Unchanged);
        let touch = |secs| {
            let f = std::fs::File::options().write(true).open(&file).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        touch(1);
        let Reload::Kept(warning) = watch.poll(now, &files, bad) else {
            panic!("an invalid bind must keep the live map");
        };
        assert!(warning.0.contains("split-right"), "{warning:?}");
        assert_eq!(watch.poll(now, &files, bad), Reload::Unchanged);
        touch(2);
        let good = || crate::keys::resolve_keymap(Some("5"), &[("split-right".into(), "=".into())]);
        assert_eq!(watch.poll(now, &files, good), Reload::Reloaded(good().0));
    }
}
