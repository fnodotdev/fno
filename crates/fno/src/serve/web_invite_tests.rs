//! The invite tables. Each sends bytes over TCP to [`router`], reads only
//! temp files and starts no process.

use super::tests::{answer_state, raw_request, seed_model, serve_state};
use super::*;
use crate::web_invites::Invite;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ANN: &str = "ann@example.com";
const BEN: &str = "ben@example.com";
const LOCAL: &str = "Host: 127.0.0.1:8722\r\n";
const PROXY: &str = "Host: machine.tail1.ts.net\r\n";
const ACT: &str = r#"{"act":"blueprint","id":"x-none"}"#;
/// What a loaded test machine may add to the feed's bound.
const MARGIN: Duration = Duration::from_secs(1);

thread_local! {
    static FIXTURES: tempfile::TempDir = tempfile::tempdir().unwrap();
}

/// A source over a temp global file that lives as long as the test's thread.
/// `None` is a global file that does not exist.
pub(super) fn invite_source(invites: Option<Vec<Invite>>) -> Arc<InviteSource> {
    FIXTURES.with(|dir| {
        let mut path = dir.path().join("absent.toml");
        if let Some(list) = invites {
            let nth = std::fs::read_dir(dir.path()).unwrap().count();
            path = dir.path().join(format!("{nth}.toml"));
            let text: String = list
                .iter()
                .map(|i| row(&i.email, &i.scope, &format!("{:?}", i.role)))
                .collect();
            std::fs::write(&path, text).unwrap();
        }
        Arc::new(InviteSource::at_bind(TrustedFile::Global(path), &[]).0)
    })
}

fn row(email: &str, scope: &str, role: &str) -> String {
    format!("[[web.invites]]\nemail = \"{email}\"\nscope = \"{scope}\"\nrole = \"{role}\"\n")
}

fn via(login: &str) -> String {
    format!("{PROXY}Tailscale-User-Login: {login}\r\n")
}

/// A proxy that rewrote Host to loopback.
fn via_loopback(login: &str) -> String {
    format!("{LOCAL}Tailscale-User-Login: {login}\r\n")
}

/// Replace by rename, as a config write does.
fn put(path: &Path, text: &str) {
    let next = path.with_extension("next");
    std::fs::write(&next, text).unwrap();
    std::fs::rename(&next, path).unwrap();
}

