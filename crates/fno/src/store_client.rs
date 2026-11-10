//! The mux's native path onto the ported graph store. The reorder
//! commands used to shell the `fno` porcelain server-side; the store is ported
//! now, so the server speaks the store keeper's framed protocol directly:
//! `u8 tag | u32 LE length | payload`, the pane keeper's shape (mirrored in
//! `pty.rs`), with the protocol version riding Identify.
//!
//! The socket is resolved exactly as the Python client
//! (`cli/src/fno/graph/store.py store_socket_for`) resolves it - a
//! `<graph>.store.sock` sibling, or a hashed short name under the platform
//! temp dir when the sibling would overrun the unix-socket address limit -
//! and a missing keeper is never spawned: the request rides the one-shot
//! `--store-exec` lane instead, because a resident keeper's memory grows
//! with the requests it serves. A refused store is a REFUSAL,
//! never an empty answer: the commands surface the failure as the card's
//! notice line.

#[cfg(feature = "full")]
use crate::node_row::KanbanColumn;
use crate::node_row::NodeRow;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Why a store request failed. `Display` prints the line a caller shows as
/// the card's notice, so a caller that only reports the failure keeps its
/// text through `String::from`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreClientError {
    /// No lane reached the store: the socket refused for a reason other
    /// than absence, or the one-shot worker could not be found or spawned.
    Unreachable(String),
    /// A read or write on an open lane failed.
    Io(String),
    /// The reply broke the protocol: a bad frame, bad JSON, or a missing
    /// field the request promises.
    Protocol(String),
    /// The keeper answered and refused; the keeper's own message.
    Refused(String),
    /// The store holds no node the request names.
    Missing(String),
    /// The one-shot worker ran past its deadline. A write may have landed.
    TimedOut { secs: u64 },
}

impl std::fmt::Display for StoreClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m)
            | Self::Io(m)
            | Self::Protocol(m)
            | Self::Refused(m)
            | Self::Missing(m) => f.write_str(m),
            Self::TimedOut { secs } => write!(
                f,
                "store-exec timed out after {secs}s; a write may not be confirmed, read before retrying"
            ),
        }
    }
}

impl std::error::Error for StoreClientError {}

impl From<StoreClientError> for String {
    fn from(e: StoreClientError) -> String {
        e.to_string()
    }
}

type Reply = Result<Value, StoreClientError>;

/// Why a socket connect failed. Absence and a dead listener are positive
/// proof the keeper is gone, so the request takes the exec lane.
enum ConnectError {
    Absent,
    NoListener,
    Other(String),
}

const TAG_REQUEST: u8 = 1;
const TAG_RESPONSE: u8 = 4;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const SOCK_PATH_LIMIT: usize = 96;

/// The store keeper socket for a graph file. Mirrors the Python client's
/// `store_socket_for` (the Python side is the path authority - it passes
/// `--sock`): a sibling socket, or a uid-keyed hashed name under the
/// platform temp dir when the sibling path would overrun the sun_path limit.
pub fn store_socket_for(graph: &Path) -> PathBuf {
    let dir = graph.parent().unwrap_or_else(|| Path::new("."));
    let name = graph
        .file_name()
        .map(|n| format!("{}.store.sock", n.to_string_lossy()))
        .unwrap_or_else(|| "graph.store.sock".to_string());
    let sibling = dir.join(&name);
    if sibling.to_string_lossy().len() <= SOCK_PATH_LIMIT {
        return sibling;
    }
    use sha2::Digest as _;
    let absolute = graph
        .canonicalize()
        .unwrap_or_else(|_| absolute_lexical(graph));
    let digest = sha2::Sha256::digest(absolute.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    // SAFETY: getuid reads a per-process kernel value; it cannot fail or race.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir()
        .join(format!("fno-store-{uid}"))
        .join(format!("{}.sock", &hex[..16]))
}

/// The absolute spelling a nonexistent path hashes to, matching the Python
/// client's non-strict `resolve()`: symlinks in the existing directory
/// prefix are followed, the missing tail is joined on lexically. A spelling
/// divergence here would make this client miss a keeper the Python client
/// spawned for the same graph and spawn a second one.
fn absolute_lexical(graph: &Path) -> PathBuf {
    let abs = if graph.is_absolute() {
        graph.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(graph)
    };
    if let Some(parent) = abs.parent() {
        if let Ok(resolved_parent) = parent.canonicalize() {
            if let Some(name) = abs.file_name() {
                return resolved_parent.join(name);
            }
        }
    }
    abs
}

pub(crate) fn worker_binary() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("FNO_AGENTS_WORKER") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(front) = std::env::var("FNO_AGENTS_FRONT") {
        let p = Path::new(&front).parent()?.join("fno-agents-worker");
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("fno-agents-worker");
    if p.is_file() {
        return Some(p);
    }
    which_worker()
}

fn which_worker() -> Option<PathBuf> {
    crate::product_boundary::find_on_path("fno-agents-worker")
}

fn connect(sock: &Path) -> Result<UnixStream, ConnectError> {
    // A signal-interrupted connect is retryable; reporting it would read the
    // live socket as unreachable for reasons the deadline exists to absorb.
    for _ in 0..4 {
        match UnixStream::connect(sock) {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ConnectError::Absent),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                return Err(ConnectError::NoListener)
            }
            Err(e) => return Err(ConnectError::Other(format!("unreachable: {e}"))),
        }
    }
    Err(ConnectError::Other(
        "unreachable: connect kept getting interrupted".into(),
    ))
}

