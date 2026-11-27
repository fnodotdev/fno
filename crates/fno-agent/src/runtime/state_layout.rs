//! The state-root layout: one table (`docs/state-root-layout.tsv`, read at
//! build time) that names where every movable top-level file of the state
//! root lives after the migration, plus the one resolver ([`place`]) and the
//! one mover ([`migrate`]). The tidiness law: nothing writes at the top level
//! of the state root when a named subfolder can hold it.
//!
//! `place` is read-old-as-fallback: a migrated root reads the new path, an
//! unmigrated root keeps reading the legacy one, a fresh install gets the
//! new path. Nothing here ever deletes data: the lawful deletions are an
//! emptied lock after its data moved, an emptied shell, and a page the
//! daemon rebuilds. Every conflict parks into `backups/state-root-migration/`.
//! A dry run writes nothing. An apply records each step in its stamp's
//! `manifest.json`, and a reverse holds (`state_layout_undo`).
//! The `sqlite` kind moves through a backup-API protocol in a later wave;
//! until that lands the migrate pass reports those rows pending.
//!
//! The `fno` crate carries its own copy of the table parse and `place`
//! (`crates/fno/src/setup/state_layout.rs`), same file, same dialect - the
//! dual-implementation inventory pattern (one TSV, two readers, no third
//! mechanism).

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::state_layout_move::{ensure_dir_like, move_path, occupied};
use crate::state_layout_undo::{Action, Holds};

pub use crate::state_layout_home::{cache_dir, home_of, logs_dir};

/// The step record an apply writes into its stamp.
pub(crate) const MANIFEST_NAME: &str = "manifest.json";

/// The shipped layout table, vendored beside this module: a `cargo package`
/// tarball carries only crate-root files, so the repo's `docs/` copy cannot
/// feed `include_str!` in a packaged build (the publish dry-run proved it).
/// The repo file is the one EDIT source; the drift test pins this copy
/// byte-identical to it, so a table edit that skips the copies fails CI.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    Daemon,
    Mux,
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

#[derive(Clone, Debug)]
pub struct Row {
    pub legacy: String,
    pub new: String,
    pub kind: Kind,
    pub owner: Owner,
}

/// Parse the table. Every failure names its 1-based line so a bad row can
/// never ship silently (the unit tests pin both failure shapes).
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
        let Some(kind) = Kind::parse(kind) else {
            return Err(bad("unknown kind"));
        };
        let Some(owner) = Owner::parse(owner) else {
            return Err(bad("unknown owner"));
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

/// The shipped rows, parsed once. A malformed shipped table panics loud
/// rather than resolving guessed paths; the unit tests keep that unreachable.
fn all_rows() -> &'static [Row] {
    static ROWS: OnceLock<&'static [Row]> = OnceLock::new();
    *ROWS.get_or_init(|| {
        let parsed = parse_table(LAYOUT_TSV).unwrap_or_else(|e| panic!("{e}"));
        Box::leak(parsed.into_boxed_slice())
    })
}

/// The rows the resolvers and the mover read. A keep row names a top-level
/// entry that stays where it is, so neither ever sees one.
pub fn rows() -> &'static [Row] {
    static MOVING: OnceLock<&'static [Row]> = OnceLock::new();
    *MOVING.get_or_init(|| {
        let moving: Vec<Row> = all_rows()
            .iter()
            .filter(|r| r.kind != Kind::Keep)
            .cloned()
            .collect();
        Box::leak(moving.into_boxed_slice())
    })
}

/// Whether `name` may sit at the top level of a state root: a keep row
/// carries it, by name or by glob. This is the one closed set.
pub fn is_kept(name: &str) -> bool {
    all_rows()
        .iter()
        .filter(|r| r.kind == Kind::Keep)
        .any(|r| crate::state_root_drift::glob_match(&r.legacy, name))
}

/// Whether any row carries `name` at the top level: kept, or owed a move or
/// a park. A name this answers false for is one fno never declared.
pub fn is_carried(name: &str) -> bool {
    is_kept(name)
        || rows().iter().any(|r| {
            crate::state_root_drift::glob_match(&r.legacy, name)
                || r.new.split('/').next() == Some(name)
        })
}

pub fn find(legacy_name: &str) -> Option<&'static Row> {
    rows().iter().find(|r| r.legacy == legacy_name)
}

/// Fold candidates whose children can be held by an open flock. The mover
/// will not fold one while a regular-file child's lock is held; the row
/// reports pending and the next daemon-start pass retries.
const LOCK_BEARING_FOLDERS: &[&str] = &[
    "bus",
    "claim-aux",
    "flight",
    "fleet",
    "locks",
    "mail-hold",
    "migrations",
];

/// Fold candidates that drain instead of folding. A live reader may retain a
/// child's absolute path, so the mover never renames it. New writes use the
/// row's destination; cargo-build residue, plugin-stage swap debris and a
/// route-settings file no writer has touched past the age limit have safe
/// park rules, while live entries stay until their owner drains them. A
/// `screens` shot stays until no question row names it.
const DRAIN_ONLY_FOLDERS: &[&str] = &["cargo-build", "plugin-stage", "route-settings", "screens"];
pub(crate) const LEGACY_DRAIN_AGE_SECS: u64 = 3 * 24 * 60 * 60;

/// Fold candidates that hold message or journal lines. A clashing child never
/// parks here: it merges (`state_layout_merge`), and a destination that does
/// not exist yet takes the whole folder in one rename.
const MERGE_FOLDERS: &[&str] = &["bus", "chats", "history"];

/// Children a fold leaves at the legacy path, with the holder a pending
/// reading names. The sealed eval worker's Claude login is a Keychain item
/// whose service name hashes the config folder's path, so a moved folder
/// reads a different item and the login is lost.
const PINNED_CHILDREN: &[(&str, &str, &str)] = &[(
    "evals",
    "claude-config",
    "the sealed eval login is keyed by evals/claude-config's path",
)];

/// True while the setup wizard's lock names a live process: the wizard
/// holds its report and answers paths open for the whole run.
fn wizard_lock_live(sidecar: &Path) -> bool {
    std::fs::read_to_string(sidecar.join("wizard-lock"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .is_some_and(crate::evals_arm::pid_alive)
}

/// The anchor's file name. Frozen: the lock, its queue, the keeper socket and
/// the note journal all derive from it, so an older binary and a newer one
/// share them. A message a person reads names the `.db` store instead.
const GRAPH_ANCHOR: &str = "graph.json";

/// The graph store's anchor under `root`, through the layout ladder. Every
/// production resolver calls this, so the literal lives in one place.
pub fn graph_anchor(root: &Path) -> PathBuf {
    place(root, GRAPH_ANCHOR)
}

/// The anchor probe: a `graph.json`-shaped path exists when its `.db`
/// sibling does - the journal anchor is virtual, the store is the file.
fn db_twin(p: &Path) -> PathBuf {
    p.with_extension("db")
}

/// Where `legacy_name` lives under `root`: the new path when it exists, else
/// the legacy path when it exists, else the new path (fresh install). A
/// `cache/` or `logs/` destination also reads its spelling inside the root.
/// The
/// `anchor` kind probes the `.db` twin instead of the virtual json. A name
/// the table does not carry resolves to `root/<name>` unchanged.
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
            let found = crate::state_layout_home::read_home(root, &row.new);
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

/// Where a writer puts `legacy_name`: its home, always. A writer that took
/// [`place`]'s fallback would refresh the legacy copy on every write.
pub fn place_new(root: &Path, legacy_name: &str) -> PathBuf {
    find(legacy_name).map_or_else(|| root.join(legacy_name), |row| home_of(root, &row.new))
}

/// The home of a page named by a legacy spelling: right under `root`
/// (`<root>/graph.html`) or in the root's own cache folder
/// (`<root>/cache/pages/graph.html`). A render target that still names one
/// writes the home instead, so no writer re-creates a file the migration
/// must clear, and no config needs an edit.
pub fn page_home(root: &Path, path: &Path) -> Option<PathBuf> {
    let name = path.strip_prefix(root).ok()?.to_str()?;
    let row = rows().iter().find(|row| {
        row.kind == Kind::Page && row.new != row.legacy && (name == row.legacy || name == row.new)
    })?;
    Some(home_of(root, &row.new))
}

/// Where a ruled folder lives after its fold: the `dir` row's destination,
/// always, and nothing is created. `None` refuses a name the table does not
/// carry as a folder, so a caller never silently lands a new top-level
/// entry. Slices 4 to 8 route their writers through this.
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

/// The write path of a cache file that is read and rewritten in place (a
/// cursor file): the home, seeded once from a legacy copy.
pub(crate) fn adopt(root: &Path, name: &str, file: &str) -> PathBuf {
    let home = place_dir(root, name)
        .unwrap_or_else(|| root.join(name))
        .join(file);
    let found = place_in(root, name, file);
    if found != home {
        let seeded = std::fs::read_to_string(&found)
            .map_err(|e| e.to_string())
            .and_then(|text| crate::rundown::write_atomic(&home, &text));
        if let Err(e) = seeded {
            eprintln!("state-root: {} starts empty ({e})", home.display());
        }
    }
    home
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

// ── The migration ────────────────────────────────────────────────────────

/// One entry's outcome. `absent` = nothing at the legacy path and the new
/// path already in place or equally empty: no work now, none owed, and it
/// does not count toward the pending total. `deleted` = the two lawful
/// deletions: an emptied lock, and a stale copy of a page whose home already
/// holds a render. Everything else is the plan's vocabulary.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Moved,
    Merged,
    Parked,
    Deleted,
    Absent,
    /// A reverse put it back and recorded a hold. A forward pass leaves it.
    Held,
    Pending(String),
    Refused(String),
}

impl Status {
    fn word(&self) -> &'static str {
        match self {
            Status::Moved => "moved",
            Status::Merged => "merged",
            Status::Parked => "parked",
            Status::Deleted => "deleted",
            Status::Absent => "absent",
            Status::Held => "held",
            Status::Pending(_) => "pending",
            Status::Refused(_) => "refused",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Status::Pending(d) | Status::Refused(d) => Some(d),
            _ => None,
        }
    }

    fn is_pending(&self) -> bool {
        matches!(self, Status::Pending(_))
    }
}

#[derive(Debug)]
pub struct Entry {
    pub legacy: String,
    pub status: Status,
}

#[derive(Debug, Default)]
pub struct Receipt {
    pub entries: Vec<Entry>,
}

impl Receipt {
    /// Entries still owed work: a dry run of movable rows, a later-wave kind,
    /// a mux-owned row waiting for its server, a lock waiting for its data
    /// file. The close probe reads this: `.pending == 0`.
    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status.is_pending())
            .count()
    }

    pub fn refused_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, Status::Refused(_)))
            .count()
    }

    fn count_matching(&self, pred: fn(&Status) -> bool) -> usize {
        self.entries.iter().filter(|e| pred(&e.status)).count()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} moved, {} merged, {} parked, {} deleted, {} pending, {} refused, {} held, {} absent",
            self.count_matching(|s| matches!(s, Status::Moved)),
            self.count_matching(|s| matches!(s, Status::Merged)),
            self.count_matching(|s| matches!(s, Status::Parked)),
            self.count_matching(|s| matches!(s, Status::Deleted)),
            self.pending_count(),
            self.refused_count(),
            self.count_matching(|s| matches!(s, Status::Held)),
            self.count_matching(|s| matches!(s, Status::Absent)),
        )
    }

    /// The reclaim note: the counts, then which entries wait and why. A
    /// count alone cannot say which folder sat refused for a month.
    pub fn note(&self) -> String {
        const SHOWN: usize = 12;
        let mut note = self.summary();
        for word in ["pending", "refused", "held"] {
            let named: Vec<String> = self
                .entries
                .iter()
                .filter(|e| e.status.word() == word)
                .map(|e| match e.status.detail() {
                    Some(why) => format!("{} ({why})", e.legacy),
                    None => e.legacy.clone(),
                })
                .collect();
            if named.is_empty() {
                continue;
            }
            let shown: Vec<&str> = named.iter().take(SHOWN).map(String::as_str).collect();
            let shown = shown.join(", ");
            note.push_str(&format!("; {word}: {shown}"));
            if named.len() > SHOWN {
                note.push_str(&format!(", and {} more", named.len() - SHOWN));
            }
        }
        note
    }

    fn extend(&mut self, lane: &str, found: Vec<(String, Status)>) {
        self.entries
            .extend(found.into_iter().map(|(name, status)| Entry {
                legacy: format!("{name} ({lane})"),
                status,
            }));
    }
}