/// One bridge on a socket, with a board that holds `x-1` and `x-2`.
struct Bridge {
    addr: std::net::SocketAddr,
    tx: broadcast::Sender<String>,
    server: tokio::task::JoinHandle<()>,
    notices: Vec<String>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Bridge {
    async fn start(file: TrustedFile, project: &[PathBuf]) -> Self {
        Self::start_with(file, project, "right").await
    }

    async fn start_with(file: TrustedFile, project: &[PathBuf], token: &str) -> Self {
        let mut st = answer_state(true, None);
        let (source, notices) = InviteSource::at_bind(file, project);
        st.invites = Arc::new(source);
        st.token = token.into();
        let inputs = seed_model(
            &mut st,
            vec![
                serde_json::json!({"id": "x-1", "status": "ready"}),
                serde_json::json!({"id": "x-2", "status": "ready"}),
            ],
        )
        .await;
        // The answer route finds its question here, so it starts no fold read.
        let question = crate::needs_overlay::QuestionItem {
            id: "q-1".into(),
            node: Some("x-1".into()),
            state: "open".into(),
            ..Default::default()
        };
        let fold = crate::needs_overlay::QuestionsFold {
            items: vec![question],
            ..Default::default()
        };
        st.hq.board().await.record(
            Err("the test seeds no fleet".into()),
            Ok(fold),
            &inputs,
            1,
            &[],
        );
        Self::serving(st, notices).await
    }

    async fn serving(st: AppState, notices: Vec<String>) -> Self {
        let tx = st.tx.clone();
        let (addr, server) = serve_state(st).await;
        Self {
            addr,
            tx,
            server,
            notices,
        }
    }

    async fn reply(&self, method: &str, path: &str, who: &str, body: &str) -> String {
        // One write: a refusal can close before a second one arrives.
        let sent = format!(
            "{method} {path} HTTP/1.1\r\n{who}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        raw_request(self.addr, &sent, b"").await
    }

    async fn status(&self, method: &str, path: &str, who: &str, body: &str) -> String {
        let reply = self.reply(method, path, who, body).await;
        reply.get(9..12).unwrap_or_default().to_string()
    }

    async fn hq(&self, who: &str) -> String {
        self.status("GET", "/hq?t=right", who, "").await
    }

    async fn act(&self, who: &str) -> String {
        self.status("POST", "/backlog/act?t=right", who, ACT).await
    }

    /// An upgraded feed.
    async fn feed(&self, path: &str, who: &str) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let head = format!(
            "GET {path} HTTP/1.1\r\n{who}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        let reply = read_head(&mut stream).await;
        assert!(reply.starts_with("HTTP/1.1 101"), "{reply}");
        stream
    }

    /// A feed that was admitted: its first frame is the bridge's status.
    async fn open_feed(&self, who: &str) -> tokio::net::TcpStream {
        let mut stream = self.feed("/ws?t=right", who).await;
        assert_eq!(
            next_frame(&mut stream).await.0,
            1,
            "a text frame comes first"
        );
        stream
    }
}

async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(head).unwrap()
}

/// The opcode and payload of the next unmasked frame.
async fn next_frame(stream: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
    let opcode = stream.read_u8().await.unwrap() & 0x0f;
    let len = match stream.read_u8().await.unwrap() {
        126 => u64::from(stream.read_u16().await.unwrap()),
        127 => stream.read_u64().await.unwrap(),
        short => u64::from(short),
    };
    let mut payload = vec![0; usize::try_from(len).unwrap()];
    stream.read_exact(&mut payload).await.unwrap();
    (opcode, payload)
}

/// The code of the close frame, past any frames sent before it.
async fn close_code_of(stream: &mut tokio::net::TcpStream) -> u16 {
    loop {
        let (opcode, payload) = next_frame(stream).await;
        if opcode == 8 {
            let code = payload.first_chunk::<2>().expect("a close code");
            return u16::from_be_bytes(*code);
        }
    }
}

#[tokio::test]
async fn invite_source_request_table() {
    trusted_rows_grant_and_a_folder_does_not().await;
    an_unusable_trusted_file_admits_nobody().await;
    a_project_list_holds_or_changes_nothing().await;
    the_startup_folder_decides_the_hold().await;
    the_layer_gates_every_caller_shape().await;
}

async fn trusted_rows_grant_and_a_folder_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    put(&path, &row(ANN, "*", "owner"));
    for file in [
        TrustedFile::Global(path.clone()),
        TrustedFile::Pin(path.clone()),
    ] {
        let bridge = Bridge::start(file.clone(), &[]).await;
        assert_eq!(bridge.hq(&via(ANN)).await, "200", "{file:?}");
        assert!(bridge.notices.is_empty(), "{:?}", bridge.notices);
    }

    let global = dir.path().join("absent.toml");
    let checkout = dir.path().join("checkout.toml");
    let canonical = dir.path().join("canonical.toml");
    put(&canonical, &row(ANN, "*", "owner"));
    put(&checkout, &row(ANN, "*", "owner"));
    let missing = dir.path().join("missing.toml");
    for project in [vec![checkout], vec![missing, canonical]] {
        let bridge = Bridge::start(TrustedFile::Global(global.clone()), &project).await;
        assert_eq!(bridge.hq(&via(ANN)).await, "403", "{project:?}");
        let answer = bridge
            .status(
                "POST",
                "/hq/answer/q-1?t=right",
                &via(ANN),
                r#"{"option":1}"#,
            )
            .await;
        assert_eq!(answer, "403", "{project:?}");
        assert_eq!(bridge.hq(LOCAL).await, "200", "{project:?}");
    }
}

