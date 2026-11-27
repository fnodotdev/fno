//! The `fno` crate's copy of the state-root layout resolver: the same table
//! (`docs/state-root-layout.tsv`, read at build time) and the same `place`
//! semantics as `fno-agent/src/runtime/state_layout.rs` - the dual-implementation
//! inventory pattern. No migration lives here; the daemon crate owns it.
//!
//! A destination that starts with `cache/` or `logs/` names a class home on
//! the default root ([`home_of`]); every other root keeps both inside itself.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const LAYOUT_TSV: &str = include_str!("../state-root-layout.tsv");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Sqlite,
    Anchor,
    Append,
    Overwrite,
    Marker,
    Page,
    Lock,
    Park,
    Dir,
    Keep,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    Daemon,
    Mux,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub legacy: String,
    pub new: String,
    pub kind: Kind,
    pub owner: Owner,
}

/// Parse the table. Failures name their 1-based line, so a bad row can never
/// ship silently (pinned by unit tests).
pub fn parse_table(tsv: &str) -> Result<Vec<Row>, String> {
    let mut out = Vec::new();
    for (idx, line) in tsv.lines().enumerate() {
        let no = idx + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let bad = |why: &str| -> String {
            format!("state-root layout table line {no}: {why} ({line:?})")
        };
        let [legacy, new, kind, owner] = fields.as_slice() else {
            return Err(bad("expected 4 TAB-separated fields"));
        };
        if legacy.is_empty() || new.is_empty() {
            return Err(bad("empty name"));
        }
        let kind = match Kind::parse(kind) {
            Some(k) => k,
            None => return Err(bad("unknown kind")),
        };
        let owner = match Owner::parse(owner) {
            Some(o) => o,
            None => return Err(bad("unknown owner")),
        };
        out.push(Row {
            legacy: legacy.to_string(),
            new: new.to_string(),
            kind,
            owner,
        });
    }
    if out.is_empty() {
        return Err("state-root layout table: no rows".to_string());
    }
    Ok(out)
}

impl Kind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sqlite" => Self::Sqlite,
            "anchor" => Self::Anchor,
            "append" => Self::Append,
            "overwrite" => Self::Overwrite,
            "marker" => Self::Marker,
            "page" => Self::Page,
            "lock" => Self::Lock,
            "park" => Self::Park,
            "dir" => Self::Dir,
            "keep" => Self::Keep,
            _ => return None,
        })
    }
}

impl Owner {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "daemon" => Self::Daemon,
            "mux" => Self::Mux,
            _ => return None,
        })
    }
}

/// The shipped rows, parsed once. A malformed shipped table panics loud
/// rather than resolving guessed paths.
pub fn rows() -> &'static [Row] {
    static ROWS: OnceLock<&'static [Row]> = OnceLock::new();
    *ROWS.get_or_init(|| {
        let mut parsed = parse_table(LAYOUT_TSV).unwrap_or_else(|e| panic!("{e}"));
        // A keep row names an entry that stays put; no resolver reads one.
        parsed.retain(|r| r.kind != Kind::Keep);
        Box::leak(parsed.into_boxed_slice())
    })
}

pub fn find(legacy_name: &str) -> Option<&'static Row> {
    rows().iter().find(|r| r.legacy == legacy_name)
}

/// Whether `root` is this HOME's default state root.
fn is_default_root(root: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) else {
        return false;
    };
    let default = PathBuf::from(home).join(".fno");
    root == default
        || matches!((root.canonicalize(), default.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// One class home: the env carrier, then `[paths]` config, then the platform
/// default. Mirrors `paths_cli::class_home` in fno-agent, without a spawn: a
/// pin expands `~` and resolves links, a config value also expands variables,
/// and a config value with a glob or a template is no answer.
fn class_home(env_key: &str, config_key: &str, default: fn(&Path) -> PathBuf) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let user = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from);
    if let Some(pin) = crate::digest_overlay::non_empty_env_os(env_key) {
        let pin = crate::config_doctor::expanduser(&pin.to_string_lossy());
        return crate::config_doctor::resolve(Path::new(&pin));
    }
    let configured = crate::digest_overlay::config_str(&cwd, "paths", config_key)
        .filter(|raw| !raw.contains(['*', '?', '[']))
        .map(|raw| crate::config_doctor::expandvars(raw.trim_end_matches('/')))
        .filter(|path| crate::config_cli::model::template_vars(path).is_empty());
    match configured {
        Some(path) => cwd.join(crate::config_doctor::expanduser(&path)),
        None => default(&user),
    }
}

