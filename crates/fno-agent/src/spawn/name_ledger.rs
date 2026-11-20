//! The fleet-wide name ledger: every name any machine drew or bound, in one
//! append-only `name_history` table on the shared primary
//! (`store.remote_url`).
//!
//! Why: `name_history.json` is per machine. The MacBook and the iMac each
//! held 14 names and shared one, so the iMac accepted `--name Tamsin` for a
//! new lead the MacBook had already named Tamsin. The `name:` claims did not
//! close it: only the successor draw takes one, and an expired or released
//! claim leaves the `claims` table for `claim_history`, which no naming read
//! looks at. This table is never swept, so a name stays used once it lands.
//!
//! Each machine still writes its local file first. Every append also lands
//! here, and every read pushes the local rows the primary lacks, so a
//! machine's older names migrate on its first naming read or daemon sweep.
//! The sweep also lands the primary's rows in the local file, so a peer's
//! bound name resolves here without a dial-out. A configured primary that
//! cannot be read refuses the sweep's sync read (its caller retries); the
//! naming read degrades to this machine's mirror with one stderr line, so
//! one offline machine cannot blind the lead beat. The mirror holds every
//! name the fleet's history had at the last sweep; a peer's name bound
//! during the outage can slip through, and the degrade is the trade the
//! 2026-11-19 r2d2 outage asked for.

use crate::store_remote::{Remote, SqlValue};
use crate::team_names::NameHistoryEntry;
use std::collections::BTreeSet;
use std::path::Path;

const DDL: &str = "CREATE TABLE IF NOT EXISTS name_history (
  name TEXT NOT NULL,
  scope TEXT NOT NULL,
  session TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL,
  ts TEXT NOT NULL,
  host TEXT NOT NULL,
  UNIQUE (name, scope, session, kind, ts, host)
);";

#[cfg(test)]
thread_local! {
    static TEST_PRIMARY: std::cell::RefCell<Option<Remote>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: route every store's ledger on this thread to `remote`, so two
/// tempdir stores play two machines sharing one primary.
#[cfg(test)]
pub(crate) fn route_to_primary(route: Option<Remote>) {
    TEST_PRIMARY.with(|p| *p.borrow_mut() = route);
}

/// The primary that holds the ledger for `name_ledger`'s reads. Only the
/// shared `$HOME/.fno/agents` store dials out, so a test or scratch home
/// never writes a name into the fleet's ledger. `pub(crate)` so the naming
/// ceremony resolves the primary itself and holds the malformed-config
/// refusal at its own existing string-error signature, while the outage
/// degrade stays a plain tuple (the R6 ratchet holds string-error results
/// shrink-only).
pub(crate) fn primary(store_path: &Path) -> Result<Option<Remote>, String> {
    #[cfg(test)]
    {
        let _ = store_path;
        Ok(TEST_PRIMARY.with(|p| p.borrow().clone()))
    }
    #[cfg(not(test))]
    {
        let Some(root) = store_path.parent() else {
            return Ok(None);
        };
        if crate::paths::AgentsHome::at(root).is_sandbox() {
            return Ok(None);
        }
        crate::store_remote::configured()
    }
}

/// One statement on the primary. The table is made on first use, the way
/// the claim tables are.
fn run(
    remote: &Remote,
    sql: &str,
    args: &[SqlValue],
) -> Result<crate::store_remote::Reply, String> {
    match remote.execute(sql, args) {
        Err(error) if error.contains("no such table") => {
            remote.script(DDL)?;
            remote.execute(sql, args)
        }
        reply => reply,
    }
}

/// One row's identity, in column order: the insert args.
fn key(entry: &NameHistoryEntry, host: &str) -> [String; 6] {
    let [name, scope, session, kind, ts] = row_id(entry);
    [name, scope, session, kind, ts, host.to_string()]
}

/// A row's identity without its host. The push dedups on this: a macOS
/// hostname moves with the network (u, u.local, Mac), and keying the
/// push on it re-sent this machine's whole history under each new label.
fn row_id(entry: &NameHistoryEntry) -> [String; 5] {
    [
        entry.name.to_ascii_lowercase(),
        entry.scope.clone(),
        entry.session.clone().unwrap_or_default(),
        entry.kind.clone(),
        entry.ts.clone(),
    ]
}

/// The machine label a row carries. macOS `LocalHostName` is the name the
/// user set and holds still; `gethostname` drifts with DHCP and VPN.
pub(crate) fn host_label() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            if let Ok(out) = std::process::Command::new("/usr/sbin/scutil")
                .args(["--get", "LocalHostName"])
                .output()
            {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if out.status.success() && !name.is_empty() {
                    return name;
                }
            }
            let host = crate::claims::hostname();
            host.strip_suffix(".local").unwrap_or(&host).to_string()
        })
        .clone()
}

