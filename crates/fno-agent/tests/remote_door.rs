//! The remote door end to end on loopback: a device pairs from an offer,
//! opens Noise IK, and reads the fleet through the door from a fake
//! supervisor socket. This file is also the reference client the phone
//! app (docs/architecture/remote-door.md) mirrors.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fno_agent::paths::AgentsHome;
use fno_agent::protocol::{self, ErrorCode, Request, Response};
use fno_agent::remote::channel::{self, Channel, ChannelError, Keypair};
use fno_agent::remote::{door, pairing};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener};

const TAILNET: &str = "mac.tail1234.ts.net";

fn home() -> (tempfile::TempDir, AgentsHome) {
    let dir = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(dir.path().join("a"));
    home.ensure_root().unwrap();
    (dir, home)
}

/// What the fake daemon answers for a method.
fn fleet_reply(id: u64, method: &str) -> Response {
    Response::ok(
        id,
        json!({"method": method, "agents": [{"name": "w1", "status": "running"}]}),
    )
}

/// A supervisor socket that answers one request per connection, like the
/// daemon, and counts connections.
fn fake_daemon(home: &AgentsHome) -> Arc<AtomicUsize> {
    let conns = Arc::new(AtomicUsize::new(0));
    let listener = UnixListener::bind(home.supervisor_sock()).unwrap();
    let seen = conns.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            seen.fetch_add(1, Ordering::SeqCst);
            let req = protocol::read_request(&mut s).await.unwrap();
            protocol::write_response(&mut s, &fleet_reply(req.id, &req.method))
                .await
                .unwrap();
        }
    });
    conns
}

async fn start_door(home: &AgentsHome) -> SocketAddr {
    let listener = door::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(door::serve(listener, home.clone(), Some(TAILNET.into())));
    addr
}

/// Forward bytes one way, keeping a copy of each.
fn pump<R, W>(mut r: R, mut w: W, log: Arc<Mutex<Vec<u8>>>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0 || w.write_all(&buf[..n]).await.is_err() {
                break;
            }
            log.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
}

/// A TCP hop that forwards both ways and keeps every byte it saw.
async fn tap(target: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let (inbound, _) = listener.accept().await.unwrap();
        let outbound = TcpStream::connect(target).await.unwrap();
        let (ri, wi) = inbound.into_split();
        let (ro, wo) = outbound.into_split();
        pump(ri, wo, log.clone());
        pump(ro, wi, log);
    });
    (addr, seen)
}

async fn open(
    addr: SocketAddr,
    device: &Keypair,
    daemon_public: &[u8; 32],
    claim: &[u8],
) -> Result<Channel<TcpStream>, ChannelError> {
    let stream = TcpStream::connect(addr).await.unwrap();
    channel::initiate(stream, device, daemon_public, claim).await
}

async fn call(ch: &mut Channel<TcpStream>, id: u64, method: &str) -> Response {
    let req = Request::new(id, method, json!({}));
    ch.send(&serde_json::to_vec(&req).unwrap()).await.unwrap();
    serde_json::from_slice(&ch.recv().await.unwrap()).unwrap()
}

/// Pair a fresh device from a new offer; returns its key and the offer.
async fn pair(
    home: &AgentsHome,
    addr: SocketAddr,
) -> (Keypair, pairing::ParsedOffer, Channel<TcpStream>) {
    let ticket = pairing::new_offer(home, TAILNET, addr.port(), 600, pairing::now_unix()).unwrap();
    let offer = pairing::parse_offer(&ticket.url).unwrap();
    let device = channel::generate_keypair().unwrap();
    let ch = open(addr, &device, &offer.daemon_public, &offer.claim("phone"))
        .await
        .unwrap();
    (device, offer, ch)
}

