//! `-H muse --substrate thread`: start the host, register the row, and the
//! row's mail, ask and stop. The host itself is host.rs.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::host::{child_file, host_call, Spec, CALL_WAIT};
use super::{outcome, resolve_muse_bin};
use crate::claude_ask::AskOutcome;
use crate::paths::AgentsHome;
use crate::protocol::{Request, Response};
use crate::state::{load_registry, update_registry, RegistryEntry};

const READY_WAIT: Duration = Duration::from_secs(40);

/// exec's `--approval-mode` names to the wire's `ApprovalMode`.
pub fn wire_mode(mode: &str) -> Option<&'static str> {
    match mode {
        "untrusted" => Some("promptUnmatched"),
        "on-request" => Some("onRequest"),
        "never" => Some("allowAll"),
        _ => None,
    }
}

/// The `muse serve` argv for a thread, its wire approval mode, and the turn
/// effort. serve has no `--yolo`, approval mode (`--approval-mode`, or the
/// spawn door's `--permission-mode`) or `--reasoning-effort`,
/// so those three are mapped; every other fenced token rides verbatim and
/// serve refuses an unknown one itself.
pub fn serve_argv(
    bin: &str,
    harness_args: &[String],
    permission_mode: Option<&str>,
) -> Result<(Vec<String>, String, Option<String>), String> {
    let mut argv = vec![bin.to_string(), "serve".to_string()];
    let mut mode = permission_mode.map(str::to_string);
    let mut effort = None;
    let mut yolo = false;
    let mut args = harness_args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--yolo" => yolo = true,
            "--approval-mode" | "--permission-mode" => mode = args.next().cloned(),
            "--reasoning-effort" | "--effort" => effort = args.next().cloned(),
            _ => argv.push(a.clone()),
        }
    }
    let wire = match (yolo, mode.as_deref()) {
        (true, _) => "allowAll",
        (false, None) => "promptUnmatched",
        (false, Some(m)) => wire_mode(m).ok_or_else(|| {
            format!("muse approval mode {m:?} is not untrusted, on-request or never")
        })?,
    };
    if yolo {
        argv.extend([
            "--disable-sandbox".to_string(),
            "--trust-workspace".to_string(),
        ]);
    }
    Ok((argv, wire.to_string(), effort))
}

/// The host socket of a muse thread row, when the row has one.
pub fn host_socket(entry: &RegistryEntry) -> Option<PathBuf> {
    if entry.harness_name() != "muse" || entry.substrate.as_deref() != Some("thread") {
        return None;
    }
    entry.messaging_socket_path.as_ref().map(PathBuf::from)
}

