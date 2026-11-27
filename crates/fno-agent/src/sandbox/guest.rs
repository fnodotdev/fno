//! `fno-agent hook sandbox-guest`: the container half of the seam.
//!
//! The host starts it with `engine exec -i`; its stdin and stdout are the
//! relay stream. It listens at the pinned socket path, so a client inside
//! needs no change, and turns each request into one of the allowed frames.
//! Anything else is refused here and never crosses the stream. It also
//! serves the HTTP proxy, tails the harness transcript, and hands the launch
//! env to `sandbox-guest exec` once. On stream EOF it kills the harness
//! process group: a killed host exec client does not kill guest processes.

use super::{Refusal, Res};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::wire::{GuestFrame, HostFrame, ReportState, CHUNK};
use super::{CONTAINER_SETTINGS, CONTAINER_SOCK, LAUNCH_METHOD, PROXY_ADDR};

const REPLY_BOUND: Duration = Duration::from_secs(15);
/// A loop op may push, open a PR or run the Stop decision on the host.
const OP_BOUND: Duration = Duration::from_secs(900);
const TAIL_EVERY: Duration = Duration::from_secs(2);
const MAX_REQUEST: u32 = 1024 * 1024;

#[derive(Default)]
struct Launch {
    env: Vec<(String, Option<String>)>,
    handed: bool,
    pid: Option<i32>,
}

struct Shared {
    out: Sender<GuestFrame>,
    replies: Mutex<HashMap<u32, Sender<(bool, String)>>>,
    streams: Mutex<HashMap<u32, Sender<Option<Vec<u8>>>>>,
    connects: Mutex<HashMap<u32, Sender<(bool, String)>>>,
    next: AtomicU32,
    seq: AtomicU64,
    launch: Mutex<Launch>,
}

impl Shared {
    fn id(&self) -> u32 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a request frame and wait for its reply.
    fn ask(&self, make: impl FnOnce(u32) -> GuestFrame) -> (bool, String) {
        self.ask_within(REPLY_BOUND, make)
    }

    fn ask_within(&self, bound: Duration, make: impl FnOnce(u32) -> GuestFrame) -> (bool, String) {
        let req = self.id();
        let (tx, rx) = mpsc::channel();
        self.replies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req, tx);
        if self.out.send(make(req)).is_err() {
            return (false, "the relay stream is closed".into());
        }
        let answer = rx.recv_timeout(bound).unwrap_or((
            false,
            format!("the host did not answer in {}s", bound.as_secs()),
        ));
        self.replies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&req);
        answer
    }
}

/// The entry. `exec -- <argv>` is the harness launcher; no argument is the
/// relay guest.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("exec") => exec(args.get(1..).unwrap_or(&[])),
        Some("probe") => probe(),
        None => serve(),
        Some(other) => {
            eprintln!("fno-agent hook sandbox-guest: unknown argument {other:?}");
            2
        }
    }
}

fn serve() -> i32 {
    let mut stdin = std::io::stdin().lock();
    let (env, settings, transcript_dir) = match HostFrame::read(&mut stdin) {
        Ok(Some(HostFrame::Hello {
            env,
            settings,
            transcript_dir,
        })) => (env, settings, transcript_dir),
        other => {
            eprintln!("sandbox-guest: the first frame must be hello, got {other:?}");
            return 2;
        }
    };
    if let Some(body) = settings {
        if let Err(e) = write_private(Path::new(CONTAINER_SETTINGS), &body) {
            eprintln!("sandbox-guest: settings copy: {e}");
            return 2;
        }
    }
    let (out, out_rx) = mpsc::channel::<GuestFrame>();
    std::thread::spawn(move || write_frames(out_rx));
    let shared = Arc::new(Shared {
        out,
        replies: Mutex::default(),
        streams: Mutex::default(),
        connects: Mutex::default(),
        next: AtomicU32::new(1),
        seq: AtomicU64::new(1),
        launch: Mutex::new(Launch {
            env,
            ..Launch::default()
        }),
    });
    if let Err(e) = start_listeners(&shared) {
        eprintln!("sandbox-guest: {e}");
        return 2;
    }
    let tail = Arc::new(Mutex::new(Tail::new(transcript_dir.map(PathBuf::from))));
    {
        let (shared, tail) = (shared.clone(), tail.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(TAIL_EVERY);
            tail.lock()
                .unwrap_or_else(|e| e.into_inner())
                .pump(&shared.out);
        });
    }
    read_host(&mut stdin, &shared, &tail);
    // Stream EOF: the host is gone, so the harness goes with it.
    if let Some(pid) = shared.launch.lock().unwrap_or_else(|e| e.into_inner()).pid {
        // SAFETY: plain kill(2) on the process group the launcher made.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    // The listener threads hold senders, so the writer never drains to an
    // end; the host is gone, so nothing is left to write to.
    0
}

