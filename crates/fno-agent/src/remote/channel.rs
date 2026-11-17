//! The device channel: Noise IK over one TCP stream.
//!
//! The daemon side is the responder and holds a static key. A device is the
//! initiator and already knows the daemon's public key from the pairing QR,
//! which is what IK needs. Rulings D2503 (Noise IK through `snow`) and D2513
//! (tailnet, no relay).
//!
//! Wire: every Noise message travels as a 4-byte little-endian length, then
//! that many bytes (at most 65,535, the Noise limit). The handshake is two
//! messages. msg1 carries the device's claim (empty for a paired device, a
//! pairing secret for a new one); msg2 carries the door's verdict, so a
//! refused device learns why. After that, the plaintext stream is the
//! daemon's own frame format (protocol.rs: 4-byte LE length, then JSON),
//! sealed in chunks of at most [`CHUNK_PLAIN_MAX`] bytes.

use serde_json::json;
use snow::{Builder, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::MAX_FRAME_BYTES;

/// The one Noise suite the door speaks.
pub const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Both ends mix this into the handshake hash: a peer on another wire
/// version fails the handshake instead of misreading frames.
pub const PROLOGUE: &[u8] = b"fno-remote/1";
/// Noise's own cap on one message, ciphertext included.
pub const NOISE_MAX: usize = 65_535;
/// ChaChaPoly's tag rides on every transport message.
const TAG_LEN: usize = 16;
/// The most plaintext one transport message seals.
pub const CHUNK_PLAIN_MAX: usize = NOISE_MAX - TAG_LEN;
/// X25519 key length.
pub const KEY_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("wire message of {0} bytes is over the 65535-byte noise limit")]
    TooLong(usize),
    #[error("frame of {0} bytes is over the daemon frame cap")]
    FrameTooLarge(usize),
    #[error("a sealed chunk ran past the end of its frame")]
    Overrun,
    #[error("the handshake carried no usable static key")]
    NoStaticKey,
    #[error("refused: {0}")]
    Refused(String),
}

/// The door's reason for turning a device away. msg2 carries its text to
/// that device, so it must never hold a local path or other detail.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Rejection(pub String);

/// The first `n` bytes of `buf`. Snow never reports more than it wrote,
/// so a miss is a fault, not a panic.
fn upto(buf: &[u8], n: usize) -> Result<&[u8], ChannelError> {
    buf.get(..n).ok_or(ChannelError::TooLong(n))
}

impl ChannelError {
    /// The peer hung up between messages: a normal end, not a fault.
    pub fn is_eof(&self) -> bool {
        matches!(self, ChannelError::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof)
    }
}

/// An X25519 static keypair.
#[derive(Clone)]
pub struct Keypair {
    pub private: [u8; KEY_LEN],
    pub public: [u8; KEY_LEN],
}

fn params() -> snow::params::NoiseParams {
    PATTERN.parse().expect("the static noise pattern parses")
}

/// A 32-byte key from a slice, or None at any other length.
pub fn key32(bytes: &[u8]) -> Option<[u8; KEY_LEN]> {
    bytes.try_into().ok()
}

/// A fresh static keypair from the OS random source.
pub fn generate_keypair() -> Result<Keypair, ChannelError> {
    let kp = Builder::new(params()).generate_keypair()?;
    match (key32(&kp.private), key32(&kp.public)) {
        (Some(private), Some(public)) => Ok(Keypair { private, public }),
        _ => Err(ChannelError::NoStaticKey),
    }
}