/// This machine's mail bus host: the stable `host_label()`, normalized.
/// `gethostname(2)` drifts with DHCP, VPN and sleep and strands pending rows
/// addressed to the old spelling. Pull and the sender stamp both read this
/// one function, so they cannot disagree.
pub(crate) fn local_bus_host() -> String {
    fno_mail::address::normalize_host(&host_label())
}

/// Insert `entries` as this machine's rows. A row already there is skipped,
/// so a retry or a second backfill writes nothing twice.
fn insert(remote: &Remote, entries: &[&NameHistoryEntry]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let host = host_label();
    let args: Vec<SqlValue> = entries
        .iter()
        .flat_map(|e| key(e, &host).map(SqlValue::Text))
        .collect();
    let rows = vec!["(?,?,?,?,?,?)"; entries.len()].join(",");
    run(
        remote,
        &format!("INSERT OR IGNORE INTO name_history (name, scope, session, kind, ts, host) VALUES {rows}"),
        &args,
    )
    .map(drop)
}

/// Publish one appended entry. Best-effort: the local file already holds it,
/// and the next [`read_merged`] or [`sync`] pushes whatever this write lost.
pub(crate) fn publish(store_path: &Path, entry: &NameHistoryEntry) {
    let Ok(Some(remote)) = primary(store_path) else {
        return;
    };
    if let Err(e) = insert(&remote, &[entry]) {
        eprintln!(
            "name ledger: {} not published to {} ({e}); the next naming read retries",
            entry.name,
            remote.url()
        );
    }
}

/// Every row on the primary, each with the host that wrote it. `local` is
/// this machine's file; its rows the primary lacks are pushed first, so the
/// answer always covers this machine too. `Ok(empty)` when no primary is
/// configured.
pub(crate) fn read_merged(
    store_path: &Path,
    local: &[NameHistoryEntry],
) -> Result<Vec<NameHistoryEntry>, String> {
    let Some(remote) = primary(store_path)? else {
        return Ok(Vec::new());
    };
    let refuse = |e: String| {
        format!(
            "the fleet name ledger at {} cannot be read ({e}); a name is judged against every machine's history, so this naming decision refuses until store.remote_url answers",
            remote.url()
        )
    };
    let shared = select_all(&remote).map_err(refuse)?;
    Ok(merge_tail(&remote, store_path, shared, local))
}

/// The push/pull both reads run after an answered select: a failed push
/// still answers the read, the shared rows plus the caller's own local rows
/// cover every name this decision needs.
fn merge_tail(
    remote: &Remote,
    store_path: &Path,
    shared: Vec<NameHistoryEntry>,
    local: &[NameHistoryEntry],
) -> Vec<NameHistoryEntry> {
    if let Err(e) = push_missing(remote, &shared, local) {
        eprintln!("name ledger: backfill to {} failed ({e})", remote.url());
    }
    if let Err(e) = pull_missing(store_path, &shared, local) {
        eprintln!("name ledger: pull from {} failed ({e})", remote.url());
    }
    shared
}