fn holds_unreadable(error: &crate::state_layout_move::MoveError) -> Entry {
    Entry {
        legacy: "layout-hold.json".to_string(),
        status: Status::Refused(format!("unreadable, so nothing moves: {error}")),
    }
}

/// One migration pass over the whole table against one state root.
/// Dry run (default) reports every owed row `pending` and writes nothing;
/// `apply` moves, merges, parks and deletes per kind. A held name stays.
/// Idempotent: a second apply over a migrated root moves nothing and writes
/// no stamp. The class homes resolve through `fno_homes`, the same leg the
/// front door reads, so no pre-check holds the pass back on a setting one
/// binary would answer differently.
pub fn migrate(root: &Path, apply: bool) -> Receipt {
    let _pass = if apply {
        crate::state_layout_undo::lock_root(root)
    } else {
        None
    };
    let mut receipt = Receipt::default();
    let holds = match Holds::load(root) {
        Ok(holds) => holds,
        Err(e) => {
            receipt.entries.push(holds_unreadable(&e));
            return receipt;
        }
    };
    let stamp = fresh_stamp(root);
    // The sqlite family's deferred verify: stamps parked on earlier passes
    // get their parked copies compared against their recorded counts here.
    receipt.extend(
        "verify",
        crate::state_layout_sqlite::verify_sweep(root, &stamp, apply),
    );
    // A stale writer's reappeared legacy event-family store imports into the
    // published store, then parks. Dry runs skip it.
    receipt.extend(
        "straggler",
        crate::state_layout_sqlite::straggler_import(root, &stamp, apply, &holds),
    );
    // The retired root journal family: a reappeared `events.jsonl*` imports
    // once and parks whole, and the receipt row carries `repeat: true`.
    receipt.extend(
        "journal",
        crate::state_layout_sqlite::journal_retire(root, &stamp, apply, &holds),
    );
    // The ledger history imports once into the store: a pass ahead of the
    // carrier's deployment refuses by name, an imported root is silent.
    receipt.extend(
        "run-history",
        crate::run_import::ledger_history_import(root, &stamp, apply),
    );
    receipt
        .entries
        .extend(migrate_rows(root, rows(), apply, &stamp));
    receipt
        .entries
        .extend(park_nested_duplicates(root, apply, &stamp, &holds));
    receipt.extend(
        "cache-class",
        crate::state_layout_class::cache_pass(root, &stamp, apply, &holds),
    );
    receipt.extend(
        "logs-class",
        crate::state_layout_class::logs_pass(root, &stamp, apply, &holds),
    );
    receipt
}

/// The per-row pass over any table: the shipped rows for `migrate`, a
/// synthetic table for the tests. Glob and plain park rows need the state
/// root's own child scan; everything else dispatches on kind.
fn migrate_rows(root: &Path, table: &[Row], apply: bool, stamp: &str) -> Vec<Entry> {
    let holds = match Holds::load(root) {
        Ok(holds) => holds,
        Err(e) => return vec![holds_unreadable(&e)],
    };
    let mut out = Vec::new();
    for row in table {
        if row_held(root, row, &holds) {
            out.push(Entry {
                legacy: row.legacy.to_string(),
                status: Status::Held,
            });
            continue;
        }
        match row.kind {
            // Glob rows: every root entry the pattern matches parks.
            Kind::Park if row.legacy.contains('*') => {
                let names = glob_matches(root, &row.legacy);
                if names.is_empty() {
                    continue;
                }
                // The mux server owns its residue's timing (change 3.1); the
                // daemon never parks mux rows, glob or not.
                if row.owner == Owner::Mux {
                    out.push(Entry {
                        legacy: row.legacy.to_string(),
                        status: Status::Pending(
                            "owner mux (the mux server parks it at its start)".to_string(),
                        ),
                    });
                    continue;
                }
                for name in names {
                    let status = if holds.holds(&name) {
                        Status::Held
                    } else if apply {
                        park_entry(root, &stamp, &name)
                    } else {
                        Status::Pending("park (dry run)".to_string())
                    };
                    out.push(Entry {
                        legacy: name,
                        status,
                    });
                }
            }
            Kind::Park => {
                let status = if !root.join(&row.legacy).exists() {
                    Status::Absent
                } else if apply {
                    park_entry(root, &stamp, &row.legacy)
                } else {
                    Status::Pending("park (dry run)".to_string())
                };
                out.push(Entry {
                    legacy: row.legacy.to_string(),
                    status,
                });
            }
            _ => {
                let status = migrate_entry(root, row, apply, stamp);
                out.push(Entry {
                    legacy: row.legacy.to_string(),
                    status,
                });
            }
        }
    }
    out
}

/// A held row, or a lock or sidecar whose store is held, with work at the
/// legacy path.
fn row_held(root: &Path, row: &Row, holds: &Holds) -> bool {
    let base = [".lock", "-wal", "-shm"]
        .iter()
        .find_map(|suffix| row.legacy.strip_suffix(suffix));
    let held = holds.holds(&row.legacy) || base.is_some_and(|base| holds.holds(base));
    held && occupied(&root.join(&row.legacy))
}

fn migrate_entry(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    if row.kind == Kind::Lock {
        return migrate_lock(root, row, apply, stamp);
    }
    if row.kind == Kind::Dir {
        return migrate_dir(root, row, apply, stamp);
    }
    let legacy = root.join(&row.legacy);
    let new = home_of(root, &row.new);
    let legacy_exists = legacy.exists();
    let new_exists = new.exists();
    if !legacy_exists && !new_exists {
        return Status::Absent;
    }
    // Already migrated: the new path holds the file, nothing at the legacy
    // path owes a move.
    if !legacy_exists && new_exists {
        return Status::Moved;
    }
    // Skipped families. Their legacy file EXISTS here, so the work is real
    // and reads pending: the mux server moves its own rows at its start
    // (change 3.1).
    if row.owner == Owner::Mux {
        return Status::Pending("owner mux (the mux server moves it at its start)".to_string());
    }
    if row.kind == Kind::Sqlite {
        return crate::state_layout_sqlite::migrate_sqlite_row(root, row, apply, stamp);
    }
    if !apply {
        return Status::Pending(match row.kind {
            Kind::Append if legacy_exists && new_exists => "append merge (dry run)".to_string(),
            _ => "move (dry run)".to_string(),
        });
    }
    match row.kind {
        // New wins; the legacy copy parks.
        Kind::Marker if legacy_exists && new_exists => park_file(root, stamp, &legacy),
        // A page is a render the daemon rebuilds, so the stale copy goes.
        // Parking it filled the backups pile with one page per pass.
        Kind::Page if legacy_exists && new_exists => drop_page(root, stamp, &legacy),
        Kind::Overwrite if legacy_exists && new_exists => {
            let legacy_newer = mtime_of(&legacy) > mtime_of(&new);
            if legacy_newer {
                replace_older(root, stamp, &legacy, &new)
            } else {
                park_file(root, stamp, &legacy)
            }
        }
        // Rename keeps open writers on the same inode (launchd fds), so an
        // append merge must copy bytes, never re-open through the old name.
        Kind::Append if legacy_exists && new_exists => match append_merge(&legacy, &new) {
            Ok(()) => match park_file(root, stamp, &legacy) {
                Status::Parked => {
                    crate::state_layout_undo::record(root, stamp, Action::Merged, &legacy, &new);
                    Status::Merged
                }
                other => other,
            },
            Err(e) => Status::Refused(format!("merge failed: {e}")),
        },
        _ => match crate::state_layout_undo::carry(root, stamp, &legacy, &new) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(format!("refused: {e}")),
        },
    }
}

/// A legacy lock leaves only after its data file moved (so no waiter still
/// needs it) and a non-blocking flock succeeds (so no holder still owns it).
/// A socket never flocks; it parks once its store moved.
///
/// "Moved" is kind-aware. An `anchor` base (`graph.json.lock` waits on
/// `graph.json`) is virtual, so its probe is the `.db` twin at the new
/// location - the same probe `place` uses. Otherwise the data has moved when
/// the new path exists OR the legacy base is gone: a lock whose data file no
/// longer exists at either spelling protects nothing, and the flock below
/// still refuses while a live holder owns it. A base the table does not
/// carry (`graph-archive.json.store.sock`) reads the same way.
fn migrate_lock(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    let legacy = root.join(&row.legacy);
    if !legacy.exists() {
        return Status::Absent;
    }
    let base = row.legacy.strip_suffix(".lock").unwrap_or(&row.legacy);
    let legacy_base_exists = root.join(base).exists();
    let data_moved = match find(base) {
        Some(base_row) if base_row.kind == Kind::Anchor => {
            db_twin(&root.join(&base_row.new)).exists()
        }
        Some(base_row) => root.join(&base_row.new).exists() || !legacy_base_exists,
        None => !legacy_base_exists,
    };
    if !data_moved {
        return Status::Pending(format!("waiting for {base} to move"));
    }
    if !apply {
        return Status::Pending("lock removal (dry run)".to_string());
    }
    let is_socket = std::fs::symlink_metadata(&legacy)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false);
    if is_socket {
        return park_file(root, stamp, &legacy);
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
                return Status::Pending("lock held".to_string());
            }
            match crate::state_layout_undo::remove_file(root, stamp, &legacy) {
                Ok(()) => Status::Deleted,
                Err(e) => Status::Refused(format!("refused: {e}")),
            }
        }
        Err(e) => Status::Refused(format!("refused: {e}")),
    }
}