async fn an_unusable_trusted_file_admits_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let at = |name: &str, text: &str| {
        let path = dir.path().join(name);
        put(&path, text);
        TrustedFile::Global(path)
    };
    for file in [
        TrustedFile::Untrusted("FNO_CONFIG must be an absolute path to a .toml file"),
        TrustedFile::Pin(dir.path().join("no-such-pin.toml")),
        at("bad.toml", "[[web.invites"),
        at("table.toml", "web = 3\n"),
        at("string.toml", "[web]\ninvites = \"all\"\n"),
        // A directory is a file that cannot be read, also as root.
        TrustedFile::Global(dir.path().to_path_buf()),
    ] {
        let bridge = Bridge::start(file.clone(), &[]).await;
        assert_eq!(bridge.hq(&via(ANN)).await, "403", "{file:?}");
        assert_eq!(bridge.hq(PROXY).await, "403", "{file:?}");
        assert_eq!(bridge.hq(LOCAL).await, "200", "{file:?}");
        assert_eq!(bridge.notices.len(), 1, "{file:?}");
    }
}

async fn a_project_list_holds_or_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let project = dir.path().join("checkout.toml");
    let file = TrustedFile::Global(global.clone());
    let both = format!("{}{}", row(ANN, "*", "viewer"), row(BEN, "x-1", "editor"));
    put(&global, &both);
    let reversed = format!("{}{}", row(BEN, "x-1", "editor"), row(ANN, "*", "viewer"));
    put(&project, &reversed);
    let bridge = Bridge::start(file.clone(), &[project.clone()]).await;
    assert_eq!(
        bridge.hq(&via(ANN)).await,
        "200",
        "the same rows in any order"
    );
    let notices = bridge.notices.join("\n");
    assert_eq!(bridge.notices.len(), 1, "{notices}");
    assert!(!notices.contains("example.com"), "{notices}");

    // The narrower project list used to win over the whole-board one.
    put(&global, &row(ANN, "*", "owner"));
    put(&project, &row(ANN, "x-1", "owner"));
    let held = Bridge::start(file.clone(), &[project.clone()]).await;
    for path in [
        "/backlog/model.json?t=right",
        "/backlog/node.json?t=right&id=x-2",
    ] {
        assert_eq!(
            held.status("GET", path, &via(ANN), "").await,
            "403",
            "{path}"
        );
    }
    assert_eq!(held.act(&via_loopback(ANN)).await, "403");
    assert_eq!(held.act(LOCAL).await, "404", "the local user still acts");
    let notices = held.notices.join("\n");
    assert_eq!(held.notices.len(), 1, "{notices}");
    assert!(notices.contains("fno config set web.invites"), "{notices}");
    assert!(!notices.contains("example.com"), "{notices}");
    put(&project, "");
    assert_eq!(
        held.hq(&via(ANN)).await,
        "403",
        "a hold lasts until a restart"
    );

    put(&project, &row(BEN, "*", "owner"));
    let pinned = Bridge::start(TrustedFile::Pin(global.clone()), &[project]).await;
    assert_eq!(pinned.hq(&via(ANN)).await, "200", "a pin checks no project");
    assert_eq!(pinned.hq(&via(BEN)).await, "403");
    let same = Bridge::start(file, &[global]).await;
    assert_eq!(
        same.hq(&via(ANN)).await,
        "200",
        "the trusted file is no project"
    );
    assert!(same.notices.is_empty(), "{:?}", same.notices);

    let path = dir.path().join("scopes.toml");
    put(
        &path,
        &format!(
            "[[web.invites]]\nemail = \"{ANN}\"\nscope = [\"x-1\"]\nrole = \"owner\"\n[[web.invites]]\nemail = \"{BEN}\"\nrole = \"viewer\"\n"
        ),
    );
    let bridge = Bridge::start(TrustedFile::Global(path), &[]).await;
    let board = "/backlog/model.json?t=right";
    assert_eq!(bridge.status("GET", board, &via(ANN), "").await, "403");
    let reply = bridge.reply("GET", board, &via(BEN), "").await;
    assert!(
        reply.contains("\"x-2\""),
        "no scope is the whole board: {reply}"
    );
}

