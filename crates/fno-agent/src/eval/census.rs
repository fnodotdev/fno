//! The running-process census : one row per long-lived process,
//! answering "is this running fno process an older build than the binary it
//! was launched from?". Python's `update.running_components` is a thin
//! adapter over `fno-agent census --json`; the walking and classifying live
//! here beside the drift classifier they reuse.
//!
//! Classifier sources, no third rule: a build SELF-REPORT (the keeper's
//! Identify reply carries `drift`, computed live from
//! [`crate::drift::self_drift`]) or, for a pre-report build, the process's
//! start time against its executable's mtime. A probe that cannot decide
//! reads `unknown` with the failure named, never `current`.

use crate::drift;
use crate::pid::Pid;
#[cfg(target_os = "macos")]
pub use crate::proc_table::{argv_of, comm_string};
pub use crate::proc_table::{
    hidden_process, parse_ps_row, pid_is_zombie, process_table, process_table_ps, ProcRow,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROBE_BUDGET: Duration = Duration::from_millis(750);

const TAG_IDENTIFY_GRAPH: u8 = 3; // graph_keeper.rs
const TAG_IDENTIFY_PANE: u8 = 4; // pane_keeper.rs Frame::Identify
const TAG_REPLY: u8 = 5; // both keepers

/// The shared Codex app-server's census row: health and installed-version
/// readiness are DIFFERENT axes, so the row carries both. A healthy daemon
/// running a version older than the installed CLI reads healthy + stale,
/// never healthy alone and never no row.
fn codex_app_server_rows() -> Vec<Value> {
    let readiness = crate::codex_daemon_readiness::codex_daemon_readiness();
    let verdict = match readiness.verdict {
        crate::codex_daemon_readiness::VersionVerdict::Current => "current",
        crate::codex_daemon_readiness::VersionVerdict::Stale => "stale",
        crate::codex_daemon_readiness::VersionVerdict::Ahead => "ahead",
        crate::codex_daemon_readiness::VersionVerdict::Unknown => "unknown",
    };
    let health = if readiness.healthy { "healthy" } else { "down" };
    let evidence = format!(
        "installed {}, live {}, {}, home {}",
        readiness
            .installed_version
            .as_deref()
            .unwrap_or("unreadable"),
        readiness.live_version.as_deref().unwrap_or("unreadable"),
        health,
        readiness.codex_home,
    );
    // No exe: the readiness reader knows the pid, never the binary path, and
    // the exe-position field must not carry a directory and read like one.
    let mut row = row(
        "codex-app-server",
        readiness.pid,
        Some("codex-app-server".to_string()),
        None,
        readiness.start_token.map(|t| t as f64),
        verdict,
        evidence.as_str(),
    );
    row["installed_version"] = json!(readiness.installed_version);
    row["live_version"] = json!(readiness.live_version);
    row["codex_home"] = json!(readiness.codex_home);
    vec![row]
}

/// Every process under `root` in one table read, found by walking each
/// row's ppid chain (at most 64 hops).
pub(crate) fn descendants(table: &[ProcRow], root: u32) -> Vec<u32> {
    let parent: std::collections::HashMap<u32, u32> =
        table.iter().map(|row| (row.pid, row.ppid)).collect();
    table
        .iter()
        .filter(|row| {
            let mut current = row.ppid;
            for _ in 0..64 {
                if current == root {
                    return true;
                }
                match parent.get(&current) {
                    Some(&next) if current > 1 => current = next,
                    _ => return false,
                }
            }
            false
        })
        .map(|row| row.pid)
        .collect()
}

#[cfg(test)]
pub(crate) fn test_proc_row(pid: u32, ppid: u32, command: &str) -> ProcRow {
    ProcRow {
        pid,
        ppid,
        state: 'S',
        elapsed_s: 1,
        cpu_pct: 0.0,
        rss_kb: 0,
        command: command.to_string(),
    }
}

/// True when a live process argv contains `needle`. One pass over the pid
/// list reading argv only, so a click path pays no per-thread census. This
/// process is skipped: its own argv carries the needle when the caller was
/// handed it on the command line.
pub fn live_argv_contains(needle: &str) -> bool {
    !needle.is_empty() && any_live_argv(|argv| argv.iter().any(|a| a.contains(needle)))
}

/// True when a live claude process holds `session_id`: its argv names the
/// session as the value of `--resume`, `-r` or `--session-id`. Narrower than
/// [`live_argv_contains`], which also matches an fno command that only
/// mentions the id (`fno agent resume <id>`) and holds nothing.
pub fn live_claude_holds_session(session_id: &str) -> bool {
    !session_id.is_empty() && any_live_argv(|argv| claude_argv_holds(argv, session_id))
}

fn claude_argv_holds(argv: &[String], session_id: &str) -> bool {
    let is_claude = argv
        .first()
        .is_some_and(|program| program.contains("claude"));
    is_claude
        && argv.windows(2).any(|pair| {
            matches!(pair[0].as_str(), "--resume" | "-r" | "--session-id") && pair[1] == session_id
        })
}

/// One pass over the pid list reading argv only, this process skipped.
#[cfg(target_os = "macos")]
fn any_live_argv(matches: impl Fn(&[String]) -> bool) -> bool {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return false;
    }
    let mut pids = vec![0u32; count as usize + 64];
    let filled = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast::<libc::c_void>(),
            (pids.len() * std::mem::size_of::<u32>()) as libc::c_int,
        )
    };
    if filled <= 0 {
        return false;
    }
    let me = std::process::id();
    pids[..filled as usize]
        .iter()
        .filter(|pid| **pid > 0 && **pid != me)
        .any(|&pid| argv_of(pid).is_some_and(|argv| matches(&argv)))
}