/// The keeper's reply envelope: `result` on `ok`, the refusal otherwise.
fn open_reply(reply: Value) -> Reply {
    // Each field stays JSON, so an off-type `error` still reads as a
    // refusal rather than a protocol fault.
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Envelope {
        ok: Value,
        result: Value,
        error: Value,
    }
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Refusal {
        message: Value,
    }
    let envelope = Envelope::deserialize(reply).unwrap_or_default();
    if envelope.ok.as_bool() == Some(true) {
        return Ok(envelope.result);
    }
    let refusal = Refusal::deserialize(&envelope.error).unwrap_or_default();
    let message = refusal.message.as_str().unwrap_or("store refused");
    Err(StoreClientError::Refused(message.to_string()))
}

fn round_trip(stream: &mut UnixStream, method: &str, params: Value) -> Reply {
    let io = |e: std::io::Error| StoreClientError::Io(e.to_string());
    let payload = serde_json::to_vec(&json!({"id": 1, "method": method, "params": params}))
        .map_err(|e| StoreClientError::Protocol(e.to_string()))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(StoreClientError::Protocol(format!(
            "request frame of {} bytes exceeds the cap",
            payload.len()
        )));
    }
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(TAG_REQUEST);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))))
        .map_err(io)?;
    stream.write_all(&frame).map_err(io)?;
    // The read below surfaces a broken stream; a flush failure adds nothing.
    stream.flush().ok();

    let mut header = [0u8; 5];
    read_exact(stream, &mut header)?;
    if header[0] != TAG_RESPONSE {
        return Err(StoreClientError::Protocol(format!(
            "unexpected frame tag {} from keeper",
            header[0]
        )));
    }
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(StoreClientError::Protocol(format!(
            "oversized reply frame ({len} bytes)"
        )));
    }
    let mut body = vec![0u8; len];
    read_exact(stream, &mut body)?;
    let reply: Value =
        serde_json::from_slice(&body).map_err(|e| StoreClientError::Protocol(e.to_string()))?;
    open_reply(reply)
}

fn read_exact(stream: &mut UnixStream, buf: &mut [u8]) -> Result<(), StoreClientError> {
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(StoreClientError::Io(
                    "keeper closed the connection mid-frame".into(),
                ))
            }
            Ok(n) => filled += n,
            // A stray signal (another test's orphan reaper, a harness
            // delivery) interrupts the blocked read; std's own read_exact
            // retries EINTR, and so must this loop, or the gather reports a
            // bare "Interrupted system call" instead of riding its deadline.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StoreClientError::Io(e.to_string())),
        }
    }
    Ok(())
}

/// The store-exec deadline: the Python client's `_EXEC_TIMEOUT_S`
/// (cli/src/fno/graph/store.py). The two legs agree at 60s so a caller
/// cannot observe one lane hanging past the other's bound.
const EXEC_DEADLINE: Duration = Duration::from_secs(60);