/// The test's cwd is not the startup folder, as after a bridge changes folder.
/// A source that read the cwd would find no project rows and admit the login.
async fn the_startup_folder_decides_the_hold() {
    let dir = tempfile::tempdir().unwrap();
    let startup = dir.path().join("checkout");
    std::fs::create_dir_all(startup.join(".git")).unwrap();
    std::fs::create_dir_all(startup.join(".fno")).unwrap();
    put(&startup.join(".fno/config.toml"), &row(ANN, "x-1", "owner"));
    let global = dir.path().join("config.toml");
    put(&global, &row(BEN, "*", "owner"));
    let source = {
        // The project root follows FNO_REPO_ROOT, which other tests set.
        let _env = crate::digest_overlay::ENVIRONMENT_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_REPO_ROOT");
        std::env::remove_var("FNO_REPO_ROOT");
        let source = InviteSource::for_bridge(TrustedFile::Global(global), &startup);
        if let Some(root) = saved {
            std::env::set_var("FNO_REPO_ROOT", root);
        }
        source
    };
    let mut st = answer_state(true, None);
    st.invites = Arc::new(source);
    let bridge = Bridge::serving(st, Vec::new()).await;
    assert_eq!(
        bridge.hq(&via(BEN)).await,
        "403",
        "the startup folder holds"
    );
    assert_eq!(bridge.hq(LOCAL).await, "200", "the local user still reads");
}

async fn the_layer_gates_every_caller_shape() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let no_list = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    assert_eq!(
        no_list.hq(&via(BEN)).await,
        "200",
        "no list: the token rules"
    );
    put(&global, &row(ANN, "*", "viewer"));
    let bridge = Bridge::start(TrustedFile::Global(global), &[]).await;
    for (who, status) in [
        (via(ANN), "200"),
        (via(BEN), "403"),
        (PROXY.to_string(), "403"),
        (LOCAL.to_string(), "200"),
        (format!("{LOCAL}X-Forwarded-For: 100.64.0.9\r\n"), "403"),
        (via_loopback(""), "403"),
        (via_loopback("zo\u{e9}@example.com"), "403"),
    ] {
        assert_eq!(bridge.hq(&who).await, status, "{who}");
    }
}

#[tokio::test]
async fn ui_click_consent_table() {
    use crate::web_invites::Role;
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("events.jsonl");
    let query = crate::event_store::EventQuery {
        types: vec!["ui_click".into()],
        ..Default::default()
    };
    let click = "/ui/click?t=right";
    let own = format!("{LOCAL}Origin: http://127.0.0.1:8722\r\n");
    let foreign = format!("{LOCAL}Origin: https://evil.example\r\n");
    // The first case is the only row. Every later one leaves it alone.
    let mut cases = vec![
        (true, None, click, own, "204"),
        (true, None, "/ui/click", LOCAL.to_string(), "401"),
        (true, None, "/ui/click?t=wrong", LOCAL.to_string(), "401"),
        (true, None, click, foreign, "403"),
        (true, None, click, via(ANN), "204"),
        (true, None, click, PROXY.to_string(), "204"),
        (true, None, click, via_loopback(""), "204"),
        (
            true,
            None,
            "/fno/ui/click?t=right",
            via_loopback(ANN),
            "204",
        ),
        (false, None, click, LOCAL.to_string(), "204"),
    ];
    for header in ["X-Forwarded-For", "X-Forwarded-Host", "Forwarded"] {
        let who = format!("{LOCAL}{header}: 100.64.0.9\r\n");
        cases.push((true, None, click, who, "204"));
    }
    for role in [Role::Viewer, Role::Commenter, Role::Editor, Role::Owner] {
        let invite = Invite {
            email: ANN.into(),
            scope: "*".into(),
            role,
        };
        cases.push((true, Some(vec![invite]), click, via(ANN), "204"));
    }
    for (writable, invites, path, who, status) in cases {
        let mut st = answer_state(writable, invites);
        st.click_journal = Some(journal.clone());
        let bridge = Bridge::serving(st, Vec::new()).await;
        let body = r#"{"screen":"web_hq","control":"tab_row"}"#;
        let got = bridge.status("POST", path, &who, body).await;
        assert_eq!(got, status, "{path} {who} writable={writable}");
        let rows = crate::event_store::query_events(&journal, &query).unwrap();
        assert_eq!(rows.len(), 1, "{path} {who} writable={writable}");
    }
}