/// The `ps` leg splits each command on whitespace, as `process_argv` does.
#[cfg(not(target_os = "macos"))]
fn any_live_argv(matches: impl Fn(&[String]) -> bool) -> bool {
    let me = std::process::id();
    process_table_ps().0.iter().any(|r| {
        let argv: Vec<String> = r.command.split_whitespace().map(str::to_string).collect();
        r.pid != me && matches(&argv)
    })
}

/// The live argv of one pid, for the caller outside the census that needs
/// it: a pane-to-thread conversion carries the running writer's own pins
/// into the relaunch rather than re-deriving them from a default.
#[cfg(target_os = "macos")]
pub(crate) fn process_argv(pid: u32) -> Option<Vec<String>> {
    argv_of(pid)
}

/// The parent pid of `pid`, for the spend journal's caller walk: the same
/// read the table scan makes, one pid at a time.
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    Pid::new(pid).and_then(|p| p.parent_pid())
}

/// The `ps` leg. It splits on whitespace, so an argument that CONTAINS a
/// space comes back as two. Every flag the conversion carries takes a
/// space-free value, and the caller drops what it does not recognise.
#[cfg(not(target_os = "macos"))]
pub(crate) fn process_argv(pid: u32) -> Option<Vec<String>> {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    (!argv.is_empty()).then_some(argv)
}

fn format_elapsed(secs: u64) -> String {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let d = secs / 86_400;
    if d > 0 {
        format!("{d:02}-{h:02}:{m:02}:{s:02}")
    } else if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// One line per process is this table's whole contract, and an argv may hold
/// a newline that third-party launchers write. Escape the two characters that
/// break a line, the way `ps` itself does, so the reader sees the bytes a real
/// `ps` would have handed it.
fn escape_row_command(command: &str) -> std::borrow::Cow<'_, str> {
    if !command.contains(['\n', '\r']) {
        return std::borrow::Cow::Borrowed(command);
    }
    std::borrow::Cow::Owned(command.replace('\n', "\\012").replace('\r', "\\015"))
}

/// The table as the `ps -Ao pid,ppid,state,etime,%cpu,rss,command` text the
/// Python footprint reader already parses.
pub fn ps_text(rows: &[ProcRow]) -> String {
    let mut out = String::from("PID PPID STAT ELAPSED %CPU RSS COMMAND\n");
    for row in rows {
        out.push_str(&format!(
            "{} {} {} {} {:.1} {} {}\n",
            row.pid,
            row.ppid,
            row.state,
            format_elapsed(row.elapsed_s),
            row.cpu_pct,
            row.rss_kb,
            escape_row_command(&row.command)
        ));
    }
    out
}

fn started_epoch(etime: Option<f64>) -> Option<f64> {
    let etime = etime?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some(now - etime)
}