/// One `dir` row's fold: each child of the legacy folder renames into the
/// new one when the name is free, a clashing child parks (the new side
/// wins), and the legacy folder is removed only once it is empty. A
/// self-mapped row (the shipped state until a group slice retargets it)
/// declares the folder and owes nothing. Runs again on every daemon start,
/// so a straggler a stale writer drops mid-pass folds in next time.
fn migrate_dir(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    if row.legacy == row.new {
        return Status::Absent;
    }
    if row.owner == Owner::Mux {
        return Status::Pending("owner mux (the mux server folds it at its start)".to_string());
    }
    let legacy = home_of(root, &row.legacy);
    let new = home_of(root, &row.new);
    if !legacy.exists() {
        return if new.exists() {
            Status::Moved
        } else {
            Status::Absent
        };
    }
    // A drain row never moves a child: a recorded absolute path a registry
    // row still names must stay put until its prune. Only the emptied shell
    // goes.
    if DRAIN_ONLY_FOLDERS.contains(&row.legacy.as_str()) {
        if matches!(
            row.legacy.as_str(),
            "cargo-build" | "plugin-stage" | "route-settings"
        ) {
            let remaining = drain_legacy_children(root, &legacy, stamp, apply);
            if !remaining.is_empty() {
                return Status::Pending(format!(
                    "drains: {} child path(s) remain",
                    remaining.len()
                ));
            }
        } else {
            let emptied = std::fs::read_dir(&legacy)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
            if !emptied {
                return Status::Pending("drains: recorded paths stay until pruned".to_string());
            }
        }
        if !apply {
            return Status::Pending("fold (dry run)".to_string());
        }
        let emptied = std::fs::read_dir(&legacy)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !emptied {
            return Status::Pending("drains: recorded paths stay until pruned".to_string());
        }
        return match crate::state_layout_undo::remove_shell(root, stamp, &legacy) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(format!("refused: {e}")),
        };
    }
    // The folder's own name, whichever way the row points: `--reverse` swaps
    // the ends and the same rules still apply.
    let name = legacy
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&row.legacy);
    let merges = MERGE_FOLDERS.contains(&name);
    let mut held_locks: Vec<std::fs::File> = Vec::new();
    if LOCK_BEARING_FOLDERS.contains(&name) && !dir_children_lockable(&legacy, &mut held_locks) {
        return Status::Pending("a child lock is held".to_string());
    }
    // A claim-aux lane dir belongs to a live waiter that polls its path, and
    // every claim-aux child is a dir, so the lock probe never sees a ticket.
    // The fold waits until every child dir is empty; the next daemon-start
    // pass retries.
    if name == "claim-aux" && !lane_dirs_empty(&legacy) {
        return Status::Pending("a waiter holds a ticket".to_string());
    }
    if name == "sidecar" && wizard_lock_live(&legacy) {
        return Status::Pending("wizard-lock names a live setup run".to_string());
    }
    // The probe fds stay open through the fold: a lock another writer takes
    // between the probe and a rename cannot split a child from its holder.
    // The vec drops when the fold ends and releases them.
    if !apply {
        return Status::Pending("fold (dry run)".to_string());
    }
    // claim-aux, locks and the message folders fold whole: a directory rename is atomic, so no
    // waiter's ticket path (claim-aux) or held lock inode (locks) ever moves
    // mid-write - the kernel carries every child to its new name in one
    // step, and a taker that already opened the file keeps it. The emptiness
    // probe re-runs right before the rename (claim-aux only; locks' guard
    // is the flock sweep above), and any lane holding a ticket skips the
    // row for this pass with place() still answering the legacy dir. Only
    // the destination's parent is created first; the rename carries every
    // child in one step.
    if name == "claim-aux" || name == "locks" || merges {
        if name == "claim-aux" && !lane_dirs_empty(&legacy) {
            return Status::Pending("a waiter holds a ticket".to_string());
        }
        if !new.exists() {
            return match crate::state_layout_undo::carry(root, stamp, &legacy, &new) {
                Ok(()) => Status::Moved,
                Err(e) => Status::Refused(format!("refused: {e}")),
            };
        }
        // Both folders exist: a straddler recreated the legacy spelling
        // after the fold. The whole-dir rename cannot run (the destination
        // is taken), so fall through to the per-child fold: its flock probe
        // covers the legacy children, and its emptied-folder removal drains
        // the residue the whole-dir rename cannot touch.
        if merges {
            return match crate::state_layout_merge::merge_tree(root, stamp, &legacy, &new) {
                Ok(true) => Status::Merged,
                Ok(false) => Status::Pending("a destination lock is held".to_string()),
                Err(e) => Status::Refused(e),
            };
        }
    }
    fold_dir_children(root, row, stamp, &legacy, &new)
}

/// The per-child fold. A clash parks, but a cache clash first waits until
/// the legacy copy's writer has been quiet.
fn fold_dir_children(root: &Path, row: &Row, stamp: &str, legacy: &Path, new: &Path) -> Status {
    if let Err(e) = ensure_dir_like(legacy, new) {
        return Status::Refused(format!("refused: {e}"));
    }
    let children = match std::fs::read_dir(legacy) {
        Ok(children) => children,
        Err(e) => return Status::Refused(format!("refused: {e}")),
    };
    let cache = row.new.starts_with("cache/") && crate::state_layout_home::is_default_root(root);
    let (mut moved, mut waiting, mut refused) = (false, None, None);
    for child in children.flatten() {
        let from = child.path();
        let target = new.join(child.file_name());
        let pinned = PINNED_CHILDREN
            .iter()
            .find(|(folder, kid, _)| row.legacy == *folder && child.file_name() == *kid);
        let status = if let Some((_, _, holder)) = pinned {
            Status::Pending((*holder).to_string())
        } else if !occupied(&target) {
            match crate::state_layout_undo::carry(root, stamp, &from, &target) {
                Ok(()) => Status::Moved,
                Err(e) => Status::Refused(format!("refused: {e}")),
            }
        } else if is_page_home(root, &target) {
            drop_page(root, stamp, &from)
        } else if let Some(wait) = cache
            .then(|| crate::state_layout_class::waits_for_quiet(&from))
            .flatten()
        {
            wait
        } else {
            park_file(root, stamp, &from)
        };
        match status {
            Status::Pending(_) => waiting = Some(status),
            Status::Refused(_) => refused = Some(status),
            _ => moved = true,
        }
    }
    crate::state_layout_undo::remove_if_empty(root, stamp, legacy);
    refused
        .or(waiting)
        .unwrap_or(if moved { Status::Moved } else { Status::Absent })
}

/// Whether `path` is the home a page row names.
fn is_page_home(root: &Path, path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(find)
        .is_some_and(|row| row.kind == Kind::Page && home_of(root, &row.new) == path)
}

/// Remove a stale page copy whose home already holds a render.
fn drop_page(root: &Path, stamp: &str, legacy: &Path) -> Status {
    match crate::state_layout_undo::remove_file(root, stamp, legacy) {
        Ok(()) => Status::Deleted,
        Err(e) => Status::Refused(format!("refused: {e}")),
    }
}

/// Every regular-file child of `dir` takes a non-blocking exclusive flock,
/// and each taken `File` lands in `held` so the caller keeps the locks
/// until its fold ends. Subdirs and sockets are neither probes nor locks:
/// their writers fold by rename together with the folder, and an fd that
/// cannot open refuses the fold for this pass rather than guessing.
fn dir_children_lockable(dir: &Path, held: &mut Vec<std::fs::File>) -> bool {
    let Ok(children) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in children.flatten() {
        let is_file = child.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        match std::fs::OpenOptions::new().read(true).open(child.path()) {
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
                held.push(f);
            }
            Err(_) => return false,
        }
    }
    true
}

/// Whether every child dir of `dir` is empty: the `claim-aux` fold guard.
/// Tickets live one level down (`<lane>/<NNNNNN>/holder`), a depth the lock
/// probe's file-only walk never reaches.
fn lane_dirs_empty(dir: &Path) -> bool {
    let Ok(children) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in children.flatten() {
        if !child.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let empty = std::fs::read_dir(child.path())
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty {
            return false;
        }
    }
    true
}

/// The dead duplicate the table's root-level park rows cannot reach: the
/// pre-dedup `graph.db.*` rotation copies inside `backups/` (the one home
/// is `db/backups`). Root scan only sees the root's own children, so this
/// runs beside it. `agents/migrations` is deliberately absent: the role
/// migration writes that folder live (its receipt path derives from the
/// agents home's parent), so folding it into `migrations/` fights the
/// writer and re-fills every pass.
fn park_nested_duplicates(root: &Path, apply: bool, stamp: &str, holds: &Holds) -> Vec<Entry> {
    let mut out = Vec::new();
    let backups = root.join("backups");
    if backups.is_dir() {
        for name in glob_matches(&backups, "graph.db.*") {
            let path = backups.join(&name);
            let status = if holds.holds(&format!("backups/{name}")) {
                Status::Held
            } else if apply {
                park_file(root, stamp, &path)
            } else {
                Status::Pending("park backups rotation (dry run)".to_string())
            };
            out.push(Entry {
                legacy: format!("backups/{name}"),
                status,
            });
        }
    }
    out
}

/// The legacy copy is newer: the older one keeps a name in the stamp, then
/// the rename replaces it, so readers never see the path empty.
fn replace_older(root: &Path, stamp: &str, legacy: &Path, new: &Path) -> Status {
    let dir = backup_dir(root, stamp);
    let Some(name) = new.file_name() else {
        return Status::Refused("refused: no file name".to_string());
    };
    let parked = park_target(&dir, name);
    let replaced = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::hard_link(new, &parked))
        .and_then(|()| std::fs::rename(legacy, new));
    if let Err(e) = replaced {
        // Either copy of the older bytes is whole; the extra name is harmless.
        return Status::Refused(format!("rename over new failed: {e}"));
    }
    crate::state_layout_undo::record(root, stamp, Action::Parked, new, &parked);
    crate::state_layout_undo::record(root, stamp, Action::Moved, legacy, new);
    Status::Moved
}