#[tokio::test]
async fn invite_revoke_request_table() {
    a_replaced_file_rules_the_next_request().await;
    no_removal_reopens_the_bridge().await;
    a_board_act_passes_nobody_new().await;
    a_waiting_write_reads_the_revoke().await;
    a_new_process_refuses_the_old_token().await;
    open_feeds_close_inside_the_bound().await;
}

async fn a_replaced_file_rules_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let ben = row(BEN, "*", "viewer");
    put(&global, &ben);
    let bridge = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    for prefix in ["", "/fno"] {
        let hq = format!("{prefix}/hq?t=right");
        let board = format!("{prefix}/backlog/model.json?t=right");
        let node = format!("{prefix}/backlog/node.json?t=right&id=x-2");
        let who = via(ANN);
        assert_eq!(bridge.status("GET", &hq, &who, "").await, "403", "{prefix}");
        put(&global, &format!("{ben}{}", row(ANN, "*", "owner")));
        let reply = bridge.reply("GET", &board, &who, "").await;
        assert!(reply.contains("\"x-2\""), "added: {reply}");
        put(&global, &format!("{ben}{}", row(ANN, "x-1", "viewer")));
        let reply = bridge.reply("GET", &board, &who, "").await;
        assert!(reply.contains("\"x-1\""), "downgraded: {reply}");
        assert!(!reply.contains("\"x-2\""), "downgraded: {reply}");
        assert_eq!(
            bridge.status("GET", &node, &who, "").await,
            "403",
            "{prefix}"
        );
        put(&global, &ben);
        for path in [&hq, &board, &node] {
            assert_eq!(bridge.status("GET", path, &who, "").await, "403", "{path}");
        }
        assert_eq!(bridge.status("GET", &hq, &via(BEN), "").await, "200");
    }
}

async fn no_removal_reopens_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let listed = row(ANN, "*", "owner");
    put(&global, &listed);
    let bridge = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    for left in [Some("[web]\ninvites = []\n"), Some("[web]\n"), None] {
        put(&global, &listed);
        assert_eq!(bridge.hq(&via(ANN)).await, "200");
        match left {
            Some(text) => put(&global, text),
            None => std::fs::remove_file(&global).unwrap(),
        }
        assert_eq!(bridge.hq(&via(ANN)).await, "403", "{left:?}");
        assert_eq!(bridge.hq(PROXY).await, "403", "{left:?}");
        assert_eq!(bridge.hq(LOCAL).await, "200", "{left:?}");
    }

    put(&global, &listed);
    assert_eq!(bridge.hq(&via(ANN)).await, "200");
    let modified = std::fs::metadata(&global).unwrap().modified().unwrap();
    // The same length and the same mtime.
    std::fs::write(&global, row(BEN, "*", "owner")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&global)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(bridge.hq(&via(ANN)).await, "403", "no cache by mtime");
    assert_eq!(bridge.hq(&via(BEN)).await, "200");
}

async fn a_board_act_passes_nobody_new() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let no_list = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    // No node is named x-none, so a caller past the guards gets 404 and no
    // process starts.
    assert_eq!(no_list.act(&via_loopback(ANN)).await, "404");
    put(
        &global,
        &format!("{}{}", row(ANN, "*", "owner"), row(BEN, "x-1", "owner")),
    );
    let bridge = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    for (who, status) in [
        (LOCAL.to_string(), "404"),
        (via_loopback(ANN), "404"),
        (via(ANN), "403"),
        (via_loopback(BEN), "403"),
    ] {
        assert_eq!(bridge.act(&who).await, status, "{who}");
    }
    put(&global, &row(BEN, "x-1", "owner"));
    assert_eq!(bridge.act(&via_loopback(ANN)).await, "403", "revoked");
}