/// The pinned client socket and the proxy, each on its own accept thread.
fn start_listeners(shared: &Arc<Shared>) -> Res<()> {
    let listener = bind_socket(Path::new(CONTAINER_SOCK))
        .map_err(|e| format!("listen {CONTAINER_SOCK}: {e}"))?;
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let shared = shared.clone();
                std::thread::spawn(move || serve_client(&shared, conn));
            }
        });
    }
    match TcpListener::bind(PROXY_ADDR) {
        Ok(proxy) => {
            let shared = shared.clone();
            std::thread::spawn(move || {
                for conn in proxy.incoming().flatten() {
                    let shared = shared.clone();
                    std::thread::spawn(move || serve_proxy(&shared, conn));
                }
            });
        }
        Err(e) => eprintln!("sandbox-guest: proxy {PROXY_ADDR}: {e}; the worker has no egress"),
    }
    Ok(())
}

/// Host frames until the relay stream ends.
fn read_host(stdin: &mut impl Read, shared: &Shared, tail: &Mutex<Tail>) {
    loop {
        match HostFrame::read(stdin) {
            Ok(Some(HostFrame::Reply { req, ok, body })) => {
                if let Some(tx) = shared
                    .replies
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&req)
                {
                    tx.send((ok, body)).ok();
                }
            }
            Ok(Some(HostFrame::Connected { stream, ok, reason })) => {
                if let Some(tx) = shared
                    .connects
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stream)
                {
                    tx.send((ok, reason)).ok();
                }
            }
            Ok(Some(HostFrame::Data { stream, bytes })) => {
                if let Some(tx) = shared
                    .streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&stream)
                {
                    tx.send(Some(bytes)).ok();
                }
            }
            Ok(Some(HostFrame::Close { stream })) => {
                if let Some(tx) = shared
                    .streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stream)
                {
                    tx.send(None).ok();
                }
            }
            Ok(Some(HostFrame::Shutdown)) => {
                tail.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pump(&shared.out);
                shared.out.send(GuestFrame::Flushed).ok();
            }
            Ok(Some(HostFrame::Hello { .. })) => {
                eprintln!("sandbox-guest: a second hello is ignored");
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("sandbox-guest: relay stream: {e}");
                break;
            }
        }
    }
}

fn write_frames(rx: Receiver<GuestFrame>) {
    let mut stdout = std::io::stdout().lock();
    for frame in rx {
        if frame.write(&mut stdout).is_err() {
            return;
        }
    }
}

fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::remove_file(path).ok();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(body)
}

fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::remove_file(path).ok();
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

fn read_request(conn: &mut UnixStream) -> Option<Value> {
    let mut len = [0u8; 4];
    conn.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len);
    if len > MAX_REQUEST {
        return None;
    }
    let mut body = vec![0u8; len as usize];
    conn.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_response(conn: &mut UnixStream, id: u64, answer: Res<Value>) {
    let reply = match answer {
        Ok(result) => json!({"id": id, "result": result}),
        Err(message) => {
            json!({"id": id, "error": {"code": "invalid_params", "message": message.0}})
        }
    };
    let body = serde_json::to_vec(&reply).unwrap_or_default();
    conn.write_all(&(body.len() as u32).to_le_bytes()).ok();
    conn.write_all(&body).ok();
}