/// One request through the `--store-exec` one-shot lane. The child serves it
/// through the keeper's own dispatch and exits, so a store with no resident
/// keeper never grows one (a keeper's memory grows with requests
/// served, ~3.6 GB/hour on the operator machine). Writer safety keeps the
/// bounded file flock the publish takes, which is cross-process; the
/// optimistic version/digest transaction is unchanged. The child reads
/// stdin to EOF before replying, so write-then-read cannot deadlock.
fn exec_call(graph: &Path, method: &str, params: Value) -> Reply {
    exec_call_within(graph, method, params, EXEC_DEADLINE)
}

/// [`exec_call`] with the deadline as a parameter, so a test can pass a
/// short one. `wait_with_output` is not enough: under load a contended graph
/// kept children alive past every caller bound, so this polls `try_wait`
/// and kills past the deadline. Stdout is drained on a helper thread: a
/// reply bigger than the pipe buffer would otherwise wedge the poll against
/// a child blocked on write.
fn exec_call_within(graph: &Path, method: &str, params: Value, deadline: Duration) -> Reply {
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::CommandExt;
    use StoreClientError::{Io, Protocol, Unreachable};

    let binary = worker_binary()
        .ok_or_else(|| Unreachable(crate::product_boundary::graph_worker_missing_error()))?;
    let request = json!({"id": 1, "method": method, "params": params});
    let mut child = std::process::Command::new(&binary)
        .args([
            "--store-exec",
            "--graph",
            graph.to_str().unwrap_or_default(),
        ])
        // Own process group: the deadline kill takes the worker's
        // descendants with it, or a grandchild holding the stdout pipe
        // keeps the drain joined past the caller's bound.
        .process_group(0)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            Unreachable(crate::product_boundary::graph_worker_spawn_error(
                &binary, e,
            ))
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| Io("cannot open the store-exec stdin".into()))?
        .write_all(request.to_string().as_bytes())
        .map_err(|e| {
            // The child died mid-write: reap it, or a long-lived mux server
            // accumulates one zombie per failed send.
            let _ = child.kill();
            let _ = child.wait();
            Io(format!("cannot send the store request: {e}"))
        })?;
    // stdin dropped here: the child sees EOF and answers.
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Io("cannot open the store-exec stdout".into()))?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout
            .read_to_end(&mut buf)
            .map(|_| buf)
            .map_err(|e| e.to_string())
    });
    let expires = std::time::Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= expires {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(Io(format!("store-exec failed: {e}")));
            }
        }
    };
    let Some(status) = status else {
        // Deadline fired: take down the whole group and reap, or a
        // long-lived mux server accumulates one zombie per timed-out read.
        // SAFETY: process_group(0) made the child a group leader, so its
        // pid is the pgid; killpg signals that group and cannot fail on a
        // live leader.
        unsafe {
            libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.wait();
        let _ = reader.join();
        return Err(StoreClientError::TimedOut {
            secs: deadline.as_secs(),
        });
    };
    let buf = reader
        .join()
        .map_err(|_| Io("store-exec reader thread panicked".into()))?
        .map_err(|e| Io(format!("store-exec failed: {e}")))?;
    if buf.is_empty() {
        return Err(Protocol(format!(
            "store-exec exited with code {} and no reply",
            status.code().unwrap_or(-1)
        )));
    }
    let reply: Value = serde_json::from_slice(&buf)
        .map_err(|e| Protocol(format!("store-exec reply is not JSON: {e}")))?;
    open_reply(reply)
}

/// One store request against `graph`: a live keeper's socket is preferred;
/// when the socket is positively dead, the request rides the one-shot exec
/// lane and no resident keeper is spawned. A caller whose error type is
/// `String` converts with `?` or `.map_err(String::from)`; the text is the
/// same as before the typed error.
pub fn call(graph: &Path, method: &str, params: Value) -> Reply {
    match connect(&store_socket_for(graph)) {
        Ok(mut stream) => round_trip(&mut stream, method, params),
        Err(ConnectError::Absent | ConnectError::NoListener) => exec_call(graph, method, params),
        Err(ConnectError::Other(e)) => Err(StoreClientError::Unreachable(e)),
    }
}

/// A finite, non-huge rank value (render._rank_band's rule): a poisoned
/// non-finite or giant-int peer rank is unranked, so it can never corrupt
/// the --top arithmetic or persist a NaN/inf rank.
#[cfg(feature = "full")]
fn ranked_rank(e: &NodeRow) -> Option<f64> {
    e.rank().filter(|v| v.is_finite() && v.abs() < 1e15)
}