/// Legacy bytes then new bytes, through a tmp file in the new folder, fsync,
/// rename over the new file. The new file's mode carries over (same dir).
fn append_merge(legacy: &Path, new: &Path) -> std::io::Result<()> {
    let perms = std::fs::metadata(new)?.permissions();
    let tmp = new.with_extension(format!("migrating.{}", std::process::id()));
    let mut dst = std::fs::File::create(&tmp)?;
    // Streamed: the logs this merges are documented unbounded, and a full
    // read would spike the daemon's memory by both files' size.
    std::io::copy(&mut std::fs::File::open(legacy)?, &mut dst)?;
    std::io::copy(&mut std::fs::File::open(new)?, &mut dst)?;
    dst.sync_all()?;
    std::fs::set_permissions(&tmp, perms)?;
    std::fs::rename(&tmp, new)?;
    Ok(())
}

fn mtime_of(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

fn drain_legacy_children(root: &Path, legacy: &Path, stamp: &str, apply: bool) -> Vec<String> {
    let Some(folder) = legacy.file_name().and_then(|name| name.to_str()) else {
        return vec![legacy.display().to_string()];
    };
    let Ok(entries) = std::fs::read_dir(legacy) else {
        eprintln!(
            "state-root drain: kept {} (directory unreadable)",
            legacy.display()
        );
        return vec![legacy.display().to_string()];
    };
    let now = std::time::SystemTime::now();
    let mut remaining = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            remaining.push(legacy.display().to_string());
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let park_reason = match folder {
            "cargo-build" => match quiet_untagged_tree(&path, now) {
                Ok(true) => Some("untagged cargo residue quiet past the age limit"),
                Ok(false) => None,
                Err(reason) => {
                    eprintln!("state-root drain: kept {} ({reason})", path.display());
                    remaining.push(name);
                    continue;
                }
            },
            "route-settings" if path.is_file() => match quiet_tree(&path, now) {
                Ok(true) => Some("route-settings file quiet past the age limit"),
                Ok(false) => None,
                Err(reason) => {
                    eprintln!("state-root drain: kept {} ({reason})", path.display());
                    remaining.push(name);
                    continue;
                }
            },
            "plugin-stage"
                if path.is_dir()
                    && (name.starts_with(".fno.new-") || name.starts_with(".fno.old-")) =>
            {
                match quiet_tree(&path, now) {
                    Ok(true) => Some("stale plugin-stage swap residue"),
                    Ok(false) => None,
                    Err(reason) => {
                        eprintln!("state-root drain: kept {} ({reason})", path.display());
                        remaining.push(name);
                        continue;
                    }
                }
            }
            _ => None,
        };
        let Some(reason) = park_reason else {
            let detail = if folder == "plugin-stage" {
                "live stage or unknown entry"
            } else {
                "tagged, recent, or unknown entry"
            };
            eprintln!("state-root drain: kept {} ({detail})", path.display());
            remaining.push(name);
            continue;
        };
        if !apply {
            eprintln!("state-root drain: would park {} ({reason})", path.display());
            remaining.push(name);
            continue;
        }
        match park_file_at(root, stamp, &path) {
            Ok(parked) => eprintln!("state-root drain: parked {} ({reason})", parked.display()),
            Err(error) => {
                eprintln!("state-root drain: kept {} ({error})", path.display());
                remaining.push(name);
            }
        }
    }
    remaining
}

fn quiet_untagged_tree(path: &Path, now: std::time::SystemTime) -> Result<bool, &'static str> {
    if contains_cache_tag(path)? {
        return Err("contains CACHEDIR.TAG");
    }
    quiet_tree(path, now)
}

fn contains_cache_tag(path: &Path) -> Result<bool, &'static str> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "path unreadable")?;
    if metadata.file_type().is_symlink() {
        return Err("symlink is not safe to park");
    }
    if !metadata.is_dir() {
        return Ok(path.file_name().and_then(|n| n.to_str()) == Some("CACHEDIR.TAG"));
    }
    for entry in std::fs::read_dir(path).map_err(|_| "directory unreadable")? {
        let entry = entry.map_err(|_| "directory entry unreadable")?;
        if contains_cache_tag(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn quiet_tree(path: &Path, now: std::time::SystemTime) -> Result<bool, &'static str> {
    fn newest(path: &Path, latest: &mut std::time::SystemTime) -> Result<(), &'static str> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "path unreadable")?;
        if metadata.file_type().is_symlink() {
            return Err("symlink is not safe to park");
        }
        let modified = metadata.modified().map_err(|_| "mtime unavailable")?;
        *latest = (*latest).max(modified);
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|_| "directory unreadable")? {
                newest(
                    &entry.map_err(|_| "directory entry unreadable")?.path(),
                    latest,
                )?;
            }
        }
        Ok(())
    }

    let mut latest = std::time::SystemTime::UNIX_EPOCH;
    newest(path, &mut latest)?;
    let quiet = now.duration_since(latest).unwrap_or_default();
    Ok(quiet >= std::time::Duration::from_secs(LEGACY_DRAIN_AGE_SECS))
}

/// The run's backup folder: `backups/state-root-migration/<utc stamp>/`.
pub(crate) fn backup_dir(root: &Path, stamp: &str) -> PathBuf {
    root.join("backups")
        .join("state-root-migration")
        .join(stamp)
}

fn park_entry(root: &Path, stamp: &str, name: &str) -> Status {
    let p = root.join(name);
    if !p.exists() {
        return Status::Absent;
    }
    park_file(root, stamp, &p)
}

/// Rename one legacy path into the run's backup folder, returning the parked
/// path. Never deletes: a name collision in the same stamp gains `.1`, `.2`,
/// ... so a second pass over the same stamp never clobbers the first copy.
pub(crate) fn park_file_at(root: &Path, stamp: &str, legacy: &Path) -> Result<PathBuf, String> {
    let dir = backup_dir(root, stamp);
    let Some(name) = legacy.file_name() else {
        return Err("refused: no file name".to_string());
    };
    let target = park_target(&dir, name);
    match move_path(legacy, &target) {
        Ok(()) => {
            crate::state_layout_undo::record(root, stamp, Action::Parked, legacy, &target);
            Ok(target)
        }
        Err(e) => {
            // A failed park leaves no empty stamp behind.
            std::fs::remove_dir(&dir).ok();
            Err(format!("refused: {e}"))
        }
    }
}

/// A free name in the stamp. The manifest's own name is never free.
fn park_target(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut target = dir.join(name);
    let mut n = 0;
    while occupied(&target) || target == dir.join(MANIFEST_NAME) {
        n += 1;
        let mut os = name.to_os_string();
        os.push(format!(".{n}"));
        target = dir.join(os);
    }
    target
}

/// [`park_file_at`] as a [`Status`], for callers that only report the fold.
pub(crate) fn park_file(root: &Path, stamp: &str, legacy: &Path) -> Status {
    match park_file_at(root, stamp, legacy) {
        Ok(_) => Status::Parked,
        Err(e) => Status::Refused(e),
    }
}

/// Single-`*` glob match: prefix and suffix around the star. Enough for the
/// table's residue rows; no dependency.
fn glob_matches(root: &Path, pattern: &str) -> Vec<String> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.len() >= prefix.len() + suffix.len()
            && name.starts_with(prefix)
            && name.ends_with(suffix)
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

/// A stamp no earlier pass used: a reverse by stamp must name one pass.
fn fresh_stamp(root: &Path) -> String {
    let base = utc_stamp();
    let mut stamp = base.clone();
    let mut n = 1;
    while backup_dir(root, &stamp).exists() {
        n += 1;
        stamp = format!("{base}-{n}");
    }
    stamp
}

fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Civil-from-days (Howard Hinnant's algorithm); no chrono dependency.
    let days = (now / 86_400) as i64;
    let secs = (now % 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The daemon bin calls this once at startup, BEFORE `daemon::run` opens a
/// store: one apply pass over the state root. Best effort - a refusal only
/// prints, and the daemon's reclaim lane retries it (law: the mover runs
/// from the daemon, never an operator command).
pub fn run_at_daemon_start(home: &crate::paths::AgentsHome) {
    let root = crate::reclaim::reclaim_state_root(home);
    let receipt = migrate(&root, true);
    if receipt.refused_count() > 0 {
        eprintln!(
            "fno-agent-daemon: state-root migration: {}",
            receipt.summary()
        );
    }
}

/// The rollback. A stamp restores what that pass recorded. A folder name
/// swaps its `dir` row's ends and the same mover (merge lane included)
/// carries it home. Both record a hold. A name that is neither is an error,
/// so a typo never reads as a clean rollback.
fn migrate_reverse(root: &Path, names: &[String], apply: bool) -> Result<Receipt, String> {
    let _pass = if apply {
        crate::state_layout_undo::lock_root(root)
    } else {
        None
    };
    let mut folders = Vec::new();
    let mut stamps = Vec::new();
    for name in names {
        if crate::state_layout_undo::is_stamp(root, name) {
            stamps.push(name);
            continue;
        }
        let row = find(name)
            .filter(|r| r.kind == Kind::Dir && r.legacy != r.new)
            .ok_or_else(|| format!("no stamp with a manifest and no moved dir row named {name}"))?;
        folders.push(row);
    }
    let mut receipt = Receipt::default();
    for from_stamp in stamps {
        let stamp = fresh_stamp(root);
        let part = crate::state_layout_undo::reverse_stamp(root, from_stamp, &stamp, apply)
            .map_err(|e| e.to_string())?;
        receipt.entries.extend(part.entries);
    }
    for row in folders {
        let stamp = fresh_stamp(root);
        receipt.entries.push(Entry {
            legacy: row.new.clone(),
            status: reverse_folder(root, row, &stamp, apply),
        });
    }
    Ok(receipt)
}

/// One folder home, held before it moves.
fn reverse_folder(root: &Path, row: &Row, stamp: &str, apply: bool) -> Status {
    let swapped = Row {
        legacy: row.new.clone(),
        new: row.legacy.clone(),
        ..row.clone()
    };
    let now = home_of(root, &row.new);
    let home = root.join(&row.legacy);
    if !MERGE_FOLDERS.contains(&row.legacy.as_str()) {
        for child in std::fs::read_dir(&now).into_iter().flatten().flatten() {
            let back = home.join(child.file_name());
            if occupied(&back) {
                let clash = crate::state_layout_move::MoveError::Occupied {
                    from: child.path(),
                    to: back,
                };
                return Status::Refused(format!("a reverse never overwrites: {clash}"));
            }
        }
    }
    if !apply {
        return migrate_dir(root, &swapped, false, stamp);
    }
    let held = Holds::load(root).and_then(|mut holds| {
        let added = holds.add(root, [row.legacy.clone()])?;
        Ok((holds, added))
    });
    let (mut holds, added) = match held {
        Ok(held) => held,
        Err(e) => return Status::Refused(format!("the hold was not recorded: {e}")),
    };
    let status = migrate_dir(root, &swapped, true, stamp);
    if matches!(status, Status::Pending(_)) && !added.is_empty() {
        if let Err(e) = holds.drop_names(root, &added) {
            return Status::Refused(format!("nothing moved and the hold stays: {e}"));
        }
        return status;
    }
    if let Some(parent) = now.parent() {
        if parent.starts_with(root) && parent != root && parent.parent() != Some(root) {
            crate::state_layout_undo::remove_if_empty(root, stamp, parent);
        }
    }
    status
}

/// What one `state migrate` call asks for.
enum Ask {
    Forward,
    Release(Vec<String>),
    Reverse(Vec<String>),
}

fn parse_ask(args: &[String]) -> Result<(bool, Ask), &'static str> {
    let (mut apply, mut reverse) = (false, false);
    let mut names = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--apply" => apply = true,
            "--reverse" => reverse = true,
            other if crate::json_output::is_flag(other) => {}
            other if other.starts_with('-') => return Err("unknown flag"),
            other => names.push(other.to_string()),
        }
    }
    let ask = match (reverse, names.is_empty()) {
        (true, true) => return Err("--reverse needs at least one folder name or stamp"),
        (true, false) => Ask::Reverse(names),
        (false, true) => Ask::Forward,
        (false, false) if apply => Ask::Release(names),
        (false, false) => return Err("a name clears a hold, and that needs --apply"),
    };
    Ok((apply, ask))
}