/// One client request: translate to a frame, or refuse it here.
fn serve_client(shared: &Shared, mut conn: UnixStream) {
    conn.set_read_timeout(Some(REPLY_BOUND)).ok();
    let Some(req) = read_request(&mut conn) else {
        return;
    };
    let Ok(crate::protocol::Request { id, method, params }) = serde_json::from_value(req) else {
        return;
    };
    let answer = match method.as_str() {
        "agent.status" => relay(shared.ask(|req| GuestFrame::Status { req })),
        "agent.report" => match params
            .get("state")
            .and_then(Value::as_str)
            .and_then(ReportState::parse)
        {
            Some(state) => relay(shared.ask(|req| GuestFrame::Report { req, state })),
            None => {
                Err("the sandbox forwards agent.report with state working, blocked or done".into())
            }
        },
        "agent.session-report" => {
            let source = params
                .get("source")
                .or_else(|| params.pointer("/payload/source"))
                .and_then(Value::as_str)
                .unwrap_or("startup")
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(32)
                .collect();
            relay(shared.ask(|req| GuestFrame::SessionReport { req, source }))
        }
        LAUNCH_METHOD => launch(shared, &params),
        super::ops::OP_METHOD => match serde_json::from_value::<super::ops::Op>(params) {
            Ok(op) => match op.check() {
                Ok(()) => {
                    let seq = shared.seq.fetch_add(1, Ordering::Relaxed);
                    relay(shared.ask_within(OP_BOUND, |req| GuestFrame::Op { req, seq, op }))
                }
                Err(e) => Err(e),
            },
            Err(e) => Err(super::ops::refused(&format!(
                "{} ({e})",
                super::ops::OP_METHOD
            ))),
        },
        other => Err(Refusal(format!(
            "the sandbox refuses `{}`: a sandboxed worker may call agent.status, \
             agent.report, agent.session-report and {} only",
            other.escape_debug(),
            super::ops::OP_METHOD
        ))),
    };
    write_response(&mut conn, id, answer);
}

fn relay((ok, body): (bool, String)) -> Res<Value> {
    if ok {
        Ok(serde_json::from_str(&body).unwrap_or(Value::String(body)))
    } else {
        Err(Refusal(body))
    }
}

/// The launch env, handed once to the launcher that names its own pid.
fn launch(shared: &Shared, params: &Value) -> Res<Value> {
    let pid = params
        .get("pid")
        .and_then(Value::as_i64)
        .and_then(|p| i32::try_from(p).ok())
        .filter(|p| *p > 1)
        .ok_or("the launch request names no pid")?;
    let mut launch = shared.launch.lock().unwrap_or_else(|e| e.into_inner());
    if launch.handed {
        return Err("the launch env was already handed out".into());
    }
    launch.handed = true;
    launch.pid = Some(pid);
    let env: Vec<Value> = launch.env.iter().map(|(k, v)| json!([k, v])).collect();
    Ok(json!({ "env": env }))
}

