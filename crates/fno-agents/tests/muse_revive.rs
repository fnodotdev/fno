//! The muse host revival, proven through the built binary.
//!
//! The keeper file proves the same shape for keeper lanes. These tests prove
//! the muse lane: the resume door routes an exited muse thread row into a
//! fresh `fno-agents-worker --muse-host` on the SAME session id, the probe
//! refuses while a live host holds the socket (-32021 guard), and the
//! persisted spec survives so the revival relaunches the same conversation.

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "33333333-4444-5555-6666-777788889999";

struct TempDirGuard(tempfile::TempDir);

struct Fixture {
    root: TempDirGuard,
    home: PathBuf,
    bins: PathBuf,
    name: String,
    muse_log: PathBuf,
}

impl Fixture {
    /// One exited muse thread row, a fake `muse` that serves the MSP
    /// handshake over stdio, and the persisted spec a revival reads.
    fn new() -> Self {
        let root = TempDirGuard(tempfile::tempdir().unwrap());
        let home = root.0.path().join("agents");
        let bins = root.0.path().join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bins).unwrap();
        let name = format!("t-muse-revive-{}", std::process::id());
        let cwd = root.0.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let muse_dir = home.join("muse");
        fs::create_dir_all(&muse_dir).unwrap();
        let log = muse_dir.join(format!("{SID}.log"));
        fs::File::create(&log).unwrap();
        let row = json!({
            "name": name,
            "harness": "muse",
            "substrate": "thread",
            "status": "exited",
            "exited_at": "2026-11-01T00:00:00Z",
            "cwd": cwd,
            "log_path": log,
            "created_at": "2026-10-31T00:00:00Z",
            "harness_session_id": SID,
            "short_id": SID,
            "session_id": SID,
            "messaging_socket_path": socket_path(&home),
        });
        write_registry(&home, &[row]);
        write_spec(&home, &bins.join("muse"), &cwd);
        write_executable(
            &bins.join("muse"),
            &format!(
                "#!/usr/bin/env python3\nimport json, os, sys\nlog = os.environ['FAKE_MUSE_LOG']\nwith open(log, 'a') as f:\n    f.write(' '.join(sys.argv[1:]) + '\\n')\nif sys.argv[1:2] == ['serve']:\n    for line in sys.stdin:\n        req = json.loads(line)\n        if 'id' not in req:\n            continue\n        rid, method = req.get('id'), req.get('method')\n        result = {{}}\n        if method == 'initialize':\n            result = {{'serverInfo': {{'name': 'muse', 'version': '1.4.12'}}}}\n        elif method == 'session/start':\n            result = {{'session': {{'sessionId': os.environ['MUSE_FAKE_SID']}}}}\n        print(json.dumps({{'jsonrpc': '2.0', 'id': rid, 'result': result}}), flush=True)\n    import time\n    time.sleep(300)\n"
            ),
        );
        let muse_log = root.0.path().join("muse-fake.log");
        Self {
            root,
            home,
            bins,
            name,
            muse_log,
        }
    }

    fn resume(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["resume", &self.name])
            .env_clear()
            .envs(fno_agents::test_run::self_owner_env())
            .env("FNO_AGENTS_HOME", &self.home)
            .env("HOME", self.root.0.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bins.display()))
            .env(
                "FNO_AGENTS_WORKER_BIN",
                env!("CARGO_BIN_EXE_fno-agents-worker"),
            )
            .env("FNO_SPAWN_GATE", "0")
            .env("FNO_MUSE_BIN", self.bins.join("muse"))
            .env("MUSE_FAKE_SID", SID)
            .env("FAKE_MUSE_LOG", &self.muse_log)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fno-agents starts")
            .wait_with_output()
            .expect("fno-agents resume finishes")
    }

    fn socket(&self) -> PathBuf {
        PathBuf::from(socket_path(&self.home))
    }

    fn spec(&self) -> PathBuf {
        self.home.join("muse").join(format!("{SID}.spec.json"))
    }

    fn registry_row(&self) -> Value {
        let raw = registry_text(&self.home.join("registry.json")).unwrap();
        let registry: Value = serde_json::from_str(&raw).unwrap();
        registry
            .get("agents")
            .and_then(Value::as_array)
            .expect("registry rows")
            .iter()
            .find(|row| row.get("name").and_then(Value::as_str) == Some(self.name.as_str()))
            .cloned()
            .expect("the row survives")
    }

    /// Kill whatever the revival left live: the flipped row names the host pid.
    fn kill_revived(&self) {
        if let Some(pid) = self.registry_row().get("pid").and_then(Value::as_u64) {
            // SAFETY: group SIGKILL to the host a test started. A pid kill
            // leaves descendants running (the fn-47ea driver pattern): the
            // host leads its own group via setsid, so the fake muse child
            // dies with it.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        // The revived host outlives the test unless the row's pid dies first.
        let _ = &self.0;
    }
}