// AC3 + AC5: a device pairs from an offer and reads the fleet; the
// daemon's replies come back unchanged, one socket per request, and the
// captured TCP bytes hold no plaintext method or row.
#[tokio::test]
async fn paired_device_reads_the_fleet_and_the_wire_hides_it() {
    let (_dir, home) = home();
    let conns = fake_daemon(&home);
    let door_addr = start_door(&home).await;
    let (addr, seen) = tap(door_addr).await;
    let (_device, _offer, mut ch) = pair(&home, addr).await;

    for (id, method) in [(1, "agent.list"), (2, "agent.watch"), (3, "agent.truth")] {
        assert_eq!(call(&mut ch, id, method).await, fleet_reply(id, method));
    }
    assert_eq!(conns.load(Ordering::SeqCst), 3);
    let wire = seen.lock().unwrap().clone();
    assert!(!wire.is_empty());
    for plain in [&b"agent.list"[..], b"running", b"phone"] {
        assert!(!wire.windows(plain.len()).any(|w| w == plain));
    }
}

// AC6: a method off the allow list is refused by the door and the
// supervisor socket is never opened.
#[tokio::test]
async fn off_list_method_is_refused_without_touching_the_socket() {
    let (_dir, home) = home();
    let conns = fake_daemon(&home);
    let addr = start_door(&home).await;
    let (_device, _offer, mut ch) = pair(&home, addr).await;

    for method in ["agent.spawn", "agent.stop", "channel.push_to_channel"] {
        let resp = call(&mut ch, 9, method).await;
        assert_eq!(resp.error().map(|e| e.code), Some(ErrorCode::UnknownMethod));
    }
    assert_eq!(conns.load(Ordering::SeqCst), 0);
}

// AC4: a revoked device's key is refused with its reason, its open
// session ends at the next request, and a second device reusing a spent
// offer is refused.
#[tokio::test]
async fn revoked_device_and_spent_offer_are_refused() {
    let (_dir, home) = home();
    let addr = start_door(&home).await;
    let (device, offer, mut ch) = pair(&home, addr).await;
    // The session is live: the door itself answers an off-list method.
    let live = call(&mut ch, 1, "agent.spawn").await;
    assert_eq!(live.error().map(|e| e.code), Some(ErrorCode::UnknownMethod));

    let other = channel::generate_keypair().unwrap();
    let reused = open(addr, &other, &offer.daemon_public, &offer.claim("other")).await;
    assert!(
        matches!(reused, Err(ChannelError::Refused(r)) if r.ends_with("pairing offer already used"))
    );

    assert!(pairing::revoke(
        &home,
        &pairing::device_id(&device.public),
        pairing::now_unix()
    )
    .unwrap());
    let req = serde_json::to_vec(&Request::new(2, "agent.list", json!({}))).unwrap();
    ch.send(&req).await.unwrap();
    assert!(
        ch.recv().await.is_err(),
        "a revoked session must not answer"
    );
    let back = open(addr, &device, &offer.daemon_public, b"").await;
    assert!(matches!(back, Err(ChannelError::Refused(r)) if r.ends_with("revoked")));
}

// AC7 + AC8: the bound door reports itself listening with its port and
// tailnet name; with no daemon it answers daemon-down and starts none; a
// non-loopback bind is refused.
#[tokio::test]
async fn door_reports_listening_and_answers_daemon_down() {
    let (_dir, home) = home();
    let addr = start_door(&home).await;
    let (_device, _offer, mut ch) = pair(&home, addr).await;

    let status: door::DoorStatus =
        serde_json::from_slice(&std::fs::read(door::status_path(&home)).unwrap()).unwrap();
    assert_eq!(status.state, "listening");
    assert_eq!(status.port, addr.port());
    assert_eq!(status.tailnet_dns.as_deref(), Some(TAILNET));
    assert!(pairing::now_unix() - status.updated_at < 60);

    let resp = call(&mut ch, 4, "agent.status").await;
    let err = resp.error().expect("daemon down is an error");
    assert!(
        err.message.starts_with(door::DAEMON_DOWN),
        "{}",
        err.message
    );
    assert!(!home.supervisor_sock().exists());

    let wide = door::bind("0.0.0.0:0".parse().unwrap()).await;
    assert!(matches!(wide, Err(door::DoorError::NotLoopback(_))));
}