async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> Result<(), ChannelError> {
    if bytes.len() > NOISE_MAX {
        return Err(ChannelError::TooLong(bytes.len()));
    }
    w.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, ChannelError> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > NOISE_MAX {
        return Err(ChannelError::TooLong(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

/// An open session: one stream, one Noise transport state.
pub struct Channel<S> {
    stream: S,
    noise: TransportState,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    /// Seal one daemon frame body and send it, split at the Noise limit.
    pub async fn send(&mut self, body: &[u8]) -> Result<(), ChannelError> {
        if body.len() > MAX_FRAME_BYTES as usize {
            return Err(ChannelError::FrameTooLarge(body.len()));
        }
        let mut plain = Vec::with_capacity(4 + body.len());
        plain.extend_from_slice(&(body.len() as u32).to_le_bytes());
        plain.extend_from_slice(body);
        let mut out = vec![0u8; NOISE_MAX];
        for chunk in plain.chunks(CHUNK_PLAIN_MAX) {
            let n = self.noise.write_message(chunk, &mut out)?;
            write_msg(&mut self.stream, upto(&out, n)?).await?;
        }
        Ok(())
    }

    /// Read and open one daemon frame body. A chunk that fails to decrypt
    /// is an error, and nothing from it is returned.
    pub async fn recv(&mut self) -> Result<Vec<u8>, ChannelError> {
        let mut plain: Vec<u8> = Vec::new();
        let mut want: Option<usize> = None;
        let mut buf = vec![0u8; NOISE_MAX];
        loop {
            let msg = read_msg(&mut self.stream).await?;
            let n = self.noise.read_message(&msg, &mut buf)?;
            plain.extend_from_slice(upto(&buf, n)?);
            if want.is_none() {
                if let [a, b, c, d, ..] = plain.as_slice() {
                    let len = u32::from_le_bytes([*a, *b, *c, *d]);
                    if len > MAX_FRAME_BYTES {
                        return Err(ChannelError::FrameTooLarge(len as usize));
                    }
                    want = Some(4 + len as usize);
                }
            }
            match want {
                Some(w) if plain.len() == w => return Ok(plain.split_off(4)),
                Some(w) if plain.len() > w => return Err(ChannelError::Overrun),
                _ => {}
            }
        }
    }

    /// The peer's static public key, proven by the handshake.
    pub fn remote_static(&self) -> Option<[u8; KEY_LEN]> {
        self.noise.get_remote_static().and_then(key32)
    }
}

/// The door's msg2 payload as the device reads it.
#[derive(serde::Deserialize)]
struct Verdict {
    ok: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// The door's msg2 payload: accept, or a refusal reason the device shows.
fn verdict_payload(refusal: Option<&Rejection>) -> Vec<u8> {
    let v = match refusal {
        None => json!({"ok": true}),
        Some(r) => json!({"ok": false, "reason": r.0}),
    };
    v.to_string().into_bytes()
}

/// Device side: open a session to the door at the other end of `stream`.
/// `claim` is the msg1 payload: empty for a paired device, the pairing
/// claim for a new one. A refusal comes back as [`ChannelError::Refused`].
pub async fn initiate<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    device: &Keypair,
    daemon_public: &[u8; KEY_LEN],
    claim: &[u8],
) -> Result<Channel<S>, ChannelError> {
    let mut hs = Builder::new(params())
        .local_private_key(&device.private)?
        .remote_public_key(daemon_public)?
        .prologue(PROLOGUE)?
        .build_initiator()?;
    let mut buf = vec![0u8; NOISE_MAX];
    let n = hs.write_message(claim, &mut buf)?;
    write_msg(&mut stream, upto(&buf, n)?).await?;
    let msg = read_msg(&mut stream).await?;
    let mut reply = vec![0u8; NOISE_MAX];
    let m = hs.read_message(&msg, &mut reply)?;
    let verdict: Verdict = serde_json::from_slice(upto(&reply, m)?)
        .map_err(|e| ChannelError::Refused(format!("unreadable door verdict: {e}")))?;
    if !verdict.ok {
        let reason = verdict.reason.unwrap_or_else(|| "no reason given".into());
        return Err(ChannelError::Refused(reason));
    }
    Ok(Channel {
        stream,
        noise: hs.into_transport_mode()?,
    })
}

/// Door side: answer one handshake. `authorize` sees the device's proven
/// static key and its msg1 claim, and decides. A refusal still answers
/// msg2 (sealed to that device alone) so the device can show the reason,
/// then the session ends.
pub async fn respond<S, T, F>(
    mut stream: S,
    daemon: &Keypair,
    authorize: F,
) -> Result<(Channel<S>, T), ChannelError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&[u8; KEY_LEN], &[u8]) -> Result<T, Rejection>,
{
    let mut hs = Builder::new(params())
        .local_private_key(&daemon.private)?
        .prologue(PROLOGUE)?
        .build_responder()?;
    let msg = read_msg(&mut stream).await?;
    let mut claim = vec![0u8; NOISE_MAX];
    let n = hs.read_message(&msg, &mut claim)?;
    let remote = hs
        .get_remote_static()
        .and_then(key32)
        .ok_or(ChannelError::NoStaticKey)?;
    let outcome = authorize(&remote, upto(&claim, n)?);
    let mut out = vec![0u8; NOISE_MAX];
    let m = hs.write_message(&verdict_payload(outcome.as_ref().err()), &mut out)?;
    write_msg(&mut stream, upto(&out, m)?).await?;
    match outcome {
        Ok(t) => Ok((
            Channel {
                stream,
                noise: hs.into_transport_mode()?,
            },
            t,
        )),
        Err(reason) => Err(ChannelError::Refused(reason.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    async fn pair_up(
        daemon: &Keypair,
        device: &Keypair,
    ) -> (
        Channel<tokio::io::DuplexStream>,
        Channel<tokio::io::DuplexStream>,
    ) {
        let (a, b) = duplex(1 << 20);
        let known = device.public;
        let door = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                respond(b, &daemon, |key, _| {
                    if *key == known {
                        Ok(())
                    } else {
                        Err(Rejection("unknown device".into()))
                    }
                })
                .await
            }
        });
        let dev = initiate(a, device, &daemon.public, b"").await.unwrap();
        let (door, ()) = door.await.unwrap().unwrap();
        (dev, door)
    }

    // AC1-HP: the door opens the exact bytes, a frame past one Noise
    // message included, and a second session seals the same frame to
    // different ciphertext.
    #[tokio::test]
    async fn sealed_frame_opens_exactly_and_sessions_differ() {
        let daemon = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let (mut dev, mut door) = pair_up(&daemon, &device).await;
        dev.send(&big).await.unwrap();
        assert_eq!(door.recv().await.unwrap(), big);
        door.send(b"{\"id\":1}").await.unwrap();
        assert_eq!(dev.recv().await.unwrap(), b"{\"id\":1}");
        assert_eq!(door.remote_static(), Some(device.public));

        let mut sealed = Vec::new();
        for _ in 0..2 {
            // Both writes are the first transport message (nonce 0), so only
            // the session keys differ between them.
            let (mut dev, _door) = pair_up(&daemon, &device).await;
            let mut out = vec![0u8; 64];
            let m = dev.noise.write_message(b"same", &mut out).unwrap();
            sealed.push(out.get(..m).unwrap().to_vec());
        }
        assert_ne!(sealed.first(), sealed.get(1));
    }

    // AC2-ERR: a flipped ciphertext byte fails to open and yields nothing;
    // an unknown device key is refused with its reason.
    #[tokio::test]
    async fn flipped_byte_and_unknown_key_are_refused() {
        let daemon = generate_keypair().unwrap();
        let device = generate_keypair().unwrap();
        let (mut dev, mut door) = pair_up(&daemon, &device).await;
        let mut out = vec![0u8; 64];
        let n = dev.noise.write_message(b"\x02\0\0\0{}", &mut out).unwrap();
        *out.get_mut(n - 1).unwrap() ^= 0x01;
        write_msg(&mut dev.stream, out.get(..n).unwrap())
            .await
            .unwrap();
        assert!(matches!(door.recv().await, Err(ChannelError::Noise(_))));

        let stranger = generate_keypair().unwrap();
        let (a, b) = duplex(1 << 16);
        let known = device.public;
        let door = tokio::spawn({
            let daemon = daemon.clone();
            async move {
                respond(b, &daemon, |key, _| {
                    if *key == known {
                        Ok(())
                    } else {
                        Err(Rejection("unknown device".into()))
                    }
                })
                .await
            }
        });
        let dev = initiate(a, &stranger, &daemon.public, b"").await;
        assert!(matches!(dev, Err(ChannelError::Refused(r)) if r == "unknown device"));
        assert!(matches!(door.await.unwrap(), Err(ChannelError::Refused(_))));
    }
}