/// [`read_merged`] with the outage degrade the naming ceremony runs on: an
/// unreadable primary answers the caller's own rows and returns the one-line
/// notice, so the beat runs instead of dying on a read that only feeds the
/// never-reuse judge. `remote` comes from [`primary`] at the caller, which
/// holds the malformed-`store.remote_url` refusal at its own `Result`: a
/// typo never falls back to the mirror in silence.
pub(crate) fn read_merged_or_mirror(
    remote: Option<&Remote>,
    store_path: &Path,
    local: &[NameHistoryEntry],
) -> (Vec<NameHistoryEntry>, Option<String>) {
    let Some(remote) = remote else {
        return (Vec::new(), None);
    };
    let shared = match select_all(remote) {
        Ok(rows) => merge_tail(remote, store_path, rows, local),
        Err(error) => {
            let notice = format!(
                "name ledger at {} is offline; this naming decision is judged against this machine's cached names until it returns ({error})",
                remote.url()
            );
            notice_once(&notice);
            return (Vec::new(), Some(notice));
        }
    };
    (shared, None)
}

/// The offline notice prints once per process: one naming ceremony makes
/// several ledger reads, and the beat's contract is one line.
fn notice_once(notice: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("name ledger: {notice}");
    }
}

/// Push every local row the primary lacks, and land the primary's rows this
/// machine's file lacks. The daemon sweep runs this, so a machine that names
/// nobody for a while still lands its history, and a peer's bound name
/// resolves here: a remote sender's rendered header names itself, and the
/// store that verifies that header has to hold the row the name came from.
pub fn sync(store_path: &Path, local: &[NameHistoryEntry]) -> Result<usize, String> {
    let Some(remote) = primary(store_path)? else {
        return Ok(0);
    };
    let shared = select_all(&remote)?;
    let pushed = push_missing(&remote, &shared, local)?;
    Ok(pushed + pull_missing(store_path, &shared, local)?)
}

/// The reverse of [`push_missing`]: append the shared rows the local file
/// lacks, host stamps included. Best-effort - the next sweep or naming read
/// retries what a failed write drops.
fn pull_missing(
    store_path: &Path,
    shared: &[NameHistoryEntry],
    local: &[NameHistoryEntry],
) -> Result<usize, String> {
    let have: BTreeSet<[String; 5]> = local.iter().map(row_id).collect();
    let missing: Vec<NameHistoryEntry> = shared
        .iter()
        .filter(|e| !have.contains(&row_id(e)))
        .cloned()
        .collect();
    let count = missing.len();
    for entry in missing {
        crate::team_names::history_append(store_path, entry)?;
    }
    Ok(count)
}

fn push_missing(
    remote: &Remote,
    shared: &[NameHistoryEntry],
    local: &[NameHistoryEntry],
) -> Result<usize, String> {
    let mut seen: BTreeSet<[String; 5]> = shared.iter().map(row_id).collect();
    let missing: Vec<&NameHistoryEntry> = local.iter().filter(|e| seen.insert(row_id(e))).collect();
    insert(remote, &missing)?;
    Ok(missing.len())
}