fn default_cache_home(user: &Path) -> PathBuf {
    if cfg!(windows) {
        return crate::digest_overlay::non_empty_env_os("LOCALAPPDATA")
            .map_or_else(|| user.to_path_buf(), PathBuf::from)
            .join("fno")
            .join("cache");
    }
    if cfg!(target_os = "macos") {
        return user.join("Library").join("Caches").join("fno");
    }
    crate::digest_overlay::non_empty_env_os("XDG_CACHE_HOME")
        .map_or_else(|| user.join(".cache"), PathBuf::from)
        .join("fno")
}

fn default_state_home(user: &Path) -> PathBuf {
    crate::digest_overlay::non_empty_env_os("XDG_STATE_HOME")
        .map_or_else(|| user.join(".local").join("state"), PathBuf::from)
        .join("fno")
}

/// A root-relative table destination as a path. Only a class name reads the
/// environment or config.
pub fn home_of(root: &Path, rel: &str) -> PathBuf {
    let (head, rest) = rel.split_once('/').unwrap_or((rel, ""));
    if !matches!(head, "cache" | "logs") || !is_default_root(root) {
        return root.join(rel);
    }
    let base = if head == "cache" {
        class_home("FNO_CACHE_HOME", "cache_dir", default_cache_home)
    } else {
        class_home("FNO_STATE_HOME", "state_dir_local", default_state_home).join("logs")
    };
    if rest.is_empty() {
        base
    } else {
        base.join(rest)
    }
}

/// The read ladder for a root-relative name: its home when that exists, else
/// the spelling inside the root, else the home.
pub fn read_home(root: &Path, rel: &str) -> PathBuf {
    let home = home_of(root, rel);
    let inside = root.join(rel);
    if !home.exists() && inside.exists() {
        inside
    } else {
        home
    }
}

/// Where a writer puts `legacy_name`: its home, always.
pub fn place_new(root: &Path, legacy_name: &str) -> PathBuf {
    find(legacy_name).map_or_else(|| root.join(legacy_name), |row| home_of(root, &row.new))
}

/// The graph store's anchor under `root`, through the layout ladder. The name
/// is frozen: the lock, its queue, the keeper socket and the note journal all
/// derive from it.
pub fn graph_anchor(root: &Path) -> PathBuf {
    place(root, "graph.json")
}

/// The anchor probe: a virtual graph.json exists when its .db sibling does.
fn db_twin(p: &Path) -> PathBuf {
    p.with_extension("db")
}

/// Where `legacy_name` lives under `root`: the new path when it exists, else
/// the legacy path when it exists, else the new path (fresh install). The
/// `anchor` kind probes the .db twin. A name the table does not carry
/// resolves to root/<name> unchanged.
pub fn place(root: &Path, legacy_name: &str) -> PathBuf {
    let Some(row) = find(legacy_name) else {
        return root.join(legacy_name);
    };
    let new = home_of(root, &row.new);
    let legacy = root.join(&row.legacy);
    match row.kind {
        Kind::Anchor => {
            if db_twin(&new).exists() {
                new
            } else if db_twin(&legacy).exists() {
                legacy
            } else {
                new
            }
        }
        _ => {
            let found = read_home(root, &row.new);
            if found.exists() {
                found
            } else if legacy.exists() {
                legacy
            } else {
                new
            }
        }
    }
}

/// Where a ruled folder lives after its fold: the `dir` row's destination,
/// always, and nothing is created. `None` refuses a name the table does not
/// carry as a folder, so a caller never silently lands a new top-level
/// entry. The group slices route their writers through this.
pub fn place_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let row = find(name)?;
    if row.kind != Kind::Dir {
        return None;
    }
    Some(home_of(root, &row.new))
}

/// The one-level-down read: `file` at the new dir when it exists, else at
/// the legacy dir when that differs and exists, else the new path (fresh
/// write lands new). The same ladder `place` runs, one level down, so a
/// straggler a stale writer left at the legacy spelling stays readable
/// until the next daemon-start pass folds it.
pub fn place_in(root: &Path, name: &str, file: &str) -> PathBuf {
    let new = place_dir(root, name).unwrap_or_else(|| root.join(name));
    let new_file = new.join(file);
    if new_file.exists() {
        return new_file;
    }
    if let Some(row) = find(name) {
        if row.kind == Kind::Dir && row.legacy != row.new {
            for dir in [&row.new, &row.legacy] {
                let older = root.join(dir).join(file);
                if older.exists() {
                    return older;
                }
            }
        }
    }
    new_file
}