/// Write the spec, launch the detached host, and wait until it serves the
/// session. The pid is released to the daemon drain (orphan_reap). The spec
/// file stays on the success path: it is the revival's rebuild source. The
/// error carries the exit code the spawn door answered before this became a
/// helper (2 for a host that died, 12 for a machine failure), so the
/// extraction cannot reclassify an infra failure as a refusal.
fn launch_host(
    spec: &Spec,
    spec_path: &Path,
    socket: &Path,
    log: &Path,
    name: &str,
) -> Result<u32, (i32, String)> {
    let written = std::fs::create_dir_all(
        spec_path
            .parent()
            .ok_or_else(|| (12, "no muse dir".to_string()))?,
    )
    .and_then(|()| std::fs::write(spec_path, serde_json::to_vec(spec).unwrap_or_default()))
    .and_then(|()| std::fs::File::create(log));
    let log_file = match written {
        Ok(f) => f,
        Err(e) => {
            return Err((
                12,
                format!(
                    "{}: {e}",
                    spec_path.parent().unwrap_or(Path::new(".")).display()
                ),
            ))
        }
    };
    let mut cmd = Command::new(crate::daemon::resolve_worker_bin());
    cmd.arg("--muse-host")
        .args([spec_path, socket])
        .arg(name)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(
            log_file
                .try_clone()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        )
        .stderr(Stdio::from(log_file));
    crate::claims::scrub_identity_env(&mut cmd);
    // SAFETY: setsid is async-signal-safe; it runs between fork and exec. The
    // host leads its own group, which the stop command signals.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(spec_path);
            return Err((12, format!("cannot start the muse host: {e}")));
        }
    };
    let deadline = Instant::now() + READY_WAIT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(spec_path);
            let tail = std::fs::read_to_string(log).unwrap_or_default();
            return Err((
                2,
                format!("the muse host exited ({status}) before the session opened:\n{tail}"),
            ));
        }
        let status = host_call(socket, &json!({"op": "status"}));
        if status.is_ok_and(|s| {
            s.get("session_id").and_then(Value::as_str) == Some(spec.session_id.as_str())
        }) {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(spec_path);
            return Err((
                12,
                format!(
                    "the muse host did not serve its session within {}s; log: {}",
                    READY_WAIT.as_secs(),
                    log.display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let pid = child.id();
    // The host runs unwaited; the daemon drain reaps it by pid on exit.
    crate::orphan_reap::release_child(pid);
    Ok(pid)
}

/// `fno agents spawn -H muse --substrate thread`: start the host, wait until
/// it serves the session, register the row, then seed the first turn.
pub fn dispatch_thread(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    params: &Value,
) -> AskOutcome {
    if let Err(msg) = crate::claude_ask::validate_spawn_inputs(name, from_name) {
        return outcome(2, String::new(), format!("{msg}\n"));
    }
    match load_registry(&home.registry_json()) {
        Ok(reg) if reg.find(name).is_some() => {
            let msg = format!("agent {name} already exists; use 'fno agents rm {name}' first\n");
            return outcome(2, String::new(), msg);
        }
        Err(e) => return outcome(12, String::new(), format!("registry read failed: {e}\n")),
        Ok(_) => {}
    }
    let bin = match resolve_muse_bin() {
        Ok(b) => b,
        Err(e) => return outcome(2, String::new(), format!("{e}\n")),
    };
    let mode = params["permission_mode"].as_str();
    let (argv, approval_mode, effort) =
        match serve_argv(&bin.to_string_lossy(), &super::fenced_args(params), mode) {
            Ok(t) => t,
            Err(e) => return outcome(2, String::new(), format!("{e}\n")),
        };
    let sid = super::msp::mint_uuid_v7();
    let spec = Spec {
        session_id: sid.clone(),
        cwd: cwd.to_path_buf(),
        argv,
        approval_mode,
        effort,
    };
    let dir = home.root().join("muse");
    let socket = dir.join(format!("{}.sock", &sid[sid.len() - 12..]));
    let spec_path = dir.join(format!("{sid}.spec.json"));
    let log = dir.join(format!("{sid}.log"));
    let pid = match launch_host(&spec, &spec_path, &socket, &log, name) {
        Ok(pid) => pid,
        Err((code, why)) => return outcome(code, String::new(), format!("{why}\n")),
    };
    let entry = RegistryEntry {
        name: name.to_string(),
        short_id: sid.clone(),
        provider: Some("muse".into()),
        harness: Some("muse".into()),
        substrate: Some("thread".into()),
        session_id: Some(sid.clone()),
        harness_session_id: Some(sid.clone()),
        messaging_socket_path: Some(socket.to_string_lossy().into_owned()),
        requested_permission_mode: mode.map(str::to_string),
        node: params["node"].as_str().map(str::to_string),
        cwd: cwd.to_string_lossy().to_string(),
        origin: Some("spawn".into()),
        status: crate::AgentStatus::Live,
        pid: Some(pid),
        pid_start_time: crate::daemon::process_start_time(pid),
        created_at: crate::daemon::now_rfc3339_like(),
        log_path: Some(log.to_string_lossy().to_string()),
        account_record_id: Some("default".into()),
        ..RegistryEntry::new(Some(sid.clone()), crate::spawn_lineage::ambient_lineage())
    };
    let registered = update_registry(&home.registry_json(), |reg| {
        if reg.find(name).is_some() {
            return false;
        }
        reg.entries.push(entry.clone());
        true
    });
    if !matches!(registered, Ok(true)) {
        // SAFETY: the host leads its own group (setsid above).
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
        return match registered {
            Ok(_) => outcome(2, String::new(), format!("agent {name} already exists\n")),
            Err(e) => outcome(12, String::new(), format!("registry write failed: {e}\n")),
        };
    }
    let seed = (!message.is_empty())
        .then(|| host_call(&socket, &json!({"op": "prompt", "text": message})));
    let receipt = json!({
        "ok": true,
        "name": name,
        "session_id": sid,
        "substrate": "thread",
        "host_pid": pid,
        "socket": socket,
        "host_log": log,
        "seed": seed.map(|s| s.unwrap_or_else(|e| json!({"ok": false, "error": e}))),
    });
    outcome(0, format!("{receipt}\n"), String::new())
}

/// The probe answer for a row's host socket. Only a failed connect proves
/// the old host is gone: a connected socket that stays silent never does,
/// the same discipline the keeper revival's probe holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MuseProbe {
    /// A listener answered `status`.
    Live(Value),
    /// Connect failed: no listener holds the session.
    Gone,
    /// Connected but nothing answered within the window.
    Unclear(String),
}

/// Connect to the row's socket and ask `status` on the one connection, so a
/// one-shot listener is not mistaken for silence.
fn probe_host(socket: &Path) -> MuseProbe {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return MuseProbe::Gone;
    };
    let _ = stream.set_read_timeout(Some(CALL_WAIT));
    let req = json!({"op": "status"});
    if stream.write_all(format!("{req}\n").as_bytes()).is_err() {
        return MuseProbe::Unclear("muse host write failed".into());
    }
    let mut reply = String::new();
    match BufReader::new(&stream).read_line(&mut reply) {
        Ok(0) | Err(_) => MuseProbe::Unclear("muse host read failed".into()),
        Ok(_) => serde_json::from_str(reply.trim())
            .map(MuseProbe::Live)
            .unwrap_or_else(|e| MuseProbe::Unclear(format!("muse host reply: {e}"))),
    }
}

/// The seam the unit tests drive: every machine touch arrives as a closure,
/// the same shape `keeper_revival::revive_with` holds, so the probe
/// discipline and the row flip are provable without a real muse.
pub(crate) fn revive_with<P, L>(
    home: &AgentsHome,
    row: &RegistryEntry,
    probe: P,
    launch: L,
    message: Option<&str>,
) -> i32
where
    P: Fn(&Path) -> MuseProbe,
    L: Fn(&RegistryEntry, Option<&str>) -> Result<u32, String>,
{
    let row_name = &row.name;
    let sid = row
        .harness_session_id
        .as_deref()
        .or(row.session_id.as_deref())
        .unwrap_or_default();
    let Some(socket) = host_socket(row) else {
        eprintln!("fno agents resume: {row_name}: not a muse thread row; nothing was launched");
        return 13;
    };
    match probe(&socket) {
        MuseProbe::Live(reply) => {
            let answered = reply
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if answered == sid {
                println!(
                    "fno agents resume: {row_name} is already live on its host at {}; \
                     nothing was launched",
                    socket.display()
                );
                return 0;
            }
            eprintln!(
                "fno agents resume: {row_name}: the host at {} answers session \
                 {answered:?}, not the resumed {sid:?}; nothing was launched",
                socket.display()
            );
            13
        }
        MuseProbe::Unclear(why) => {
            eprintln!(
                "fno agents resume: {row_name}: the host at {} accepted the probe \
                 and stayed silent ({why}); silence never proves death, so nothing \
                 was launched",
                socket.display()
            );
            13
        }
        MuseProbe::Gone => {
            // A stale socket or child file is a kill -9 remainder.
            let _ = std::fs::remove_file(&socket);
            let _ = std::fs::remove_file(child_file(&socket));
            let pid = match launch(row, message) {
                Ok(pid) => pid,
                Err(why) => {
                    eprintln!("fno agents resume: {row_name}: {why}");
                    return 1;
                }
            };
            let flipped = update_registry(&home.registry_json(), |reg| {
                let Some(e) = reg.find_mut(row_name) else {
                    return false;
                };
                e.status = crate::AgentStatus::Live;
                e.exited_at = None;
                e.pid = Some(pid);
                e.pid_start_time = crate::daemon::process_start_time(pid);
                e.messaging_socket_path = Some(socket.to_string_lossy().into_owned());
                true
            });
            if !matches!(flipped, Ok(true)) {
                eprintln!("fno agents resume: {row_name}: the registry row flip failed");
                return 12;
            }
            println!(
                "fno agents resume: {row_name} revived on a fresh muse host \
                 (pid {pid}, socket {})",
                socket.display()
            );
            0
        }
    }
}

/// The production wrapper: real probe, real launcher. Loads the row fresh so
/// the spec rebuild reads the registry's own answer, and reuses the launch
/// half `dispatch_thread` runs.
pub fn revive(home: &AgentsHome, row_name: &str, cwd: &str, message: Option<&str>) -> i32 {
    let row = match load_registry(&home.registry_json())
        .ok()
        .and_then(|registry| registry.entries.into_iter().find(|e| e.name == row_name))
    {
        Some(row) => row,
        None => {
            eprintln!("fno agents resume: row {row_name} is no longer in the registry");
            return 13;
        }
    };
    revive_with(
        home,
        &row,
        probe_host,
        |row, message| {
            let sid = row
                .harness_session_id
                .as_deref()
                .or(row.session_id.as_deref())
                .unwrap_or_default();
            if sid.is_empty() {
                return Err(format!(
                    "fno agents resume: {}: the row has no recorded session id",
                    row.name
                ));
            }
            let dir = home.root().join("muse");
            let socket = dir.join(format!("{}.sock", &sid[sid.len() - 12..]));
            let spec_path = dir.join(format!("{sid}.spec.json"));
            let log = dir.join(format!("{sid}.log"));
            let persisted: Option<Spec> = std::fs::read_to_string(&spec_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            let (argv, approval_mode, effort) = match persisted {
                Some(spec) if !spec.argv.is_empty() => (spec.argv, spec.approval_mode, spec.effort),
                _ => {
                    let bin = resolve_muse_bin()?;
                    serve_argv(
                        &bin.to_string_lossy(),
                        &[],
                        row.requested_permission_mode.as_deref(),
                    )?
                }
            };
            let spec_cwd = if row.cwd.is_empty() {
                PathBuf::from(cwd)
            } else {
                PathBuf::from(&row.cwd)
            };
            let spec = Spec {
                session_id: sid.to_string(),
                cwd: spec_cwd,
                argv,
                approval_mode,
                effort,
            };
            let pid = match launch_host(&spec, &spec_path, &socket, &log, &row.name) {
                Ok(pid) => pid,
                // The revive door answers one failure class; the spawn door's
                // code split stays in dispatch_thread.
                Err((_code, why)) => return Err(why),
            };
            if let Some(text) = message.filter(|m| !m.is_empty()) {
                let _ = host_call(&socket, &json!({"op": "prompt", "text": text}));
            }
            Ok(pid)
        },
        message,
    )
}

/// `agent.deliver` for a muse thread row: submit the wrapped mail as a turn
/// and answer with the host's receipt. `delivered` is true only once Muse
/// recorded the user message carrying the envelope id.
pub fn deliver(entry: &RegistryEntry, body: &str) -> Value {
    let Some(socket) = host_socket(entry) else {
        return json!({"delivered": false, "reason": "no-deliver-lane"});
    };
    let envelope = crate::mail_header::delivered_msg_id(body);
    let req = json!({"op": "prompt", "text": body, "envelope_id": envelope});
    match host_call(&socket, &req) {
        Ok(mut r) if r["state"] == "parked" => {
            // Muse took the submit into its own queue and runs it when the
            // turn ends, so the send writes no durable copy: a second copy
            // would reach the worker twice. `queued` is the footnote host's
            // word for the same answer.
            r["delivered"] = json!(true);
            r["queued"] = json!(true);
            r
        }
        Ok(mut r) if r["state"].is_string() => {
            if r["delivered"] != true && r["reason"].is_null() {
                r["reason"] = json!(r["state"]);
            }
            r
        }
        Ok(r) => json!({"delivered": false, "reason": r["error"]}),
        Err(e) => json!({"delivered": false, "reason": format!("muse-host-unreachable: {e}")}),
    }
}

/// The deliver RPC's answer for a row with no muse host, so the seam can
/// keep one call shape.
pub fn deliver_rpc(home: &AgentsHome, req: &Request) -> Option<Response> {
    let name = req.params["name"].as_str().unwrap_or("");
    let reg = load_registry(&home.registry_json()).ok()?;
    let entry = reg.find(name)?;
    host_socket(entry)?;
    let body = req.params["body"].as_str().unwrap_or("");
    Some(Response::ok(req.id, deliver(entry, body)))
}

/// `fno agents ask` on a live muse thread row. None when the row has no
/// reachable host, so the caller falls back to the once lane's resume.
pub fn maybe_ask_host(entry: &RegistryEntry, params: &Value) -> Option<i32> {
    let socket = host_socket(entry)?;
    let answer = params["answer"].as_str();
    let req = match answer {
        Some(option @ ("allow_once" | "deny")) => json!({"op": "answer", "option": option}),
        Some(other) => {
            eprintln!("fno-agents: --answer takes allow_once or deny, not {other:?}");
            return Some(2);
        }
        None => json!({"op": "prompt", "text": params["message"].as_str().unwrap_or("")}),
    };
    match host_call(&socket, &req) {
        Ok(reply) => {
            println!("{reply}");
            let ok = reply["ok"] == true || reply["state"].as_str().is_some_and(|s| s != "failed");
            Some(if ok { 0 } else { 2 })
        }
        Err(e) if answer.is_some() => {
            eprintln!(
                "fno-agents: {}: the muse host is not reachable: {e}",
                entry.name
            );
            Some(2)
        }
        Err(_) => None,
    }
}

/// Stop a muse row: SIGINT the host's group (it interrupts and leaves the
/// session), then reap a `muse serve` child the host left behind.
pub(crate) async fn stop_row(pid: u32, entry: &RegistryEntry) -> bool {
    let child = host_socket(entry).map(|s| child_file(&s));
    crate::footnote_harness::thread::stop_host(pid, entry.pid_start_time, child).await
}