fn started_before_rewrite(started_at: Option<f64>, exe: Option<&Path>) -> bool {
    let Some(started) = started_at else {
        return false;
    };
    let Some(exe) = exe else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(exe) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    let Ok(nanos) = mtime.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    started < nanos.as_secs_f64()
}

/// One bounded frame round-trip: write `request`, read the reply frame
/// (`reply_tag`), parse its JSON payload. `None` on any failure.
fn frame_round_trip(sock: &Path, request: [u8; 5], reply_tag: u8) -> Option<Value> {
    use std::io::{Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(PROBE_BUDGET)).ok()?;
    stream.set_write_timeout(Some(PROBE_BUDGET)).ok()?;
    stream.write_all(&request).ok()?;
    let mut header = [0u8; 5];
    stream.read_exact(&mut header).ok()?;
    if header[0] != reply_tag {
        return None;
    }
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut payload = vec![0u8; len.min(1 << 20)];
    stream.read_exact(&mut payload).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// One Identify with a short bound; `Some` only when the keeper answers
/// with a parseable IdentifyReply. `identify_tag` selects the lane's frame
/// (the graph and pane keepers use different request tags, the same reply).
fn identify_reply(sock: &Path, identify_tag: u8) -> Option<Value> {
    frame_round_trip(sock, [identify_tag, 0, 0, 0, 0], TAG_REPLY)
}

/// The `on_restart` / `survives` pair, fixed per component so every surface
/// says the same thing.
fn fate(component: &str) -> (&'static str, &'static str) {
    match component {
        "daemon" => ("restarts", "workers and panes"),
        "store-keeper" => ("cycles; the next read respawns it", "the graph on disk"),
        "mux-server" => ("kept; only `--mux` replaces it", "its panes"),
        "codex-app-server" => (
            "safe upgrade only through the session-preserving transaction",
            "threads survive the daemon swap",
        ),
        _ => ("kept", "its pane; current only when that pane ends"),
    }
}

fn row(
    component: &str,
    pid: Option<u32>,
    name: Option<String>,
    exe: Option<String>,
    started_at: Option<f64>,
    verdict: &str,
    evidence: &str,
) -> Value {
    let (on_restart, survives) = fate(component);
    json!({
        "component": component,
        "pid": pid,
        "name": name,
        "exe": exe,
        "started_at": started_at,
        "verdict": verdict,
        "evidence": evidence,
        "on_restart": on_restart,
        "survives": survives,
    })
}

/// Keeper rows off the process table: argv[0] basename `fno-agent-worker`,
/// lane from the argv flag, socket and session from argv. Keepers sharing
/// one socket are listed as duplicates (change 2 retires them).
fn keeper_rows() -> Vec<Value> {
    let (table, _) = process_table();
    keeper_rows_from(&table)
}

fn keeper_rows_from(table: &[ProcRow]) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    for proc_row in table {
        let pid = proc_row.pid;
        let argv: Vec<&str> = proc_row.command.split_whitespace().collect();
        let Some(argv0) = argv.first() else {
            continue;
        };
        if Path::new(argv0)
            .file_name()
            .is_none_or(|n| n != "fno-agent-worker")
        {
            continue;
        }
        let flag = argv
            .iter()
            .find(|a| ["--pane", "--keeper", "--store-keeper"].contains(a));
        let component = match flag {
            Some(&"--store-keeper") => "store-keeper",
            Some(&"--keeper") => "thread-keeper",
            _ => "pane-keeper",
        };
        let sock = argv
            .windows(2)
            .find(|w| w[0] == "--sock")
            .map(|w| PathBuf::from(w[1]));
        let session = argv
            .windows(2)
            .find(|w| w[0] == "--session")
            .map(|w| w[1].to_string());
        let name = session
            .clone()
            .or_else(|| sock.as_ref().map(|s| s.display().to_string()));
        let started = started_epoch(Some(proc_row.elapsed_s as f64));
        let mut store_graph: Option<String> = None;
        let (verdict, evidence) = match sock.as_deref() {
            None => ("unknown", "argv declares no socket"),
            Some(sock) => {
                let tag = if component == "store-keeper" {
                    TAG_IDENTIFY_GRAPH
                } else {
                    TAG_IDENTIFY_PANE
                };
                match identify_reply(sock, tag) {
                    Some(reply) => {
                        if component == "store-keeper" {
                            store_graph =
                                reply.get("graph").and_then(Value::as_str).map(String::from);
                        }
                        match reply.get("drift").and_then(Value::as_str) {
                            Some("drifted") => ("stale", "build self-report"),
                            Some("fresh") => ("current", "build self-report"),
                            _ => {
                                // A reply with no drift key is a keeper built
                                // before the self-report; start time is the only
                                // reading it gives. An unreadable start time is
                                // no verdict, never current.
                                let exe = Path::new(argv0);
                                if started_before_rewrite(started, Some(exe)) {
                                    ("stale", "predates build self-report")
                                } else if started.is_some() {
                                    ("current", "started at-or-after the binary was written")
                                } else {
                                    ("unknown", "no readable start time")
                                }
                            }
                        }
                    }
                    None => ("unknown", "no Identify answer"),
                }
            }
        };
        let mut row = row(
            component,
            Some(pid),
            name,
            Some(argv0.to_string()),
            started,
            verdict,
            evidence,
        );
        if let Some(graph) = store_graph {
            row["graph"] = json!(graph);
        }
        if let Some(sock) = &sock {
            row["sock"] = json!(sock.display().to_string());
            if let Some(holder) = seen.get(&sock.display().to_string()) {
                row["evidence"] = json!(format!(
                    "{}; duplicate keeper on {} (seat held by pid {})",
                    row["evidence"].as_str().unwrap_or_default(),
                    sock.display(),
                    holder
                ));
            } else {
                seen.insert(sock.display().to_string(), pid);
            }
        }
        rows.push(row);
    }
    rows
}