/// `backlog rank <node> --top`, natively: min(ranked peer ranks in the
/// target's (column, project) lane) - 1.0, or 0.0 for an unranked lane.
/// Lanes come from this crate's own `kanban_column` mirror, computed
/// claim-blind: rank never changes a column, and a claimed peer's extra rank
/// only pushes the float slightly lower, never into another lane's band.
///
/// The min is computed from a `begin` snapshot, so the write carries that
/// snapshot's digest as `base_version`: the keeper refuses with a conflict
/// when a concurrent reorder moved the file in between, and this retries the
/// read-compute-write as one provably fresh step instead of floating a card
/// below a rank it never saw.
#[cfg(feature = "full")]
pub fn rank_top(graph: &Path, node_id: &str) -> Result<String, StoreClientError> {
    /// The `begin` snapshot: its digest and its rows.
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Begin {
        version: Value,
        entries: Value,
    }
    const ATTEMPTS: usize = 3;
    for attempt in 0..ATTEMPTS {
        let snap = Begin::deserialize(call(graph, "begin", json!({}))?).unwrap_or_default();
        let version = snap.version.as_str().ok_or_else(|| {
            StoreClientError::Protocol("the store returned no snapshot version".into())
        })?;
        let entries = match snap.entries {
            Value::Array(rows) => crate::node_row::rows_from_values(rows),
            _ => {
                return Err(StoreClientError::Protocol(
                    "the store returned no entries".into(),
                ))
            }
        };
        let target = entries
            .iter()
            .find(|e| e.id() == Some(node_id))
            .ok_or_else(|| StoreClientError::Missing(format!("no node resolves to '{node_id}'")))?;
        let project = target.project.as_deref().unwrap_or_default();
        let lane = backlog_column(target);
        let head = entries
            .iter()
            .filter(|e| e.id() != Some(node_id))
            .filter(|e| {
                backlog_column(e) == lane && e.project.as_deref().unwrap_or_default() == project
            })
            .filter_map(ranked_rank)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a: f64| a.min(v)))
            });
        let new_rank = head.map_or(0.0, |h| h - 1.0);
        let sent = call(
            graph,
            "op",
            json!({
                "name": "update_fields",
                "params": {
                    "node_id": node_id,
                    "fields": {"rank": new_rank}
                },
                "base_version": version,
            }),
        );
        match sent {
            Ok(_reply) => {
                return Ok(format!(
                    "float to top: {node_id} (rank {new_rank})",
                    new_rank = format_rank(new_rank)
                ))
            }
            // The keeper's Conflict message; a stable prefix from
            // graph_store::StoreError. Anything else is a real failure.
            Err(StoreClientError::Refused(m))
                if m.starts_with("version conflict") && attempt + 1 < ATTEMPTS =>
            {
                continue
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the retry loop returns on every branch")
}

#[cfg(feature = "full")]
fn format_rank(v: f64) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The kanban column for an entry, claim-blind (see [`rank_top`]).
#[cfg(feature = "full")]
fn backlog_column(e: &NodeRow) -> Option<KanbanColumn> {
    // `kanban_column` needs claim/underway facts the command deliberately does
    // not fetch; false for both is the unclaimed view the float targets.
    crate::backlog_view::kanban_column(e, false, false, None)
}

/// `backlog defer <node> --reason <why>`, natively: the keeper's defer op
/// mirrors the CLI's exact write (locks cleared, deferred_at stamped,
/// deferred_kind classified from the reason).
pub fn defer(graph: &Path, node_id: &str, reason: &str) -> Result<String, StoreClientError> {
    call(
        graph,
        "op",
        json!({
            "name": "defer",
            "params": {"node_id": node_id, "reason": reason}
        }),
    )?;
    Ok(format!("defer: {node_id}"))
}

/// `backlog advance --epic <node> --stop`, natively: clears the epic's
/// `mission_active` and dispatches nothing (LD6).
pub fn end_mission(graph: &Path, node_id: &str) -> Result<String, StoreClientError> {
    call(
        graph,
        "op",
        json!({
            "name": "end_mission",
            "params": {"node_id": node_id}
        }),
    )?;
    Ok(format!("end mission: {node_id}"))
}

// -- typed backlog API calls ------------------------------------------------
//
// Thin clients over the keeper's `api` command (one op per
// fno-agents `backlog::api` function, same name and fields). `rows` hands
// out `NodeRow`, this crate's lenient twin of the fno-agents model, which
// this crate deliberately does not depend on (they ship as separate
// binaries). `node` and `nodes` hand out `NodeRow` too. Each reply is
// read through a small struct whose fields stay JSON, so one off-type
// field never costs the rest.

/// The reply fields the `api` and `ready` ops answer with.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ApiReply {
    node: Value,
    nodes: Value,
    page_info: Value,
    rows: Value,
    version: Value,
    ids: Value,
    underway: Value,
    effective_priority: Value,
    flow: Option<Value>,
}