async fn a_waiting_write_reads_the_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let look_dir = crate::web_mine::store_root().join(crate::web_mine::login_key(ANN));
    let look_file = look_dir.join("invite-race.json");
    for (path, body) in [
        ("/hq/answer/q-1?t=right", r#"{"option":1}"#),
        (
            "/hq/mine/look?t=right",
            r#"{"project":"invite-race","look":{}}"#,
        ),
    ] {
        put(&global, &row(ANN, "*", "owner"));
        let bridge = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
        let mut stream = tokio::net::TcpStream::connect(bridge.addr).await.unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\n{}Content-Type: application/json\r\nExpect: 100-continue\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            via(ANN),
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        // The server asks for the body only after the gate admitted it.
        let interim = read_head(&mut stream).await;
        assert!(interim.starts_with("HTTP/1.1 100"), "{path}: {interim}");
        put(&global, &row(BEN, "*", "owner"));
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{path}: {reply}");
        assert!(reply.contains("not on the invite list"), "{path}: {reply}");
    }
    assert!(!look_file.exists(), "a refused look writes no row");
}

async fn a_new_process_refuses_the_old_token() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    put(&global, &row(ANN, "*", "owner"));
    let bridge = Bridge::start_with(TrustedFile::Global(global), &[], "fresh").await;
    let look = r#"{"project":"p","look":{}}"#;
    for (method, path, body) in [
        ("GET", "/backlog?t=right", ""),
        ("GET", "/backlog/model.json?t=right", ""),
        ("GET", "/backlog/node.json?t=right&id=x-1", ""),
        ("GET", "/rundown?t=right", ""),
        ("GET", "/team?t=right", ""),
        ("GET", "/hq?t=right", ""),
        ("GET", "/hq/model.json?t=right", ""),
        ("GET", "/hq/decisions.json?t=right", ""),
        ("GET", "/hq/mine.json?t=right", ""),
        ("GET", "/trace?t=right", ""),
        ("GET", "/trace/model.json?t=right&node=x-1", ""),
        ("POST", "/backlog/act?t=right", ACT),
        ("POST", "/hq/answer/q-1?t=right", r#"{"option":1}"#),
        ("POST", "/hq/mine/look?t=right", look),
    ] {
        let status = bridge.status(method, path, &via_loopback(ANN), body).await;
        assert_eq!(status, "401", "{path}");
    }
    let mut feed = bridge.feed("/ws?t=right", &via(ANN)).await;
    assert_eq!(close_code_of(&mut feed).await, 1008);
}

async fn open_feeds_close_inside_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("config.toml");
    let listed = row(ANN, "*", "viewer");
    let bound = FEED_RECHECK + MARGIN;

    put(&global, &listed);
    let idle = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    let mut feeds = Vec::new();
    for _ in 0..10 {
        feeds.push(idle.open_feed(&via(ANN)).await);
    }
    put(&global, &row(BEN, "*", "viewer"));
    tokio::time::timeout(bound, async {
        for feed in &mut feeds {
            assert_eq!(close_code_of(feed).await, 1008);
        }
    })
    .await
    .expect("every idle feed closes inside the bound");

    put(&global, &listed);
    let busy = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    let mut feed = busy.open_feed(&via(ANN)).await;
    let tx = busy.tx.clone();
    let frames = tokio::spawn(async move {
        loop {
            // No subscriber is left once the feed closes.
            tx.send(r#"{"frame":1}"#.to_string()).ok();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    put(&global, &row(ANN, "x-1", "viewer"));
    let code = tokio::time::timeout(bound, close_code_of(&mut feed))
        .await
        .expect("a streaming feed closes inside the bound");
    assert_eq!(code, 1008);
    frames.abort();

    std::fs::remove_file(&global).unwrap();
    let no_list = Bridge::start(TrustedFile::Global(global.clone()), &[]).await;
    let mut feed = no_list.open_feed(PROXY).await;
    put(&global, &row(BEN, "*", "viewer"));
    let code = tokio::time::timeout(bound, close_code_of(&mut feed))
        .await
        .expect("a token-only feed closes when a list appears");
    assert_eq!(code, 1008);
}