fn socket_path(home: &Path) -> String {
    let dir = home.join("muse");
    dir.join(format!("{}.sock", &SID[SID.len() - 12..]))
        .to_string_lossy()
        .into_owned()
}

fn registry_text(path: &std::path::Path) -> std::io::Result<String> {
    fno_agents::registry_store::read(path)
        .map(|doc| serde_json::to_string_pretty(&doc).unwrap())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

fn write_registry(home: &Path, rows: &[Value]) {
    fs::write(
        home.join("registry.json"),
        serde_json::to_vec(&json!({
            "schema_version": fno_agents::state::REGISTRY_SCHEMA_VERSION,
            "agents": rows,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_spec(home: &Path, muse_bin: &Path, cwd: &Path) {
    let spec = json!({
        "session_id": SID,
        "cwd": cwd,
        "argv": [muse_bin, "serve"],
        "approval_mode": "allowAll",
        "effort": null,
    });
    fs::write(
        home.join("muse").join(format!("{SID}.spec.json")),
        spec.to_string(),
    )
    .unwrap();
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

/// The revival path, end to end: exit 0, the row live on a fresh host pid,
/// the socket answering status for the same session id, and the spec file
/// still there so the next revival relaunches the same conversation.
#[test]
fn resume_revives_an_exited_muse_thread_row_on_a_fresh_host() {
    let fixture = Fixture::new();
    let output = fixture.resume();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let row = fixture.registry_row();
    assert_eq!(
        row.get("status").and_then(Value::as_str),
        Some("live"),
        "row: {row}"
    );
    assert!(
        row.get("pid").and_then(Value::as_u64).is_some(),
        "row: {row}"
    );
    assert!(row.get("exited_at").is_none(), "row: {row}");
    let fake_log = fs::read_to_string(&fixture.muse_log).unwrap_or_default();
    assert!(
        fake_log.contains("serve"),
        "the fake muse never served: {fake_log}"
    );
    let status = fno_agents::muse::host::host_call(&fixture.socket(), &json!({"op": "status"}))
        .expect("the revived host answers status");
    assert_eq!(status.get("session_id").and_then(Value::as_str), Some(SID));
    assert!(
        fixture.spec().is_file(),
        "the persisted spec must survive the host start"
    );
    fixture.kill_revived();
}

/// A live host on the row's socket holds the session's writer lease: the
/// revive answers already-live, launches nothing, and leaves the row alone.
#[test]
fn a_live_host_on_the_socket_launches_nothing() {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let served = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let reply = json!({"ok": true, "session_id": SID, "busy": false});
        (&stream)
            .write_all(format!("{reply}\n").as_bytes())
            .unwrap();
    });
    let output = fixture.resume();
    served.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already live"), "stdout: {stdout}");
    let row = fixture.registry_row();
    assert_eq!(
        row.get("status").and_then(Value::as_str),
        Some("exited"),
        "the row must stay exited: {row}"
    );
    assert!(
        !fixture.muse_log.exists() || fs::read_to_string(&fixture.muse_log).unwrap().is_empty(),
        "no host may launch while a live one holds the socket"
    );
}