impl ApiReply {
    fn of(reply: Value) -> ApiReply {
        ApiReply::deserialize(reply).unwrap_or_default()
    }
}

/// The strings of a JSON array, others dropped.
fn strings(v: &Value) -> impl Iterator<Item = String> + '_ {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
}

/// Pagination info for a typed page of rows.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Page {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub end_cursor: Option<String>,
}

/// One page of node rows in store order, plus the cursor to resume at.
#[derive(Debug, Clone)]
pub struct NodeConnection {
    pub nodes: Vec<NodeRow>,
    pub page_info: Page,
}

/// One node by id, or None when the store holds no such row.
pub fn node(graph: &Path, id: &str) -> Result<Option<NodeRow>, StoreClientError> {
    let reply = ApiReply::of(call(graph, "api", json!({ "op": "node", "id": id }))?);
    Ok(match reply.node {
        Value::Object(m) => Some(NodeRow::from(m)),
        _ => None,
    })
}

/// A filtered, ordered, cursor-paged page of node rows.
#[allow(clippy::too_many_arguments)]
pub fn nodes(
    graph: &Path,
    filter: Value,
    first: Option<usize>,
    after: Option<&str>,
    include_archived: bool,
    order_by: Option<&str>,
    fields: Option<&[&str]>,
) -> Result<NodeConnection, StoreClientError> {
    let mut params = json!({ "op": "nodes", "filter": filter });
    if let Some(first) = first {
        params["first"] = json!(first);
    }
    if let Some(after) = after {
        params["after"] = json!(after);
    }
    if include_archived {
        params["include_archived"] = json!(true);
    }
    if let Some(order_by) = order_by {
        params["order_by"] = json!(order_by);
    }
    if let Some(fields) = fields {
        params["fields"] = json!(fields);
    }
    let reply = ApiReply::of(call(graph, "api", params)?);
    let nodes = match reply.nodes {
        Value::Array(nodes) => crate::node_row::rows_from_values(nodes),
        _ => Vec::new(),
    };
    let page_info = match reply.page_info {
        Value::Null => json!({}),
        info => info,
    };
    let page_info: Page = serde_json::from_value(page_info)
        .map_err(|e| StoreClientError::Protocol(format!("bad page_info: {e}")))?;
    Ok(NodeConnection { nodes, page_info })
}

/// The total row fold: every stored row, typed or not, so a fold over the
/// answer is total. A legacy row the model would reject still rides through
/// (the `rows` wire op's contract); the typed `nodes` op would drop it.
pub fn rows(
    graph: &Path,
    filter: Option<Value>,
    fields: Option<&[&str]>,
) -> Result<Vec<NodeRow>, StoreClientError> {
    let mut params = json!({ "op": "rows" });
    if let Some(filter) = filter {
        params["filter"] = filter;
    }
    if let Some(fields) = fields {
        params["fields"] = json!(fields);
    }
    Ok(match ApiReply::of(call(graph, "api", params)?).rows {
        Value::Array(rows) => crate::node_row::rows_from_values(rows),
        _ => Vec::new(),
    })
}

/// The store's mutation counter: one bump per write, legacy writers
/// included.
pub fn version(graph: &Path) -> Result<i64, StoreClientError> {
    let reply = ApiReply::of(call(graph, "api", json!({ "op": "version" }))?);
    reply
        .version
        .as_i64()
        .ok_or_else(|| StoreClientError::Protocol("the store returned no version".into()))
}