/// The running daemon's pid from agent.status, `None` with no daemon.
async fn daemon_pid() -> Option<u32> {
    let resp = crate::client::call_if_running(
        &crate::paths::AgentsHome::from_env(),
        &crate::protocol::Request::new(1, "agent.status", json!({})),
    )
    .await
    .ok()?;
    let pid = resp.result()?.pointer("/daemon/pid")?.as_u64()?;
    Some(pid as u32)
}

/// The daemon's own and reaped-child CPU share over `window`, in cores: two
/// reads `window` apart. No daemon or an unreadable pid answers `null` with
/// a reason, never zeros.
async fn daemon_cpu(pid: Option<u32>, window: Duration) -> (Value, Value) {
    let Some(pid) = pid else {
        return (Value::Null, json!("no daemon answered agent.status"));
    };
    let start = std::time::Instant::now();
    let Some((own0, kids0)) = crate::reader_cost::cpu_split_ns(pid) else {
        return (
            Value::Null,
            json!(format!("pid {pid}: cpu time unreadable")),
        );
    };
    tokio::time::sleep(window).await;
    let Some((own1, kids1)) = crate::reader_cost::cpu_split_ns(pid) else {
        return (
            Value::Null,
            json!(format!("pid {pid}: exited during the window")),
        );
    };
    let secs = start.elapsed().as_secs_f64();
    let cores = |a: u64, b: u64| b.saturating_sub(a) as f64 / 1e9 / secs;
    let capacity = std::thread::available_parallelism().map_or(0, |n| n.get());
    let cpu = json!({
        "pid": pid,
        "own_cores": cores(own0, own1),
        "children_cores": cores(kids0, kids1),
        "window_s": secs,
        "capacity_cores": capacity,
    });
    (cpu, Value::Null)
}

/// The daemon's row, read in-process: one agent.status call feeds both the
/// pid and the drift verdict. A failed call reads unknown, never current.
async fn daemon_row() -> Value {
    use crate::client::{call_if_running, ClientError};
    use crate::protocol::Request;
    let resp = call_if_running(
        &crate::paths::AgentsHome::from_env(),
        &Request::new(1, "agent.status", json!({})),
    )
    .await;
    let pid = resp
        .as_ref()
        .ok()
        .and_then(|r| r.result())
        .and_then(|r| r.pointer("/daemon/pid"))
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let (verdict, evidence) = match resp {
        Ok(resp) => match resp.result() {
            Some(result) => {
                let drift = crate::client::drift_from_status(result);
                let label = drift::drift_label(&drift);
                match label {
                    "fresh" => ("current".to_string(), "build self-report".to_string()),
                    "drifted" => ("stale".to_string(), "build self-report".to_string()),
                    _ => (
                        "unknown".to_string(),
                        "status reply carries no drift verdict".to_string(),
                    ),
                }
            }
            None => (
                "unknown".to_string(),
                "status reply unparseable".to_string(),
            ),
        },
        Err(ClientError::DaemonNotRunning) => ("current".to_string(), "no daemon running".into()),
        Err(e) => ("unknown".to_string(), format!("status call failed: {e}")),
    };
    row(
        "daemon",
        pid,
        Some("agents home".into()),
        None,
        None,
        &verdict,
        &evidence,
    )
}