/// `sandbox-guest probe`: one status round trip through the pinned socket,
/// the guest and the host relay. The host runs it before any harness starts.
fn probe() -> i32 {
    let deadline = std::time::Instant::now() + REPLY_BOUND;
    while std::time::Instant::now() < deadline {
        let answer = crate::protocol::blocking_call(
            Path::new(CONTAINER_SOCK),
            "agent.status",
            json!({}),
            Duration::from_secs(5),
        );
        if answer.and_then(|v| v.get("sandboxed").cloned()) == Some(Value::Bool(true)) {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("sandbox-guest probe: no status answer through {CONTAINER_SOCK}");
    1
}

/// `sandbox-guest exec -- <argv>`: fetch the env once, lead a new process
/// group the guest can kill, and exec the harness with stdio untouched.
fn exec(args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let argv = match args.first().map(String::as_str) {
        Some("--") => args.get(1..).unwrap_or(&[]),
        _ => args,
    };
    let Some((prog, rest)) = argv.split_first() else {
        eprintln!("sandbox-guest exec: no command");
        return 2;
    };
    // SAFETY: setsid in a single-threaded process before exec.
    unsafe {
        libc::setsid();
    }
    let pid = std::process::id();
    let reply = crate::protocol::blocking_call(
        Path::new(CONTAINER_SOCK),
        LAUNCH_METHOD,
        json!({ "pid": pid }),
        REPLY_BOUND,
    );
    let Some(env) = reply
        .as_ref()
        .and_then(|r| r.get("env"))
        .and_then(Value::as_array)
    else {
        eprintln!("sandbox-guest exec: the guest handed no launch env");
        return 84;
    };
    let mut cmd = std::process::Command::new(prog);
    cmd.args(rest);
    for pair in env {
        match (pair.get(0).and_then(Value::as_str), pair.get(1)) {
            (Some(k), Some(Value::String(v))) => {
                cmd.env(k, v);
            }
            (Some(k), _) => {
                cmd.env_remove(k);
            }
            _ => {}
        }
    }
    let err = cmd.exec();
    eprintln!("sandbox-guest exec: {prog}: {err}");
    127
}

/// The HTTP proxy: `CONNECT host:port` tunnels, an absolute-form request is
/// re-sent in origin form. Each becomes one connect frame; the host checks it.
/// One proxied request's target: host, port, and the rewritten request
/// head for plain HTTP (`None` for CONNECT).
type Target = (String, u16, Option<Vec<u8>>);

/// Read the request head and name its target. `None` after a refusal the
/// client has already been sent, or a closed connection.
fn read_target(reader: &mut BufReader<TcpStream>, write_half: &mut TcpStream) -> Option<Target> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let end = line == b"\r\n" || line == b"\n";
        head.extend_from_slice(&line);
        if end || head.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let first = text.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("HTTP/1.1"),
    );
    if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = split_host_port(target, 443);
        return Some((h, p, None));
    }
    if let Some(rest) = target.strip_prefix("http://") {
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let (h, p) = split_host_port(authority, 80);
        let rewritten = text.replacen(first, &format!("{method} /{path} {version}"), 1);
        return Some((h, p, Some(rewritten.into_bytes())));
    }
    write_half
        .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
        .ok();
    None
}

/// Client bytes (anything buffered first) to the host, then the close.
fn pump_out(shared: &Shared, mut reader: BufReader<TcpStream>, stream: u32) {
    reader.get_ref().set_read_timeout(None).ok();
    let mut buf = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or_default().to_vec();
                if shared.out.send(GuestFrame::Data { stream, bytes }).is_err() {
                    break;
                }
            }
        }
    }
    shared.out.send(GuestFrame::Close { stream }).ok();
}

fn serve_proxy(shared: &Shared, conn: TcpStream) {
    conn.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let Ok(mut write_half) = conn.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(conn);
    let Some((host, port, preamble)) = read_target(&mut reader, &mut write_half) else {
        return;
    };
    let stream = shared.id();
    let (ctx, crx) = mpsc::channel();
    shared
        .connects
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(stream, ctx);
    let (dtx, drx) = mpsc::channel();
    shared
        .streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(stream, dtx);
    shared
        .out
        .send(GuestFrame::Connect { stream, host, port })
        .ok();
    let (ok, reason) = crx
        .recv_timeout(REPLY_BOUND)
        .unwrap_or((false, "the host did not answer the connect".into()));
    if !ok {
        shared
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&stream);
        let body = format!("fno sandbox proxy refused: {reason}\n");
        write!(
            write_half,
            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .ok();
        return;
    }
    match preamble {
        None => {
            write_half
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .ok();
        }
        Some(bytes) => {
            shared.out.send(GuestFrame::Data { stream, bytes }).ok();
        }
    }
    // Inbound: host data to the client until the host closes.
    std::thread::spawn(move || {
        for chunk in drx {
            match chunk {
                Some(bytes) if write_half.write_all(&bytes).is_ok() => {}
                _ => break,
            }
        }
        write_half.shutdown(std::net::Shutdown::Both).ok();
    });
    pump_out(shared, reader, stream);
}

fn split_host_port(authority: &str, default: u16) -> (String, u16) {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((h, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default);
            return (h.to_string(), port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(0)),
        _ => (authority.to_string(), default),
    }
}