/// A crate-local parse of the keeper's board facts reply.
#[derive(Debug, Clone, Default)]
pub struct BoardFacts {
    pub ids: Vec<String>,
    pub underway: HashSet<String>,
    pub effective_priority: HashMap<String, String>,
}

/// The keeper's whole-graph order and column facts: the `ready` op in
/// board mode. A reply with no `ids` array names the deploy skew, so a
/// keeper that predates the board mode is an error line, never a silently
/// wrong order.
pub fn board_facts(
    graph: &Path,
    entries: Option<&[NodeRow]>,
    claimed: &[String],
) -> Result<BoardFacts, StoreClientError> {
    let mut params = json!({"board": true, "claimed": claimed});
    if let Some(e) = entries {
        params["entries"] = json!(e);
    }
    let reply = ApiReply::of(call(graph, "ready", params)?);
    if !reply.ids.is_array() {
        return Err(StoreClientError::Protocol(
            "the store keeper predates the board mode; run fno doctor update".into(),
        ));
    }
    let effective_priority = reply.effective_priority.as_object().map(|o| {
        o.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    });
    Ok(BoardFacts {
        ids: strings(&reply.ids).collect(),
        underway: strings(&reply.underway).collect(),
        effective_priority: effective_priority.unwrap_or_default(),
    })
}

/// The one delivery classifier's `flow` aggregate over the graph rows and
/// the ledger. A reply whose `flow` is absent or null is a protocol error:
/// a null flow names no aggregate, so it never reads as an empty one.
pub fn flow(
    graph: &Path,
    entries: &[NodeRow],
    ledger: &[Value],
) -> Result<Value, StoreClientError> {
    let reply = call(
        graph,
        "scoreboard_classify",
        json!({"entries": entries, "rows": ledger}),
    )?;
    ApiReply::of(reply)
        .flow
        .ok_or_else(|| StoreClientError::Protocol("the store classifier returned no flow".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A stub `fno-agents-worker` script: swallows stdin, answers a fixed
    /// reply envelope. Enough to prove the dead-socket route execs instead
    /// of spawning a resident keeper; the lane's serving half is proven in
    /// graph_keeper's own suite.
    fn stub_worker(dir: &Path, name: &str, body: &str) -> PathBuf {
        let script = dir.join(name);
        std::fs::write(&script, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn a_dead_socket_routes_through_exec_and_never_spawns() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-store-exec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let graph = dir.join("graph.json");
        std::env::set_var(
            "FNO_AGENTS_WORKER",
            stub_worker(
                &dir,
                "worker-ok",
                "printf '{\"ok\":true,\"result\":{\"backend\":\"sqlite\",\"version\":\"v1\"}}\n'",
            ),
        );
        let reply = call(&graph, "export_status", json!({})).unwrap();
        assert_eq!(reply["backend"], json!("sqlite"));
        assert!(
            !dir.join("graph.json.store.sock").exists(),
            "no resident keeper was minted, so no socket exists"
        );
        std::env::remove_var("FNO_AGENTS_WORKER");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The deadline bounds a slow child and never costs a healthy one: a
    /// stub past the deadline dies with a `timed out` error inside the
    /// caller's bound (the process-group kill reaps it), and an instant
    /// reply rides any deadline unchanged.
    #[test]
    fn the_exec_deadline_bounds_slow_children_and_rides_instant_ones() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-store-exec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let graph = dir.join("graph.json");
        std::env::set_var(
            "FNO_AGENTS_WORKER",
            stub_worker(
                &dir,
                "worker-instant",
                "printf '{\"ok\":true,\"result\":{\"backend\":\"sqlite\",\"version\":\"v1\"}}\n'",
            ),
        );
        let reply =
            exec_call_within(&graph, "export_status", json!({}), Duration::from_secs(1)).unwrap();
        assert_eq!(reply["backend"], json!("sqlite"));
        std::env::set_var(
            "FNO_AGENTS_WORKER",
            stub_worker(&dir, "worker-slow", "sleep 5\n"),
        );
        let started = std::time::Instant::now();
        let err = exec_call_within(&graph, "export_status", json!({}), Duration::from_secs(1))
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "took {:?}, the caller's 3s proof bound",
            started.elapsed()
        );
        assert_eq!(err, StoreClientError::TimedOut { secs: 1 }, "{err}");
        std::env::remove_var("FNO_AGENTS_WORKER");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