/// The folder read for listers (cursors, latches, tickets): the children of
/// the new dir, then the legacy dir's children whose names the new side is
/// missing, new wins a clash. Skips both dirs when neither exists.
pub fn list_dir_children(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let new_dir = place_dir(root, name).unwrap_or_else(|| root.join(name));
    let mut seen: Vec<String> = Vec::new();
    let inside = find(name)
        .filter(|row| row.kind == Kind::Dir)
        .map(|row| root.join(&row.new))
        .filter(|dir| *dir != new_dir);
    for dir in [Some(new_dir), inside, legacy_dir_of(root, name)] {
        let Some(dir) = dir else { continue };
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let Some(file_name) = child.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if seen.contains(&file_name) {
                continue;
            }
            seen.push(file_name);
            out.push(child.path());
        }
    }
    out
}

fn legacy_dir_of(root: &Path, name: &str) -> Option<PathBuf> {
    let row = find(name)?;
    if row.kind != Kind::Dir || row.legacy == row.new {
        return None;
    }
    Some(root.join(&row.legacy))
}

/// The mux server moves its `owner = mux` rows at its own start, before it
/// opens them: it is their long-lived writer, and agents never restart the
/// shared server. A client never moves them, so an old-build server keeps
/// writing the root file and new clients read it through the gated fallback
/// until a new-build server starts. Returns how many rows resolved.
pub fn migrate_mux_sidecars_at(root: &Path) -> usize {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = format!("mux-{secs}");
    let mut resolved = 0;
    for row in rows() {
        if row.owner != Owner::Mux {
            continue;
        }
        let legacy = root.join(&row.legacy);
        if !legacy.exists() {
            continue;
        }
        let new = root.join(&row.new);
        let ok = match row.kind {
            Kind::Lock => mux_lock_can_leave(root, row),
            Kind::Overwrite => {
                if new.exists() {
                    if mtime_of(&legacy) > mtime_of(&new) {
                        keep_older(root, &stamp, &new) && move_mux(&legacy, &new)
                    } else {
                        park_mux(root, &stamp, &legacy)
                    }
                } else {
                    move_mux(&legacy, &new)
                }
            }
            _ => continue,
        };
        if ok {
            resolved += 1;
        }
    }
    resolved
}