/// Mux server rows from the front door's own `ls --json`; the pid sidecar
/// field (change 4) is what the census classifies.
fn mux_rows(table: &[ProcRow]) -> Vec<Value> {
    let elapsed: BTreeMap<u32, u64> = table
        .iter()
        .map(|proc_row| (proc_row.pid, proc_row.elapsed_s))
        .collect();
    let started_of = |pid: u32| started_epoch(elapsed.get(&pid).map(|&secs| secs as f64));
    let Some(fno) = resolve_fno() else {
        return Vec::new();
    };
    let Ok(out) = std::process::Command::new(&fno)
        .args(["mux", "ls", "--json"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(rows) = serde_json::from_slice::<Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(list) = rows.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for r in list {
        if r.get("state").and_then(Value::as_str) != Some("live") {
            continue;
        }
        let session = r
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("unnamed");
        let _panes = r.get("panes").and_then(Value::as_u64).unwrap_or(0);
        let pid = r.get("pid").and_then(Value::as_u64).map(|p| p as u32);
        let (verdict, evidence) = match pid {
            Some(pid) => {
                let started = started_of(pid);
                if started_before_rewrite(started, Some(&fno)) {
                    ("stale", "predates build self-report")
                } else if started.is_some() {
                    ("current", "started at-or-after the binary was written")
                } else {
                    ("unknown", "no readable start time")
                }
            }
            None => ("unknown", "no pid sidecar"),
        };
        let mut row = row(
            "mux-server",
            pid,
            Some(session.to_string()),
            Some(fno.display().to_string()),
            started_of(pid.unwrap_or(0)),
            verdict,
            evidence,
        );
        // The kept/unkept split: kept panes survive the server (their keeper
        // re-adopts them); unkept ones end with it. The wording names both
        // counts so an operator sees exactly who a restart would cost.
        let mut kept = 0u64;
        let mut unkept = 0u64;
        if let Ok(pane_out) = std::process::Command::new(&fno)
            .args(["mux", "pane", "ls", "--session", session, "--json"])
            .output()
        {
            if let Ok(keeper_out) = std::process::Command::new(&fno)
                .args(["mux", "pane", "keeper", "list", "--json"])
                .output()
            {
                let keeper_rows: Vec<Value> =
                    serde_json::from_slice(&keeper_out.stdout).unwrap_or_default();
                let live_keepers: Vec<u64> = keeper_rows
                    .iter()
                    .filter(|k| k.get("session").and_then(Value::as_str) == Some(session))
                    .filter(|k| k.get("stale").is_none())
                    .filter_map(|k| k.get("child_pid").and_then(Value::as_u64))
                    .collect();
                if let Ok(pane_rows) = serde_json::from_slice::<Vec<Value>>(&pane_out.stdout) {
                    for pane in &pane_rows {
                        let child = pane.get("child_pid").and_then(Value::as_u64);
                        if child.is_some_and(|c| live_keepers.contains(&c)) {
                            kept += 1;
                        } else {
                            unkept += 1;
                        }
                    }
                }
            }
        }
        row["on_restart"] = json!(format!(
            "ending {unkept} unkept shell(s); keeps {kept} kept pane(s)"
        ));
        row["survives"] = json!(format!(
            "{kept} kept pane(s){}; {unkept} unkept",
            if kept + unkept == 0 {
                " (no panes)"
            } else {
                ""
            }
        ));
        out.push(row);
    }
    out
}

fn resolve_fno() -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cargo"));
    let candidate = cargo_home.join("bin").join("fno");
    if candidate.is_file() {
        return Some(candidate);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("fno"))
            .find(|p| p.is_file())
    })
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The census: daemon, keepers, mux servers. Bounded: every probe and every
/// subprocess carries a timeout, so a wedged keeper delays one row, never
/// the census.
pub async fn census() -> Vec<Value> {
    let (table, _) = process_table();
    let mut rows = vec![daemon_row().await];
    rows.extend(keeper_rows_from(&table));
    rows.extend(mux_rows(&table));
    rows.extend(codex_app_server_rows());
    rows
}