/// `fno-agent state migrate [--apply [<held name>...]] [--reverse
/// <name|stamp>...] [--json|-J]`: dry run by default, and a dry run writes
/// nothing. A name after `--apply` clears that hold. Resolves the state root
/// the way every other state-root reader does.
pub fn run_migrate_cli(args: &[String]) -> i32 {
    let (apply, ask) = match parse_ask(args) {
        Ok(asked) => asked,
        Err(why) => {
            eprintln!("fno-agent state migrate: {why}: {}", args.join(" "));
            return 2;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join(".fno")
    });
    let receipt = match ask {
        Ask::Forward => migrate(&root, apply),
        Ask::Release(names) => match crate::state_layout_undo::release(&root, &names) {
            Ok(()) => migrate(&root, apply),
            Err(e) => {
                eprintln!("fno-agent state migrate: {e}");
                return 2;
            }
        },
        Ask::Reverse(names) => match migrate_reverse(&root, &names, apply) {
            Ok(receipt) => receipt,
            Err(e) => {
                eprintln!("fno-agent state migrate: {e}");
                return 2;
            }
        },
    };
    print_receipt(&root, &receipt, apply, crate::json_output::requested(args));
    if receipt.refused_count() > 0 {
        1
    } else {
        0
    }
}

fn print_receipt(root: &Path, receipt: &Receipt, apply: bool, json: bool) {
    if json {
        let entries: Vec<String> = receipt
            .entries
            .iter()
            .map(|e| {
                let detail = match e.status.detail() {
                    Some(d) => format!(",\"detail\":\"{}\"", json_escape(d)),
                    None => String::new(),
                };
                format!(
                    "{{\"legacy\":\"{}\",\"status\":\"{}\"{}}}",
                    json_escape(&e.legacy),
                    e.status.word(),
                    detail
                )
            })
            .collect();
        println!(
            "{{\"root\":\"{}\",\"pending\":{},\"entries\":[{}]}}",
            json_escape(&root.display().to_string()),
            receipt.pending_count(),
            entries.join(",")
        );
        return;
    }
    for e in &receipt.entries {
        match e.status.detail() {
            Some(d) => println!("  {}: {} ({})", e.status.word(), e.legacy, d),
            None => println!("  {}: {}", e.status.word(), e.legacy),
        }
    }
    println!(
        "state migrate ({}): {}",
        if apply { "applied" } else { "dry run" },
        receipt.summary()
    );
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fno-state-layout-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clean(root: &Path) {
        std::fs::remove_dir_all(root).ok();
    }

    // The keep rows are the one closed set: every destination a row names
    // starts with a kept folder, and a keep row stays where it is. A new
    // top-level name therefore needs a keep row, and that row is the review.
    #[test]
    fn top_level_set_is_closed() {
        let table = parse_table(LAYOUT_TSV).expect("shipped table must parse");
        for row in &table {
            match row.kind {
                Kind::Keep => {
                    assert_eq!(row.legacy, row.new, "keep row {} moves", row.legacy);
                    assert!(!row.new.contains('/'), "keep row {} nests", row.legacy);
                }
                Kind::Park => assert_eq!(row.new, "-", "park row {} has a home", row.legacy),
                _ => {
                    let first = row.new.split('/').next().unwrap_or(&row.new);
                    assert!(
                        row.new.contains('/') && is_kept(first),
                        "row {} lands at {}, outside the kept folders",
                        row.legacy,
                        row.new
                    );
                }
            }
        }
        assert!(rows().iter().all(|r| r.kind != Kind::Keep));
        assert!(
            find("agents").is_none(),
            "the resolvers never read a keep row"
        );
        assert!(
            is_kept("config.toml.bak-20261113-render"),
            "a keep glob admits its match"
        );
        assert!(
            is_carried("bus") && !is_kept("bus"),
            "a fold folder is owed a move"
        );
        assert!(
            !is_carried("profile-new"),
            "an undeclared name is carried by nothing"
        );
    }

    #[test]
    fn dir_row_folds_children_parks_the_clash_and_reads_both() {
        let root = tmp_root("dirfold");
        std::fs::create_dir_all(root.join("flight")).unwrap();
        std::fs::write(root.join("flight").join("a.json"), b"A").unwrap();
        std::fs::create_dir_all(root.join("state").join("flight")).unwrap();
        std::fs::write(root.join("state").join("flight").join("a.json"), b"NEW").unwrap();
        std::fs::write(root.join("state").join("flight").join("b.json"), b"B").unwrap();
        let rows = parse_table("flight\tstate/flight\tdir\tdaemon\n").unwrap();
        let dry = migrate_rows(&root, &rows, false, "test");
        assert!(
            dry.iter().all(|e| e.status.is_pending()),
            "a dry run of a retargeted dir row is pending"
        );
        assert!(
            root.join("flight").join("a.json").exists(),
            "a dry run moves nothing"
        );
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().all(|e| matches!(e.status, Status::Moved)),
            "the fold happened: {receipt:?}"
        );
        assert!(
            !root.join("flight").exists(),
            "the emptied legacy folder is removed"
        );
        assert_eq!(
            std::fs::read(root.join("state").join("flight").join("a.json")).unwrap(),
            b"NEW",
            "the new side wins the clash"
        );
        let parked = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert!(parked >= 1, "the clashing legacy child parked");
        assert_eq!(
            std::fs::read(root.join("state").join("flight").join("b.json")).unwrap(),
            b"B"
        );
        // Readers follow the SHIPPED row: flight now folds under state/, so
        // place_dir answers the destination always, while the legacy dir
        // above still holds files. The synthetic fold must not leak into the
        // reader.
        assert_eq!(
            place_dir(&root, "flight"),
            Some(root.join("state").join("flight"))
        );
        clean(&root);

        // The pre-fold read: the file resolves through the legacy dir until
        // the mover folds it, and the merge-list read sees it too.
        let root = tmp_root("dirlegacy");
        std::fs::create_dir_all(root.join("flight")).unwrap();
        std::fs::write(root.join("flight").join("c.json"), b"C").unwrap();
        let rows = parse_table("flight\tstate/flight\tdir\tdaemon\n").unwrap();
        let _ = migrate_rows(&root, &rows, false, "test");
        assert_eq!(
            place_in(&root, "flight", "c.json"),
            root.join("flight").join("c.json")
        );
        let listed = list_dir_children(&root, "flight");
        assert_eq!(listed.len(), 1, "the list read sees the legacy child");
        clean(&root);
    }

    #[test]
    fn cargo_and_plugin_stage_drains_preserve_live_payload_and_park_stale_residue() {
        fn age_tree(path: &Path, old: std::time::SystemTime) {
            if let Ok(file) = std::fs::File::options().read(true).open(path) {
                let _ = file.set_times(std::fs::FileTimes::new().set_modified(old));
            }
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    age_tree(&entry.path(), old);
                }
            }
        }

        let rows = parse_table(
            "cargo-build\tcache/cargo-build\tdir\tdaemon\nplugin-stage\tinstall/plugin-stage\tdir\tdaemon\n\
             route-settings\tstate/route-settings\tdir\tdaemon\nevals\tdb/evals\tdir\tdaemon\n",
        )
        .unwrap();
        let live = tmp_root("drain-live");
        let tagged = live.join("cargo-build/00/hash");
        std::fs::create_dir_all(&tagged).unwrap();
        std::fs::write(tagged.join("CACHEDIR.TAG"), b"Signature: cache\n").unwrap();
        let stage_file = live.join("plugin-stage/fno/hooks/stop.sh");
        std::fs::create_dir_all(stage_file.parent().unwrap()).unwrap();
        std::fs::write(&stage_file, b"live hook\n").unwrap();
        std::fs::create_dir_all(live.join("route-settings")).unwrap();
        std::fs::write(live.join("route-settings/fresh.json"), b"{}").unwrap();
        std::fs::create_dir_all(live.join("evals/claude-config")).unwrap();
        std::fs::write(live.join("evals/claude-config/.claude.json"), b"{}").unwrap();
        std::fs::write(live.join("evals/labels.jsonl"), b"{}\n").unwrap();

        let receipt = migrate_rows(&live, &rows, true, "live");
        let evals = receipt
            .iter()
            .find(|entry| entry.legacy == "evals")
            .unwrap();
        assert!(
            matches!(evals.status, Status::Pending(ref reason) if reason.contains("sealed eval login")),
            "the sealed login's folder stays and names its holder: {evals:?}"
        );
        assert!(live.join("evals/claude-config/.claude.json").is_file());
        assert!(live.join("db/evals/labels.jsonl").is_file());
        for name in ["cargo-build", "plugin-stage", "route-settings"] {
            let row = receipt.iter().find(|entry| entry.legacy == name).unwrap();
            assert!(
                matches!(row.status, Status::Pending(ref reason) if reason.starts_with("drains:")),
                "{name} must wait for its recorded path to drain: {row:?}"
            );
            assert!(live.join(name).exists(), "{name} must stay at its old path");
        }
        assert!(tagged.join("CACHEDIR.TAG").is_file());
        assert!(stage_file.is_file());
        clean(&live);

        let residue = tmp_root("drain-residue");
        let old_cargo = residue.join("cargo-build/fc/build/output");
        std::fs::create_dir_all(old_cargo.parent().unwrap()).unwrap();
        std::fs::write(&old_cargo, b"old untagged residue\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(4 * 24 * 60 * 60);
        age_tree(&residue.join("cargo-build"), old);
        for name in [".fno.new-old", ".fno.old-old"] {
            let marker = residue.join("plugin-stage").join(name).join("marker");
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            std::fs::write(marker, b"stale swap residue\n").unwrap();
        }
        age_tree(&residue.join("plugin-stage"), old);
        std::fs::create_dir_all(residue.join("route-settings")).unwrap();
        std::fs::write(residue.join("route-settings/old.json"), b"{}").unwrap();
        age_tree(&residue.join("route-settings"), old);

        let receipt = migrate_rows(&residue, &rows, true, "test");
        for name in ["cargo-build", "plugin-stage", "route-settings"] {
            let row = receipt.iter().find(|entry| entry.legacy == name).unwrap();
            assert!(matches!(row.status, Status::Moved), "{row:?}");
            assert!(
                !residue.join(name).exists(),
                "empty {name} shell is removed"
            );
        }
        let parked = residue.join("backups/state-root-migration/test");
        assert!(parked.join("fc").join("build/output").is_file());
        assert!(parked.join(".fno.new-old/marker").is_file());
        assert!(parked.join(".fno.old-old/marker").is_file());
        assert!(parked.join("old.json").is_file());
        clean(&residue);
    }

    #[test]
    fn dir_row_waits_for_a_held_child_lock() {
        let root = tmp_root("dirlock");
        std::fs::create_dir_all(root.join("locks")).unwrap();
        let lock_file = root.join("locks").join("plan-abc.lock");
        std::fs::write(&lock_file, b"").unwrap();
        let rows = parse_table("locks\tstate/locks\tdir\tdaemon\n").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .open(&lock_file)
            .unwrap();
        let rc = unsafe {
            libc::flock(
                std::os::unix::io::AsRawFd::as_raw_fd(&held),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        assert_eq!(rc, 0, "test setup holds the lock");
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt
                .iter()
                .any(|e| matches!(e.status, Status::Pending(_))),
            "a held child lock keeps the row pending: {receipt:?}"
        );
        assert!(
            root.join("locks").join("plan-abc.lock").exists(),
            "nothing moved past a held lock"
        );
        use std::os::unix::fs::MetadataExt;
        let before = std::fs::metadata(root.join("locks")).unwrap();
        drop(held);
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().any(|e| matches!(e.status, Status::Moved)),
            "the fold runs once the lock releases: {receipt:?}"
        );
        assert!(!root.join("locks").exists(), "the emptied folder left");
        let moved = root.join("state").join("locks");
        let after = std::fs::metadata(&moved).expect("state/locks exists after the fold");
        assert!(
            after.dev() == before.dev() && after.ino() == before.ino(),
            "the fold is one whole-dir rename, not per-child moves: {before:?} -> {after:?}"
        );
        assert!(
            root.join("state")
                .join("locks")
                .join("plan-abc.lock")
                .exists(),
            "the child landed at the new path"
        );
        clean(&root);
    }

    #[test]
    fn the_root_event_journal_imports_then_parks_with_a_receipt() {
        let root = tmp_root("journal-retire");
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("FNO_AGENT_HOME");
        std::env::set_var("FNO_AGENT_HOME", root.join("agents"));
        let root = std::fs::canonicalize(&root).unwrap();
        let ts = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            root.join("events.jsonl"),
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"live\"}}}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("events.jsonl.1"),
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"rotated\"}}}}\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("events.jsonl.lock"), b"").unwrap();

        let receipt = migrate(&root, true);
        let journal_entries: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("(journal)"))
            .collect();
        assert_eq!(journal_entries.len(), 2, "{receipt:?}");
        assert!(
            journal_entries
                .iter()
                .all(|e| matches!(e.status, Status::Merged)),
            "{receipt:?}"
        );
        let journal = root.join("events.jsonl");
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["retire_probe"]),
        )
        .unwrap();
        assert_eq!(rows.len(), 2, "both seeded rows committed");
        assert!(
            !journal.exists() && !root.join("events.jsonl.1").exists(),
            "the family left the root"
        );
        assert!(
            !root.join("events.jsonl.lock").exists(),
            "the lock parked beside the family"
        );
        let mut stamps = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .unwrap()
            .flatten();
        let stamp_dir = stamps.next().expect("one parked stamp folder").path();
        assert!(
            stamp_dir.join("events.jsonl").exists() && stamp_dir.join("events.jsonl.1").exists(),
            "the parked copies survive"
        );
        let receipts = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["legacy_journal_retired"]),
        )
        .unwrap();
        assert_eq!(receipts.len(), 1, "one receipt row");
        assert!(
            serde_json::from_str::<serde_json::Value>(&receipts[0].line)
                .unwrap()
                .pointer("/data/repeat")
                == Some(&serde_json::Value::Bool(false)),
            "the first retire reads repeat false"
        );

        // A recreated file is a repeat: the new bytes ingest, the file parks
        // again, and the receipt says so. A root with no family file reports
        // no journal entry at all.
        std::fs::write(
            &journal,
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"again\"}}}}\n"
            ),
        )
        .unwrap();
        let receipt = migrate(&root, true);
        assert!(
            receipt
                .entries
                .iter()
                .any(|e| e.legacy.contains("(journal)")),
            "{receipt:?}"
        );
        let receipts = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["legacy_journal_retired"]),
        )
        .unwrap();
        assert_eq!(receipts.len(), 2, "the repeat retires once more");
        assert!(
            serde_json::from_str::<serde_json::Value>(&receipts[1].line)
                .unwrap()
                .pointer("/data/repeat")
                == Some(&serde_json::Value::Bool(true)),
            "the recreated file retires as a repeat"
        );
        let quiet = migrate(&root, true);
        assert!(
            !quiet.entries.iter().any(|e| e.legacy.contains("(journal)")),
            "a root with no family file reports no journal entry"
        );

        // AC5-ERR: a question journal holding a row its store lacks stays
        // at the root, and the pass reads pending naming the file.
        let questions = root.join("questions.jsonl");
        std::fs::write(
            &questions,
            format!("{{\"ts\":\"{ts}\",\"type\":\"operator_question\"}}\n"),
        )
        .unwrap();
        let held = migrate(&root, true);
        assert!(
            held.entries
                .iter()
                .any(|e| e.legacy.contains("questions.jsonl")
                    && matches!(e.status, Status::Pending(ref why) if why.contains("lacks"))),
            "{held:?}"
        );
        assert!(
            questions.is_file(),
            "the file stays until a read ingests it"
        );
        match prior_home {
            Some(v) => std::env::set_var("FNO_AGENT_HOME", v),
            None => std::env::remove_var("FNO_AGENT_HOME"),
        }
        clean(&root);
    }

    #[test]
    fn overwrite_newer_mtime_wins() {
        // New is newer: legacy parks, new bytes stay.
        let root = tmp_root("ow-new");
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(root.join("notify-signals.json"), b"legacy").unwrap();
        let new_path = root.join("state").join("notify-signals.json");
        std::fs::write(&new_path, b"new").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(90);
        set_mtime(&new_path, newer);
        let receipt = migrate(&root, true);
        assert_eq!(std::fs::read(&new_path).unwrap(), b"new");
        assert!(!root.join("notify-signals.json").exists());
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "notify-signals.json")
            .unwrap();
        assert!(matches!(found.status, Status::Parked));
        clean(&root);
        // Legacy is newer: its bytes land on the new path.
        let root = tmp_root("ow-legacy");
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(root.join("notify-signals.json"), b"legacy").unwrap();
        let new_path = root.join("state").join("notify-signals.json");
        std::fs::write(&new_path, b"new").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(90);
        set_mtime(root.join("notify-signals.json").as_path(), newer);
        let _ = migrate(&root, true);
        assert_eq!(std::fs::read(&new_path).unwrap(), b"legacy");
        clean(&root);
        // The sqlite kind rides the same move rules through the backup-API
        // protocol (state_layout_sqlite). Branch: committed rows still in
        // the -wal land in the published copy and the trio parks.
        let root = tmp_root("ow-sq-wal");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE layout_nodes (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO layout_nodes VALUES ('x-1','one');",
        )
        .unwrap();
        let row = parse_table("graph.db\tdb/graph.db\tsqlite\tdaemon\n")
            .unwrap()
            .remove(0);
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-wal");
        assert!(matches!(status, Status::Moved), "{status:?}");
        let new_db = root.join("db").join("graph.db");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM layout_nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the committed row moved");
        assert!(!legacy.exists(), "the legacy file parked");
        assert!(root
            .join("backups")
            .join("state-root-migration")
            .join("ow-wal")
            .join("graph.db")
            .exists());
        drop(c);
        clean(&root);
        // Branch: a writer holding BEGIN IMMEDIATE past the busy timeout
        // parks the row, leaves the trio untouched, and clears the fence.
        let root = tmp_root("ow-sq-busy");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-busy");
        assert!(
            matches!(status, Status::Pending(_)),
            "expected pending, got {status:?}"
        );
        assert!(legacy.exists(), "the legacy trio is untouched");
        assert!(!crate::state_layout_sqlite::fence_path(&root).exists());
        conn.execute_batch("ROLLBACK;").unwrap();
        drop(conn);
        clean(&root);
        // Branch: rows on both sides refuse and keep both copies.
        let root = tmp_root("ow-sq-both");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        let conn = rusqlite::Connection::open(&new_db).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (2);")
            .unwrap();
        drop(conn);
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-both");
        assert!(matches!(status, Status::Refused(_)), "{status:?}");
        assert!(legacy.exists() && new_db.exists(), "both copies stay");
        clean(&root);
        // Branch: an empty new copy is a fresh-create race; it parks and the
        // legacy rows win.
        let root = tmp_root("ow-sq-empty");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        rusqlite::Connection::open(&new_db)
            .unwrap()
            .execute_batch("CREATE TABLE z (a);")
            .unwrap();
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-empty");
        assert!(matches!(status, Status::Moved), "{status:?}");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the legacy rows won");
        drop(c);
        // Branch: the deferred verify compares a parked copy against its
        // park-time counts once the record ages past the delay. Same root:
        // the ow-empty stamp's record is still in place.
        let record = root
            .join("backups")
            .join("state-root-migration")
            .join("ow-empty")
            .join("verify.tsv");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&record)
            .unwrap();
        f.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60)),
        )
        .unwrap();
        drop(f);
        let swept = crate::state_layout_sqlite::verify_sweep(&root, "ow-later", true);
        assert_eq!(swept.len(), 1, "{swept:?}");
        assert!(matches!(swept[0].1, Status::Moved), "{swept:?}");
        // Branch: a reappeared legacy event-family store imports by
        // event_id into the published store, then parks.
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}'), (2, 'destination-only', x'03', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let straggler = root.join("events.db");
        let mut c = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut c, &straggler).unwrap();
        c.execute_batch(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}'), (2, 'e-2', x'02', 0, 'x', 't', '{}');",
        )
        .unwrap();
        drop(c);
        let receipt = migrate(&root, true);
        let merged: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(matches!(merged[0].status, Status::Merged), "{merged:?}");
        let c = rusqlite::Connection::open(&published).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 3,
            "distinct event identities survive sequence collisions"
        );
        let source_rows: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_id = 'e-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_rows, 1, "the colliding source event survives once");
        assert!(!straggler.exists(), "the straggler parked");
        let mut source = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut source, &straggler).unwrap();
        source.execute_batch(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'distinct-conflict', x'03', 0, 'x', 't', '{}');",
        ).unwrap();
        drop(source);
        let refused = migrate(&root, true);
        assert!(
            refused
                .entries
                .iter()
                .any(|e| e.legacy.contains("straggler") && matches!(e.status, Status::Refused(_))),
            "{refused:?}"
        );
        assert!(
            straggler.exists(),
            "a distinct event is retained on an integrity conflict"
        );
        clean(&root);
        // Branch: a zero-length straggler beside a published store parks
        // without a schema write and reads Parked.
        let root = tmp_root("ow-straggler-empty");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        std::fs::write(root.join("events.db"), b"").unwrap();
        let receipt = migrate(&root, true);
        let parked: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(parked.len(), 1, "{parked:?}");
        assert!(matches!(parked[0].status, Status::Parked), "{parked:?}");
        assert!(
            !root.join("events.db").exists(),
            "the empty straggler left the root"
        );
        let parked_copy = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("events.db"))
            .find(|p| p.exists())
            .expect("the empty straggler parked in a stamp folder");
        assert_eq!(
            std::fs::metadata(&parked_copy).unwrap().len(),
            0,
            "the parked copy is still zero length"
        );
        clean(&root);
        // Branch: a zero-length straggler with a live -wal is not parked
        // blind. The guard routes it down the import path, so the entry
        // reads Merged (the import's park), never the blind Parked. Whether
        // the WAL's frames replay is the import's contract, pinned by the
        // ow-straggler-order branch; the verdict here is the routing.
        let root = tmp_root("ow-straggler-wal");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        drop(c);
        let full = root.join("events-full.db");
        let mut conn = rusqlite::Connection::open(&full).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;",
        )
        .unwrap();
        crate::event_store::ensure_schema(&mut conn, &full).unwrap();
        conn.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-9', x'09', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        std::fs::copy(root.join("events-full.db-wal"), root.join("events.db-wal")).unwrap();
        drop(conn);
        std::fs::write(root.join("events.db"), b"").unwrap();
        let receipt = migrate(&root, true);
        let wal_row = receipt
            .entries
            .iter()
            .find(|e| e.legacy.contains("straggler"))
            .expect("the straggler row reads");
        assert!(
            matches!(wal_row.status, Status::Merged),
            "a -wal-bearing straggler routes through the import, not the blind park: {wal_row:?}"
        );
        assert!(
            !root.join("events.db").exists(),
            "the import path left the root clean"
        );
        clean(&root);
        // Branch: a published store whose columns came out of ALTER in
        // another order still imports: the compare is set-wise now.
        let root = tmp_root("ow-straggler-order");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let c = rusqlite::Connection::open(&published).unwrap();
        c.execute_batch(
            "CREATE TABLE events (\
                 seq INTEGER PRIMARY KEY, \
                 event_id TEXT UNIQUE NOT NULL, \
                 row_hash BLOB UNIQUE NOT NULL, \
                 ts_ms INTEGER NOT NULL, \
                 type TEXT NOT NULL, \
                 source TEXT NOT NULL, \
                 scope TEXT, \
                 retention_class TEXT NOT NULL DEFAULT 'durable', \
                 session_id TEXT, \
                 node_id TEXT, \
                 pr_number INTEGER, \
                 head_sha TEXT, \
                 repo TEXT, \
                 reject_reason TEXT, \
                 line TEXT NOT NULL, \
                 caused_by TEXT);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'dst-1', x'01', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let straggler = root.join("events.db");
        let mut c = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut c, &straggler).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'src-1', x'02', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let receipt = migrate(&root, true);
        let merged: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(matches!(merged[0].status, Status::Merged), "{merged:?}");
        let c = rusqlite::Connection::open(&published).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "the straggler row landed beside the published one");
        drop(c);
        assert!(!straggler.exists(), "the imported straggler parked");
        clean(&root);
        // Branch: a zero-length legacy beside a populated published graph
        // parks as .empty-legacy and reads Parked, not refused.
        let root = tmp_root("ow-empty-legacy");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        c.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(c);
        let legacy = root.join("graph.db");
        std::fs::write(&legacy, b"").unwrap();
        std::fs::write(root.join("graph.db-shm"), b"").unwrap();
        let row = parse_table("graph.db\tdb/graph.db\tsqlite\tdaemon\n")
            .unwrap()
            .remove(0);
        let status =
            crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-empty-legacy");
        assert!(matches!(status, Status::Parked), "{status:?}");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the published rows are untouched");
        drop(c);
        assert!(!legacy.exists(), "the empty legacy left the root");
        assert!(
            root.join("backups")
                .join("state-root-migration")
                .join("ow-empty-legacy")
                .join("graph.db.empty-legacy")
                .exists(),
            "the empty legacy parked under its suffix"
        );
        assert!(
            root.join("backups")
                .join("state-root-migration")
                .join("ow-empty-legacy")
                .join("graph.db.empty-legacy-shm")
                .exists(),
            "the zero-length sidecar parked beside the main file"
        );
        clean(&root);
    }

    fn set_mtime(p: &Path, t: std::time::SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn refused_when_rename_fails() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = tmp_root("refuse");
        std::fs::write(root.join("installed-rev"), b"abc\n").unwrap();
        let restore = std::fs::metadata(&root).unwrap().permissions();
        let mut locked = restore.clone();
        use std::os::unix::fs::PermissionsExt;
        locked.set_mode(0o500);
        std::fs::set_permissions(&root, locked).unwrap();
        let receipt = migrate(&root, true);
        std::fs::set_permissions(&root, restore).unwrap();
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "installed-rev")
            .unwrap();
        assert!(
            matches!(found.status, Status::Refused(ref d) if d.contains("refused:")),
            "{:?}",
            found.status
        );
        assert!(root.join("installed-rev").exists(), "legacy untouched");
        clean(&root);
    }
    fn lines(p: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }

    #[test]
    fn reverse_carries_a_folded_folder_home_and_refuses_an_unknown_name() {
        let root = tmp_root("reverse");
        std::fs::create_dir_all(root.join("db/mail/bus")).unwrap();
        std::fs::write(root.join("db/mail/bus/messages.jsonl"), "a\n").unwrap();
        let out = migrate_reverse(&root, &["bus".to_string()], true).unwrap();
        assert!(!out.entries[0].status.is_pending());
        assert_eq!(lines(&root.join("bus/messages.jsonl")), ["a"]);
        assert!(!root.join("db/mail").exists());
        assert!(migrate_reverse(&root, &["nope".to_string()], true).is_err());
        // Outside the message folders a reverse never overwrites.
        std::fs::create_dir_all(root.join("flight")).unwrap();
        std::fs::create_dir_all(root.join("state/flight")).unwrap();
        std::fs::write(root.join("flight/a"), "legacy").unwrap();
        std::fs::write(root.join("state/flight/a"), "new").unwrap();
        let out = migrate_reverse(&root, &["flight".to_string()], true).unwrap();
        assert!(matches!(
            out.entries.first().map(|e| &e.status),
            Some(Status::Refused(_))
        ));
        assert_eq!(std::fs::read(root.join("state/flight/a")).unwrap(), b"new");
        clean(&root);
    }

    #[test]
    fn vendored_table_matches_the_repo_copy() {
        let repo = include_str!("../../../../docs/state-root-layout.tsv");
        assert_eq!(
            LAYOUT_TSV, repo,
            "the vendored layout table drifted from docs/state-root-layout.tsv;              edit the repo copy and copy it into both crates"
        );
    }

    fn age_tree_for_class(path: &Path) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            for child in std::fs::read_dir(path).unwrap().flatten() {
                age_tree_for_class(&child.path());
            }
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 3600);
        std::fs::File::open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    fn put(path: &Path, bytes: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// One machine in a temp dir: a HOME and both class homes, all pinned.
    struct Machine {
        base: PathBuf,
        root: PathBuf,
        cache: PathBuf,
        state: PathBuf,
        _env: Vec<crate::claims::EnvVarGuard>,
    }

    impl Machine {
        /// Call with the env lock held.
        fn new(tag: &str) -> Self {
            use crate::claims::EnvVarGuard;
            let base = tmp_root(tag).canonicalize().unwrap();
            let root = base.join("home/.fno");
            let cache = base.join("cache-home");
            let state = base.join("state-home");
            let config = base.join("config.toml");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(&config, "").unwrap();
            let set = |key, value: &Path| EnvVarGuard::set(key, value.to_str().unwrap());
            let env = vec![
                set("HOME", &base.join("home")),
                set("FNO_STATE_DIR", &root),
                set("FNO_AGENT_HOME", &root.join("agents")),
                set("FNO_CONFIG", &config),
                set("FNO_CACHE_HOME", &cache),
                set("FNO_STATE_HOME", &state),
                EnvVarGuard::unset("FNO_PR_STATUS_CACHE_DIR"),
                EnvVarGuard::unset("FNO_GH_FACTS_DIR"),
                EnvVarGuard::unset("FNO_PLANS_DIRS_CACHE_DIR"),
            ];
            Self {
                base,
                root,
                cache,
                state,
                _env: env,
            }
        }

        fn stamps(&self) -> Vec<String> {
            let mut names: Vec<String> =
                std::fs::read_dir(self.root.join("backups/state-root-migration"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
            names.sort();
            names
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            clean(&self.base);
        }
    }

    /// Every path below `root` with its mode, mtime and bytes.
    fn snapshot(root: &Path) -> Vec<(PathBuf, u32, std::time::SystemTime, Vec<u8>)> {
        use std::os::unix::fs::PermissionsExt;
        let mut out = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(path) = todo.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let mut bytes = Vec::new();
            if meta.is_dir() {
                todo.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .flatten()
                        .map(|e| e.path()),
                );
            } else if meta.is_file() {
                bytes = std::fs::read(&path).unwrap();
            }
            let modified = meta.modified().unwrap();
            out.push((path, meta.permissions().mode(), modified, bytes));
        }
        out.sort();
        out
    }

    #[test]
    fn cache_class_moves_once_and_every_writer_lands_in_the_cache_home() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let m = Machine::new("class-cache");
        let legacy = m.root.join("cache");
        let row = r#"{"row":1}"#;
        put(&legacy.join("gh-facts/merged/o--r-pr1.json"), row);
        put(&legacy.join("pr-status/a.json"), "{}");
        put(&legacy.join("corrections/cursors.json"), "{}");
        put(&legacy.join("lead-drain.json"), "{}");
        age_tree_for_class(&legacy);

        // Until the move, a writer's home read falls back to the legacy row.
        let facts = crate::gh_cache::rows_root(&m.base).unwrap();
        assert_eq!(facts, m.cache.join("gh-facts"));
        assert_eq!(
            crate::gh_cache::read_row(&facts, "merged", "o/r", Some(1), None, 0.0),
            serde_json::json!({"row": {"row": 1}})
        );

        migrate(&m.root, true);
        assert!(!legacy.exists(), "the cache folder drained");
        for child in [
            "gh-facts/merged/o--r-pr1.json",
            "pr-status/a.json",
            "corrections/cursors.json",
            "lead-drain.json",
        ] {
            assert!(m.cache.join(child).is_file(), "{child} lives in the home");
        }

        let cwd = &m.base;
        let writers = [
            crate::gh_cache::rows_root(cwd).unwrap().join("row.json"),
            crate::agents_config::pr_status_cache_dir(cwd)
                .unwrap()
                .join("b.json"),
            crate::pr_admission::pr_origin_cache_dir(cwd)
                .unwrap()
                .join("o.json"),
            crate::pr_steward::state_dir(cwd).unwrap().join("s.json"),
            crate::lead_ops::drain_cache_file(cwd).unwrap(),
            crate::plans_dirs::cache_path(cwd).unwrap(),
            crate::accounts::benchmarks::snapshot_path(cwd).unwrap(),
            crate::correction_fold::write_path(&m.root),
            place_new(&m.root, "graph.html"),
            place_dir(&m.root, "context-cache").unwrap().join("c.json"),
            place_dir(&m.root, "cargo-build").unwrap().join("c.json"),
            place_dir(&m.root, "fleet").unwrap().join("activity.json"),
        ];
        for path in &writers {
            assert!(
                path.starts_with(&m.cache),
                "{} left the home",
                path.display()
            );
            put(path, "{}");
        }
        assert_eq!(
            place_new(&m.root, "health-alerts.log"),
            m.state.join("logs/health-alerts.log")
        );
        assert_eq!(
            page_home(&m.root, &m.root.join("cache/pages/graph.html")),
            Some(m.cache.join("pages/graph.html")),
            "the legacy default spelling renders at the home"
        );

        let before = (m.stamps(), snapshot(&m.cache));
        migrate(&m.root, true);
        assert_eq!(
            (m.stamps(), snapshot(&m.cache)),
            before,
            "a second pass moves and parks nothing"
        );

        // An old binary's fresh copy waits: it never parks under its writer.
        put(&legacy.join("lead-drain.json"), "old binary");
        let receipt = migrate(&m.root, true);
        assert!(
            receipt
                .entries
                .iter()
                .any(|e| e.legacy.contains("lead-drain.json") && e.status.is_pending()),
            "{receipt:?}"
        );
        assert_eq!(m.stamps(), before.0);
        assert!(legacy.join("lead-drain.json").exists());
    }

    #[test]
    fn a_failed_cross_volume_copy_leaves_the_source_whole_and_parks_nothing() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid takes no argument and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let m = Machine::new("class-copy");
        let source = m.root.join("cache/tree");
        put(&source.join("a"), "A");
        put(&source.join("nested/b"), "B");
        std::os::unix::fs::symlink("a", source.join("link")).unwrap();
        age_tree_for_class(&m.root.join("cache"));
        let chmod = |name: &str, mode: u32| {
            std::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap()
        };
        chmod("a", 0o640);
        chmod("nested/b", 0o000);
        let _copy = crate::state_layout_move::ForceCopy::new();

        for pass in 1..=2 {
            let receipt = migrate(&m.root, true);
            assert!(
                receipt.entries.iter().any(|e| e.legacy.starts_with("tree")
                    && matches!(e.status, Status::Refused(_))),
                "pass {pass}: {receipt:?}"
            );
            assert_eq!(std::fs::read(source.join("a")).unwrap(), b"A");
            assert!(
                source.join("nested/b").exists(),
                "pass {pass}: source whole"
            );
            assert!(
                std::fs::read_dir(&m.cache).map_or(true, |mut d| d.next().is_none()),
                "pass {pass}: no partial destination and no temp"
            );
            assert!(m.stamps().is_empty(), "pass {pass}: nothing parked");
        }

        chmod("nested/b", 0o600);
        migrate(&m.root, true);
        assert!(!source.exists(), "the whole copy removed its source");
        let moved = m.cache.join("tree");
        assert_eq!(std::fs::read(moved.join("nested/b")).unwrap(), b"B");
        assert_eq!(
            std::fs::read_link(moved.join("link")).unwrap(),
            Path::new("a")
        );
        let mode = std::fs::metadata(moved.join("a"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640, "the copy keeps the mode");
    }

    #[test]
    fn class_passes_move_the_default_root_and_leave_another_root_alone() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let m = Machine::new("class-roots");
        // A second root that shares HOME keeps cache and logs inside itself.
        let other = m.base.join("home/.fno-second");
        put(&other.join("logs/health-alerts.log"), "mine\n");
        put(&other.join("cache/corrections/cursors.json"), "{}");
        age_tree_for_class(&other);
        migrate(&other, true);
        assert!(
            !m.cache.exists() && !m.state.exists(),
            "no shared home changed"
        );
        assert_eq!(
            std::fs::read_to_string(place(&other, "health-alerts.log")).unwrap(),
            "mine\n"
        );
        assert_eq!(
            place_in(&other, "corrections", "cursors.json"),
            other.join("cache/corrections/cursors.json")
        );
        assert!(other.join("cache/corrections/cursors.json").is_file());

        // A writer's launchd.out already in the root logs dir moves with the
        // class below; the legacy spelling stays a link after the pass.
        put(&m.root.join("logs/launchd.out"), "rows\n");

        // The default root's logs move, and the legacy spelling stays a link.
        // A new plist can write the home before this pass runs.
        put(&m.state.join("logs/new.out"), "new\n");
        let receipt = migrate(&m.root, true);
        let entry = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "logs (logs-class)")
            .expect("logs entry in the receipt");
        assert!(matches!(entry.status, Status::Moved), "{:?}", entry.status);
        let meta = std::fs::symlink_metadata(m.root.join("logs")).unwrap();
        assert!(meta.file_type().is_symlink(), "legacy spelling is a link");
        for name in ["launchd.out", "new.out"] {
            // A writer spelling the legacy path keeps resolving through the link.
            assert!(m.root.join("logs").join(name).is_file(), "{name}");
        }
        let again = migrate(&m.root, true);
        assert!(again
            .entries
            .iter()
            .all(|e| e.legacy != "logs (logs-class)"));
    }

    #[test]
    fn a_stamp_reverse_holds_and_every_dry_run_changes_no_byte() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let m = Machine::new("class-reverse");
        let old = [
            m.root.join("flight/a.json"),
            m.root.join("notify-signals.json"),
            m.root.join("ledger.md"),
            m.root.join("cache/benchmarks.json"),
            m.root.join("logs/launchd.out"),
        ];
        for path in &old {
            put(path, "kept\n");
        }
        age_tree_for_class(&m.root.join("cache"));
        // An aged verify record: the apply pass may clear it, a dry run may not.
        let parked = m.root.join("backups/state-root-migration/old");
        std::fs::create_dir_all(&parked).unwrap();
        rusqlite::Connection::open(parked.join("events.db"))
            .unwrap()
            .execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        put(&parked.join("verify.tsv"), "t\t1\t-\n");
        age_tree_for_class(&parked.join("verify.tsv"));

        let untouched = snapshot(&m.base);
        migrate(&m.root, false);
        assert_eq!(snapshot(&m.base), untouched, "a forward dry run wrote");

        migrate(&m.root, true);
        for path in &old[..4] {
            assert!(!path.exists(), "{} moved", path.display());
        }
        let stamp = m.stamps().into_iter().find(|s| s != "old").unwrap();

        let applied = snapshot(&m.base);
        migrate_reverse(&m.root, &[stamp.clone()], false).unwrap();
        assert_eq!(snapshot(&m.base), applied, "a reverse dry run wrote");

        migrate_reverse(&m.root, &[stamp], true).unwrap();
        let logs = std::fs::symlink_metadata(m.root.join("logs")).unwrap();
        assert!(logs.is_dir(), "the link left and the folder is back");
        let receipt = migrate(&m.root, true);
        for path in &old {
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                "kept\n",
                "{} survives the forward pass",
                path.display()
            );
        }
        for name in ["flight", "notify-signals.json", "ledger.md"] {
            let entry = receipt.entries.iter().find(|e| e.legacy == name).unwrap();
            assert_eq!(entry.status.word(), "held", "{entry:?}");
        }
        let held = snapshot(&m.base);
        migrate(&m.root, false);
        assert_eq!(snapshot(&m.base), held, "a dry run over a hold wrote");

        crate::state_layout_undo::release(&m.root, &["flight".to_string()]).unwrap();
        migrate(&m.root, true);
        assert!(!old[0].exists(), "a cleared hold lets the fold run");
    }
}