/// The transcript tailer: the newest `*.jsonl` under the harness store, read
/// from where the last pump stopped. It keeps the first file it chose.
struct Tail {
    dir: Option<PathBuf>,
    file: Option<PathBuf>,
    offset: u64,
}

impl Tail {
    fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            file: None,
            offset: 0,
        }
    }

    fn pump(&mut self, out: &Sender<GuestFrame>) {
        if self.file.is_none() {
            self.file = self.dir.as_deref().and_then(newest_jsonl);
        }
        let Some(path) = self.file.as_ref() else {
            return;
        };
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        use std::io::Seek as _;
        if f.seek(std::io::SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = vec![0u8; CHUNK];
        loop {
            match f.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let frame = GuestFrame::Transcript {
                        offset: self.offset,
                        bytes: buf.get(..n).unwrap_or_default().to_vec(),
                    };
                    if out.send(frame).is_err() {
                        return;
                    }
                    self.offset += n as u64;
                }
            }
        }
    }
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| at > *t) {
                    best = Some((at, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client door refuses every op outside the allowlist by name, before
    /// any frame crosses; the launch env hands out once, to a named pid.
    #[test]
    fn client_door_refuses_unlisted_ops_and_hands_the_launch_once() {
        let (out, rx) = mpsc::channel();
        let shared = Shared {
            out,
            replies: Mutex::default(),
            streams: Mutex::default(),
            connects: Mutex::default(),
            next: AtomicU32::new(1),
            seq: AtomicU64::new(1),
            launch: Mutex::new(Launch {
                env: vec![("ROUTE_KEY".into(), Some("k".into()))],
                ..Launch::default()
            }),
        };
        for method in [
            "agent.spawn",
            "agent.ask",
            "agent.stop",
            "agent.rm",
            "agent.list",
            "agent.deliver",
            "channel.push_to_channel",
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let body =
                serde_json::to_vec(&json!({"id": 3, "method": method, "params": {"argv": ["sh"]}}))
                    .unwrap();
            client
                .write_all(&(body.len() as u32).to_le_bytes())
                .unwrap();
            client.write_all(&body).unwrap();
            serve_client(&shared, server);
            let mut len = [0u8; 4];
            client.read_exact(&mut len).unwrap();
            let mut reply = vec![0u8; u32::from_le_bytes(len) as usize];
            client.read_exact(&mut reply).unwrap();
            let reply: Value = serde_json::from_slice(&reply).unwrap();
            let msg = reply
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap();
            assert!(msg.contains(method), "{method}: {msg}");
        }
        for op in [
            json!({"op": "merge"}),
            json!({"op": "attest"}),
            json!({"op": "spawn", "argv": ["sh"]}),
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let body = serde_json::to_vec(
                &json!({"id": 4, "method": super::super::ops::OP_METHOD, "params": op}),
            )
            .unwrap();
            client
                .write_all(&(body.len() as u32).to_le_bytes())
                .unwrap();
            client.write_all(&body).unwrap();
            serve_client(&shared, server);
            let mut len = [0u8; 4];
            client.read_exact(&mut len).unwrap();
            let mut reply = vec![0u8; u32::from_le_bytes(len) as usize];
            client.read_exact(&mut reply).unwrap();
            let reply: Value = serde_json::from_slice(&reply).unwrap();
            let msg = reply.pointer("/error/message").and_then(Value::as_str);
            assert!(
                msg.is_some_and(|m| m.contains("lead does")),
                "{op}: {reply}"
            );
        }
        assert!(rx.try_recv().is_err(), "no refused op crosses the stream");
        assert!(launch(&shared, &json!({})).is_err());
        let env = launch(&shared, &json!({"pid": 42})).unwrap();
        assert_eq!(env, json!({"env": [["ROUTE_KEY", "k"]]}));
        assert!(launch(&shared, &json!({"pid": 43})).is_err(), "once only");
        assert_eq!(split_host_port("[::1]:8443", 443), ("::1".into(), 8443));
        assert_eq!(
            split_host_port("example.com", 80),
            ("example.com".into(), 80)
        );
    }
}