/// Run the daemon-free census subcommand.  The process walk stays in Rust so
/// Python callers and the machine sample share one table implementation.
pub async fn run_command(args: &[String]) -> i32 {
    // The sweep reads nest here as argv ops, so they add no fno-agent root
    // action (shrink law d-fe66560a).
    if let Some(code) = crate::census_reads::dispatch(args) {
        return code;
    }
    // `fno agent orphans`: the sweep rides the census action as an argument
    // (law d-fe66560a allows no new client action); the sweep body lives in
    // orphans.rs.
    if args.iter().any(|arg| arg == "--orphans") {
        let rest: Vec<String> = args
            .iter()
            .filter(|arg| *arg != "--orphans")
            .cloned()
            .collect();
        return crate::orphans::run(&rest);
    }
    // `fno doctor footprint --spawn-rate`: the spawn-rate reading the
    // sustained-CPU meter cannot see. The window sleeps between the two
    // table reads, so this arm is a doctor-shaped one-shot; the spawn gate
    // never reads it.
    if args.iter().any(|arg| arg == "--spawn-rate") {
        let json_flag = args.iter().any(|arg| arg == "--json" || arg == "-J");
        let secs = args
            .iter()
            .position(|arg| arg == "--secs")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok());
        let alarm = args
            .iter()
            .position(|arg| arg == "--spawn-rate-alarm")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<f64>().ok());
        return crate::footprint_cause::spawn_rate_command(json_flag, secs, alarm);
    }
    // `fno agent top`: the worker table, the census of live runs.
    if args.first().map(String::as_str) == Some("--workers") {
        return crate::agents_top::run(&args[1..]);
    }
    // The diag family's reading, riding the census op (shrink law d-fe66560a:
    // no new client action). `--cause` is the gate's bounded payload; the
    // full `fno doctor footprint` command behavior rides `--footprint`.
    if args
        .iter()
        .any(|arg| arg == "--cause" || arg == "--footprint")
    {
        let json_flag = args.iter().any(|arg| arg == "--json" || arg == "-J");
        let cause_only = args.iter().any(|arg| arg == "--cause");
        return crate::footprint_cause::footprint_command(json_flag, cause_only);
    }
    // The lanes reading rides the same census op: how many more lanes fit.
    if args.iter().any(|arg| arg == "--lanes") {
        let json_flag = args.iter().any(|arg| arg == "--json" || arg == "-J");
        return crate::fleet_lanes::lanes_command(json_flag);
    }
    // `fno doctor migrate ids` rides the census op too.
    if args.iter().any(|arg| arg == "--ids") {
        return crate::numbers::migrate::run(args);
    }
    // The disk reading rides the census op too: size per class with what
    // is safe to delete (shrink law: no new client action).
    if args.iter().any(|arg| arg == "--disk") {
        let json_flag = args.iter().any(|arg| arg == "--json" || arg == "-J");
        return crate::disk_report::disk_command(json_flag);
    }
    if args.iter().any(|arg| arg == "--ps") {
        // The 1 s window is opt-in: the spawn gate reads this table on
        // every admission and must not wait on it.
        let (cpu, cpu_reason) = if args.iter().any(|arg| arg == "--daemon-cpu") {
            daemon_cpu(daemon_pid().await, Duration::from_secs(1)).await
        } else {
            (Value::Null, json!("not read: pass --daemon-cpu"))
        };
        let (rows, unreadable) = process_table();
        println!(
            "{}",
            json!({
                "ps": ps_text(&rows),
                "unreadable": unreadable,
                "sccache_server_pid": crate::cargo_build_dirs::sccache_row_pid(&rows),
                "sccache_restarts_1h": crate::machine_watch::sccache_restarts_1h(),
                "daemon_cpu": cpu,
                "daemon_cpu_reason": cpu_reason,
                // Opt-in like the CPU window: the spawn gate's probe never
                // reads memory and must not pay the registry walk for it.
                "memory": if args.iter().any(|arg| arg == "--memory") {
                    crate::memory_groups::reading(&crate::paths::AgentsHome::from_env(), &rows)
                } else {
                    Value::Null
                },
            })
        );
        return 0;
    }
    let rows = census().await;
    println!(
        "{}",
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
    );
    0
}