/// A legacy lock leaves only once its data file sits at the new path (so no
/// waiter still needs it) and a non-blocking flock succeeds (so no holder
/// still owns it).
fn mux_lock_can_leave(root: &Path, row: &Row) -> bool {
    let legacy = root.join(&row.legacy);
    let base = row.legacy.strip_suffix(".lock").unwrap_or(&row.legacy);
    let data_moved = match find(base) {
        Some(base_row) => root.join(&base_row.new).exists(),
        None => false,
    };
    if !data_moved {
        return false;
    }
    match std::fs::File::open(&legacy) {
        Ok(f) => {
            let rc = unsafe {
                libc::flock(
                    std::os::unix::io::AsRawFd::as_raw_fd(&f),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            };
            if rc != 0 {
                return false;
            }
            std::fs::remove_file(&legacy).is_ok()
        }
        Err(_) => false,
    }
}

fn move_mux(legacy: &Path, new: &Path) -> bool {
    if let Some(parent) = new.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::rename(legacy, new).is_ok()
}

/// The run's backup folder, `backups/state-root-migration/mux-<unix secs>/`.
fn park_mux(root: &Path, stamp: &str, legacy: &Path) -> bool {
    let dir = root
        .join("backups")
        .join("state-root-migration")
        .join(stamp);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    match legacy.file_name() {
        Some(name) => std::fs::rename(legacy, dir.join(name)).is_ok(),
        None => false,
    }
}

/// A second name in the stamp for the copy a newer legacy file replaces.
fn keep_older(root: &Path, stamp: &str, new: &Path) -> bool {
    let dir = root
        .join("backups")
        .join("state-root-migration")
        .join(stamp);
    let Some(name) = new.file_name() else {
        return false;
    };
    std::fs::create_dir_all(&dir).is_ok() && std::fs::hard_link(new, dir.join(name)).is_ok()
}

fn mtime_of(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// The gated sidecar resolution the mux stores share: `mux/<file>` when it
/// exists, else the legacy root spelling when allowed and present, else
/// `mux/<file>` (fresh install). The legacy gate is the test-isolation read
/// (`proto::legacy_fallback_allowed`), which `place` cannot see.
pub fn resolve_sidecar(root: &Path, file: &str, legacy_allowed: bool) -> PathBuf {
    let new = root.join("mux").join(file);
    if new.exists() {
        return new;
    }
    let legacy = root.join(file);
    if legacy_allowed && legacy.exists() {
        return legacy;
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mux_migration_moves_the_sidecars() {
        let root = std::env::temp_dir().join(format!("fno-mux-mig-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("squads.json"), b"members").unwrap();
        std::fs::write(root.join("session-names.json"), b"{}").unwrap();
        let moved = migrate_mux_sidecars_at(&root);
        assert!(moved >= 2, "expected the two data rows, moved {moved}");
        assert_eq!(
            std::fs::read(root.join("mux").join("squads.json")).unwrap(),
            b"members"
        );
        assert_eq!(
            std::fs::read(root.join("mux").join("session-names.json")).unwrap(),
            b"{}"
        );
        assert!(!root.join("squads.json").exists());
        assert!(!root.join("session-names.json.root-move").exists());
        assert_eq!(
            place(&root, "squads.json"),
            root.join("mux").join("squads.json")
        );
        std::fs::remove_dir_all(&root).ok();

        // Second branch of the same move: an overlap resolves by mtime, the
        // newer side's bytes win, and the loser leaves the root.
        let root = std::env::temp_dir().join(format!("fno-mux-both-{}", std::process::id()));
        std::fs::create_dir_all(root.join("mux")).unwrap();
        let legacy_path = root.join("squads.json");
        let new_path = root.join("mux").join("squads.json");
        std::fs::write(&legacy_path, b"legacy-newer").unwrap();
        std::fs::write(&new_path, b"new-older").unwrap();
        let older = std::time::SystemTime::UNIX_EPOCH;
        let newer = std::time::SystemTime::now();
        touch_mtime(&legacy_path, newer);
        touch_mtime(&new_path, older);
        let moved = migrate_mux_sidecars_at(&root);
        assert!(moved >= 1, "the overlap row resolved, moved {moved}");
        assert_eq!(
            std::fs::read(&new_path).unwrap(),
            b"legacy-newer",
            "the newer legacy bytes win"
        );
        assert!(!legacy_path.exists());
        std::fs::remove_dir_all(&root).ok();

        // Third branch of the same move: a lock whose data file has not
        // moved stays; it leaves once the data file did.
        let root = std::env::temp_dir().join(format!("fno-mux-lock-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("mux-view.json.lock"), b"").unwrap();
        let moved = migrate_mux_sidecars_at(&root);
        assert_eq!(moved, 0, "the lock waits for its data file");
        assert!(root.join("mux-view.json.lock").exists());
        std::fs::create_dir_all(root.join("mux")).unwrap();
        std::fs::write(root.join("mux").join("mux-view.json"), b"{}").unwrap();
        let moved = migrate_mux_sidecars_at(&root);
        assert!(moved >= 1, "the lock leaves once its data file moved");
        assert!(!root.join("mux-view.json.lock").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sidecar_resolution_matches_the_acceptance() {
        let root = std::env::temp_dir().join(format!("fno-mux-res-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("squads.json"), b"members").unwrap();
        // Unmigrated root, client lane: the legacy file is the store.
        assert_eq!(
            resolve_sidecar(&root, "squads.json", true),
            root.join("squads.json")
        );
        assert!(!root.join("mux").exists(), "nothing lands under mux/");
        // Gate off: even a present legacy file never resolves (test isolation).
        assert_eq!(
            resolve_sidecar(&root, "squads.json", false),
            root.join("mux").join("squads.json")
        );
        // Migrated root: mux/ wins over the parked legacy twin.
        std::fs::create_dir_all(root.join("mux")).unwrap();
        std::fs::write(root.join("mux").join("squads.json"), b"new").unwrap();
        assert_eq!(
            resolve_sidecar(&root, "squads.json", true),
            root.join("mux").join("squads.json")
        );
        // Fresh root: mux/ is the default.
        let fresh = std::env::temp_dir().join(format!("fno-mux-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&fresh).unwrap();
        assert_eq!(
            resolve_sidecar(&fresh, "mux-view.json", true),
            fresh.join("mux").join("mux-view.json")
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&fresh).ok();
    }

    fn touch_mtime(p: &Path, t: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
            .unwrap();
    }
}