fn select_all(remote: &Remote) -> Result<Vec<NameHistoryEntry>, String> {
    let reply = run(
        remote,
        "SELECT name, scope, session, kind, ts, host FROM name_history ORDER BY ts",
        &[],
    )?;
    let cell = |row: &[SqlValue], i: usize| {
        row.get(i)
            .and_then(SqlValue::text)
            .unwrap_or_default()
            .to_string()
    };
    Ok(reply
        .rows
        .iter()
        .map(|row| {
            let session = cell(row, 2);
            NameHistoryEntry {
                name: cell(row, 0),
                scope: cell(row, 1),
                session: (!session.is_empty()).then_some(session),
                kind: cell(row, 3),
                ts: cell(row, 4),
                host: Some(cell(row, 5)),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_names;

    /// Two tempdir stores play the MacBook and the iMac on one primary. A
    /// name one machine bound refuses on the other, naming who and when; a
    /// row written before the ledger existed lands on the first sync; the
    /// sync read refuses on an unreachable primary while the naming read
    /// degrades to the mirror and still answers.
    #[test]
    fn a_name_used_on_one_machine_is_refused_on_the_other() {
        let macbook = tempfile::tempdir().unwrap();
        let imac = tempfile::tempdir().unwrap();
        let mac_store = macbook.path().join("team_names.json");
        let imac_store = imac.path().join("team_names.json");
        let bound = |name: &str, scope: &str, session: &str| {
            NameHistoryEntry::here(name, scope, Some(session.into()), "bound")
        };

        route_to_primary(None);
        team_names::history_append(&imac_store, bound("rowan", "x-aaaa", "sess-r")).unwrap();
        let primary = crate::store_remote::test_primary::start();
        route_to_primary(Some(primary.remote.clone()));
        assert_eq!(team_names::history_sync(&imac_store).unwrap(), 1);
        assert_eq!(team_names::history_sync(&imac_store).unwrap(), 0);
        // The same rows already landed under an older hostname: a moved
        // label pushes nothing twice.
        let rows = select_all(&primary.remote).unwrap();
        let relabelled: Vec<NameHistoryEntry> = rows
            .iter()
            .map(|e| NameHistoryEntry {
                host: Some("u.local".into()),
                ..e.clone()
            })
            .collect();
        assert_eq!(
            push_missing(&primary.remote, &relabelled, &rows).unwrap(),
            0
        );
        assert!(team_names::history_names(&mac_store)
            .unwrap()
            .contains("rowan"));

        team_names::history_append(&mac_store, bound("Tamsin", "x-bbbb", "sess-mac")).unwrap();
        let err = team_names::carry_holder_name(&imac_store, "sess-imac", 2, "x-cccc", "Tamsin")
            .unwrap_err();
        assert!(
            err.contains("Tamsin is retired") && err.contains("x-bbbb") && err.contains("sess-mac"),
            "{err}"
        );
        assert_eq!(
            team_names::carry_holder_name(&imac_store, "sess-imac", 2, "x-cccc", "Odile").unwrap(),
            Some("Odile".to_string())
        );
        assert!(team_names::history_names(&mac_store)
            .unwrap()
            .contains("odile"));

        route_to_primary(Some(crate::store_remote::test_primary::dead()));
        // The sync-side read refuses on an outage; the naming read degrades
        // to the mirror and still answers with this machine's rows, one
        // notice in hand.
        let err = read_merged(&mac_store, &[]).unwrap_err();
        assert!(err.contains("cannot be read"), "{err}");
        let dead = crate::store_remote::test_primary::dead();
        let (rows, notice) = read_merged_or_mirror(Some(&dead), &mac_store, &[]);
        assert!(rows.is_empty());
        let notice = notice.expect("the dead primary answers the one-line notice");
        assert!(
            notice.contains("offline") && notice.contains("cached names"),
            "{notice}"
        );
        let names = team_names::history_names(&mac_store).unwrap();
        assert!(
            names.contains("rowan") && names.contains("odile"),
            "{names:?}"
        );
        route_to_primary(None);
    }

    /// A name one machine bound resolves on a peer after its sweep lands the
    /// primary's rows in the peer's file: the ledger read behind the remote
    /// sender's header render and the receipt-side trust lookup is the local
    /// mirror, and the mirror has to hold what the header names.
    #[test]
    fn a_sweep_lands_a_peers_bound_name_in_the_local_mirror() {
        let macbook = tempfile::tempdir().unwrap();
        let imac = tempfile::tempdir().unwrap();
        let mac_store = macbook.path().join("team_names.json");
        let imac_store = imac.path().join("team_names.json");
        let bound = |name: &str, scope: &str, session: &str| {
            NameHistoryEntry::here(name, scope, Some(session.into()), "bound")
        };
        let primary = crate::store_remote::test_primary::start();
        route_to_primary(Some(primary.remote.clone()));
        // The iMac bound briar; the MacBook's mirror starts empty.
        team_names::history_append(&imac_store, bound("briar", "fno", "sess-briar")).unwrap();
        assert_eq!(team_names::history_sync(&mac_store).unwrap(), 1);
        assert!(
            team_names::history_all(&mac_store)
                .unwrap()
                .iter()
                .any(|e| e.name == "briar" && e.kind == "bound"),
            "the sweep pulled the peer's bound row into the local mirror"
        );
        // Converged: a second sweep moves nothing.
        assert_eq!(team_names::history_sync(&mac_store).unwrap(), 0);
        route_to_primary(None);
    }
}