/// Walk the keeper rows synchronously (tests, and callers already holding no
/// daemon context).
pub fn census_blocking() -> Vec<Value> {
    let (table, _) = process_table();
    let mut rows = Vec::new();
    rows.extend(keeper_rows_from(&table));
    rows.extend(mux_rows(&table));
    // A unit-test fixture roots its stores in a tempdir no keeper serves, so
    // the truth the gate needs is "a census ran and found no stale keeper";
    // dark would park every protocol row and starve the tests on a CI
    // runner, where the real table IS empty. Production keeps dark = Pending.
    #[cfg(test)]
    if rows.is_empty() {
        rows.push(serde_json::json!({
            "component": "store-keeper",
            "verdict": "fresh",
            "pid": std::process::id()
        }));
    }
    rows
}

/// One store keeper the restart command cycled (or spared).
pub struct CycledKeeper {
    pub graph: Option<String>,
    pub old_pid: Option<u32>,
    pub result: String,
}

/// Shutdown the stale store keepers the census found (change 6). No
/// respawn is attempted here: the next read respawns each keeper on the
/// installed binary - the same self-heal the fate text promises - and the
/// Python client's spawner owns the launch flags (read_source, events).
/// A keeper answering `busy` keeps its seat and is reported, not forced.
pub async fn cycle_stale_store_keepers() -> (Vec<CycledKeeper>, usize) {
    let rows = keeper_rows();
    let stale_panes = rows
        .iter()
        .filter(|r| {
            matches!(
                r["component"].as_str(),
                Some("pane-keeper") | Some("thread-keeper")
            ) && r["verdict"] == "stale"
        })
        .count();
    let mut out = Vec::new();
    for r in rows.iter() {
        if r["component"] != "store-keeper" || r["verdict"] != "stale" {
            continue;
        }
        let Some(sock) = r["sock"].as_str().map(PathBuf::from) else {
            continue;
        };
        let result = match shutdown_reply(&sock) {
            Some(reply) if reply.get("ok") == Some(&json!(true)) => {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while std::time::Instant::now() < deadline && sock.exists() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                "cycled".to_string()
            }
            Some(reply) => format!(
                "spared: {}",
                reply
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("mutation in flight")
            ),
            None => "spared: no shutdown answer".to_string(),
        };
        out.push(CycledKeeper {
            graph: r["graph"].as_str().map(String::from),
            old_pid: r["pid"].as_u64().map(|p| p as u32),
            result,
        });
    }
    (out, stale_panes)
}

/// Send one Shutdown frame and read the reply (tag 2 out, response tag 4).
fn shutdown_reply(sock: &Path) -> Option<Value> {
    frame_round_trip(sock, [2, 0, 0, 0, 0], 4)
}

#[cfg(test)]
mod process_table_tests {

    use super::ps_text;

    #[test]
    fn ps_text_escapes_a_newline_argv_to_one_line_per_row() {
        let rows = vec![
            super::ProcRow {
                pid: 101,
                ppid: 1,
                state: 'R',
                elapsed_s: 60,
                cpu_pct: 0.0,
                rss_kb: 1024,
                command: "tr -d \"\nmore\"\r".into(),
            },
            super::ProcRow {
                pid: 102,
                ppid: 1,
                state: 'S',
                elapsed_s: 60,
                cpu_pct: 0.0,
                rss_kb: 1024,
                command: "clean argv".into(),
            },
        ];
        let text = ps_text(&rows);
        assert_eq!(text.lines().count(), 3, "header plus one line per row");
        assert!(
            text.contains("101 1 R 01:00 0.0 1024 tr -d \"\\012more\"\\015"),
            "newline and CR carry as the four characters \\012 and \\015: {text}"
        );
    }
}
