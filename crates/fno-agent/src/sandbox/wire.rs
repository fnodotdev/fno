//! The frames on the one exec stdio stream between the host relay and the
//! guest. Default deny: a frame is one of these variants with typed fields,
//! and a frame never carries an argv, env name, host path, session id, node,
//! recipient or credential that the host trusts. The host stamps identity
//! from the pipe the frame arrived on.
//!
//! Layout: `kind u8`, `len u32` big-endian, then `len` bytes of body. A body
//! over [`MAX_BODY`] is refused before allocation. Each side decodes only the
//! kinds the other side may send, so a guest cannot forge a host frame.

use std::io::{self, Read, Write};

/// The largest body either side accepts. A transcript or data chunk is cut
/// to [`CHUNK`] before it is framed, so this only bounds a hostile prefix.
pub const MAX_BODY: u32 = 1024 * 1024;
/// The size a sender cuts bulk bytes to.
pub const CHUNK: usize = 64 * 1024;

/// The three states `agent.report` stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportState {
    Working,
    Blocked,
    Done,
}

impl ReportState {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

/// What the guest may send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestFrame {
    /// `agent.status` from a client inside. The host answers locally.
    Status {
        req: u32,
    },
    /// `agent.report`: only the state. The host stamps who.
    Report {
        req: u32,
        state: ReportState,
    },
    /// `agent.session-report`: only the start source. The host builds every
    /// other param from the spawn it owns.
    SessionReport {
        req: u32,
        source: String,
    },
    /// Transcript bytes at `offset` of the one harness file.
    Transcript {
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Open an outbound connection for the proxy. The host resolves and
    /// checks every address before it dials.
    Connect {
        stream: u32,
        host: String,
        port: u16,
    },
    Data {
        stream: u32,
        bytes: Vec<u8>,
    },
    Close {
        stream: u32,
    },
    /// The transcript is flushed to EOF after a [`HostFrame::Shutdown`].
    Flushed,
    /// One loop op. `seq` is unique per request, so the host never runs a
    /// replayed write twice. The op is typed; its text crosses by value.
    Op {
        req: u32,
        seq: u64,
        op: super::ops::Op,
    },
}

/// What the host may send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFrame {
    /// The launch secrets, once, before anything else: env pairs to set
    /// (`Some`) or unset (`None`), and route settings content for the guest
    /// copy. They cross stdin, never argv, a file or a shared folder.
    Hello {
        env: Vec<(String, Option<String>)>,
        settings: Option<Vec<u8>>,
        transcript_dir: Option<String>,
    },
    /// The answer to a request frame: an `agent.*` result or error.
    Reply {
        req: u32,
        ok: bool,
        body: String,
    },
    Connected {
        stream: u32,
        ok: bool,
        reason: String,
    },
    Data {
        stream: u32,
        bytes: Vec<u8>,
    },
    Close {
        stream: u32,
    },
    /// The harness exited: flush the transcript and answer `Flushed`.
    Shutdown,
}

mod kind {
    pub const STATUS: u8 = 1;
    pub const REPORT: u8 = 2;
    pub const SESSION_REPORT: u8 = 3;
    pub const TRANSCRIPT: u8 = 4;
    pub const CONNECT: u8 = 5;
    pub const DATA: u8 = 6;
    pub const CLOSE: u8 = 7;
    pub const FLUSHED: u8 = 8;
    pub const OP: u8 = 9;
    pub const HELLO: u8 = 64;
    pub const REPLY: u8 = 65;
    pub const CONNECTED: u8 = 66;
    pub const SHUTDOWN: u8 = 67;
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("frame body {0} bytes is over the {MAX_BODY}-byte cap")]
    TooLarge(u32),
    #[error("frame kind {0} is not allowed in this direction")]
    Refused(u8),
    #[error("malformed frame kind {0}")]
    Malformed(u8),
}

/// A body reader with a cursor. Every take is bounds-checked, so a short or
/// lying body is `Malformed`, never a panic.
struct Body<'a> {
    kind: u8,
    buf: &'a [u8],
}

impl<'a> Body<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.buf.len() < n {
            return Err(WireError::Malformed(self.kind));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(WireError::Malformed(self.kind))
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        let kind = self.kind;
        let b: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| WireError::Malformed(kind))?;
        Ok(u16::from_be_bytes(b))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        let kind = self.kind;
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| WireError::Malformed(kind))?;
        Ok(u32::from_be_bytes(b))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(a))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, WireError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> Result<String, WireError> {
        String::from_utf8(self.bytes()?).map_err(|_| WireError::Malformed(self.kind))
    }
    fn opt_bytes(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.bytes()?)),
            _ => Err(WireError::Malformed(self.kind)),
        }
    }
    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf).to_vec()
    }
    fn end(self) -> Result<(), WireError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(WireError::Malformed(self.kind))
        }
    }
}

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(self, b: &[u8]) -> Self {
        let mut s = self.u32(b.len() as u32);
        s.0.extend_from_slice(b);
        s
    }
    fn raw(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }
    fn opt_bytes(self, b: Option<&[u8]>) -> Self {
        match b {
            None => self.u8(0),
            Some(b) => self.u8(1).bytes(b),
        }
    }
}

fn write_raw<W: Write>(w: &mut W, kind: u8, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.push(kind);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    w.write_all(&frame)?;
    w.flush()
}

/// Read one raw frame. `Ok(None)` is a clean EOF at a frame boundary.
fn read_raw<R: Read>(r: &mut R) -> Result<Option<(u8, Vec<u8>)>, WireError> {
    let mut hdr = [0u8; 5];
    let mut got = 0;
    while got < hdr.len() {
        match r.read(hdr.get_mut(got..).unwrap_or_default())? {
            0 if got == 0 => return Ok(None),
            0 => return Err(WireError::Io(io::ErrorKind::UnexpectedEof.into())),
            n => got += n,
        }
    }
    let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
    if len > MAX_BODY {
        return Err(WireError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(Some((hdr[0], body)))
}

impl GuestFrame {
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let (k, body) = match self {
            Self::Status { req } => (kind::STATUS, Out::default().u32(*req)),
            Self::Report { req, state } => (
                kind::REPORT,
                Out::default().u32(*req).bytes(state.as_str().as_bytes()),
            ),
            Self::SessionReport { req, source } => (
                kind::SESSION_REPORT,
                Out::default().u32(*req).bytes(source.as_bytes()),
            ),
            Self::Transcript { offset, bytes } => {
                (kind::TRANSCRIPT, Out::default().u64(*offset).raw(bytes))
            }
            Self::Connect { stream, host, port } => (
                kind::CONNECT,
                Out::default()
                    .u32(*stream)
                    .bytes(host.as_bytes())
                    .u16(*port),
            ),
            Self::Data { stream, bytes } => (kind::DATA, Out::default().u32(*stream).raw(bytes)),
            Self::Close { stream } => (kind::CLOSE, Out::default().u32(*stream)),
            Self::Flushed => (kind::FLUSHED, Out::default()),
            Self::Op { req, seq, op } => (
                kind::OP,
                Out::default()
                    .u32(*req)
                    .u64(*seq)
                    .raw(&serde_json::to_vec(op).map_err(io::Error::other)?),
            ),
        };
        write_raw(w, k, &body.0)
    }

    /// The host's reader. A host-only kind from the guest is refused.
    pub fn read<R: Read>(r: &mut R) -> Result<Option<Self>, WireError> {
        let Some((k, buf)) = read_raw(r)? else {
            return Ok(None);
        };
        let mut b = Body { kind: k, buf: &buf };
        let frame = match k {
            kind::STATUS => Self::Status { req: b.u32()? },
            kind::REPORT => {
                let req = b.u32()?;
                let state = ReportState::parse(&b.string()?).ok_or(WireError::Malformed(k))?;
                Self::Report { req, state }
            }
            kind::SESSION_REPORT => Self::SessionReport {
                req: b.u32()?,
                source: b.string()?,
            },
            kind::TRANSCRIPT => Self::Transcript {
                offset: b.u64()?,
                bytes: b.rest(),
            },
            kind::CONNECT => Self::Connect {
                stream: b.u32()?,
                host: b.string()?,
                port: b.u16()?,
            },
            kind::DATA => Self::Data {
                stream: b.u32()?,
                bytes: b.rest(),
            },
            kind::CLOSE => Self::Close { stream: b.u32()? },
            kind::FLUSHED => Self::Flushed,
            kind::OP => Self::Op {
                req: b.u32()?,
                seq: b.u64()?,
                op: serde_json::from_slice(&b.rest()).map_err(|_| WireError::Malformed(k))?,
            },
            other => return Err(WireError::Refused(other)),
        };
        b.end()?;
        Ok(Some(frame))
    }
}

impl HostFrame {
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let (k, body) = match self {
            Self::Hello {
                env,
                settings,
                transcript_dir,
            } => {
                let mut o = Out::default().u32(env.len() as u32);
                for (key, val) in env {
                    o = o
                        .bytes(key.as_bytes())
                        .opt_bytes(val.as_deref().map(str::as_bytes));
                }
                o = o
                    .opt_bytes(settings.as_deref())
                    .opt_bytes(transcript_dir.as_deref().map(str::as_bytes));
                (kind::HELLO, o)
            }
            Self::Reply { req, ok, body } => (
                kind::REPLY,
                Out::default()
                    .u32(*req)
                    .u8(u8::from(*ok))
                    .bytes(body.as_bytes()),
            ),
            Self::Connected { stream, ok, reason } => (
                kind::CONNECTED,
                Out::default()
                    .u32(*stream)
                    .u8(u8::from(*ok))
                    .bytes(reason.as_bytes()),
            ),
            Self::Data { stream, bytes } => (kind::DATA, Out::default().u32(*stream).raw(bytes)),
            Self::Close { stream } => (kind::CLOSE, Out::default().u32(*stream)),
            Self::Shutdown => (kind::SHUTDOWN, Out::default()),
        };
        write_raw(w, k, &body.0)
    }

    /// The guest's reader. A guest-only kind from the host is refused.
    pub fn read<R: Read>(r: &mut R) -> Result<Option<Self>, WireError> {
        let Some((k, buf)) = read_raw(r)? else {
            return Ok(None);
        };
        let mut b = Body { kind: k, buf: &buf };
        let frame = match k {
            kind::HELLO => {
                let n = b.u32()? as usize;
                let mut env = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    let key = b.string()?;
                    let val = match b.opt_bytes()? {
                        None => None,
                        Some(v) => Some(String::from_utf8(v).map_err(|_| WireError::Malformed(k))?),
                    };
                    env.push((key, val));
                }
                let settings = b.opt_bytes()?;
                let transcript_dir = match b.opt_bytes()? {
                    None => None,
                    Some(v) => Some(String::from_utf8(v).map_err(|_| WireError::Malformed(k))?),
                };
                Self::Hello {
                    env,
                    settings,
                    transcript_dir,
                }
            }
            kind::REPLY => Self::Reply {
                req: b.u32()?,
                ok: b.u8()? != 0,
                body: b.string()?,
            },
            kind::CONNECTED => Self::Connected {
                stream: b.u32()?,
                ok: b.u8()? != 0,
                reason: b.string()?,
            },
            kind::DATA => Self::Data {
                stream: b.u32()?,
                bytes: b.rest(),
            },
            kind::CLOSE => Self::Close { stream: b.u32()? },
            kind::SHUTDOWN => Self::Shutdown,
            other => return Err(WireError::Refused(other)),
        };
        b.end()?;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frame round-trips in its own direction; the other direction's
    /// kinds, an oversized prefix and a short body refuse without panicking.
    #[test]
    fn frames_round_trip_and_refuse_the_wrong_direction() {
        let guest = [
            GuestFrame::Status { req: 1 },
            GuestFrame::Report {
                req: 2,
                state: ReportState::Blocked,
            },
            GuestFrame::SessionReport {
                req: 3,
                source: "startup".into(),
            },
            GuestFrame::Transcript {
                offset: 9,
                bytes: b"{}\n".to_vec(),
            },
            GuestFrame::Connect {
                stream: 4,
                host: "example.com".into(),
                port: 443,
            },
            GuestFrame::Data {
                stream: 4,
                bytes: vec![0, 1, 2],
            },
            GuestFrame::Close { stream: 4 },
            GuestFrame::Flushed,
            GuestFrame::Op {
                req: 5,
                seq: 6,
                op: crate::sandbox::ops::Op::PlanSettle {
                    item: "U1".into(),
                    text: "measured".into(),
                },
            },
        ];
        let host = [
            HostFrame::Hello {
                env: vec![("K".into(), Some("v'1".into())), ("GONE".into(), None)],
                settings: Some(b"{}".to_vec()),
                transcript_dir: None,
            },
            HostFrame::Reply {
                req: 1,
                ok: false,
                body: "refused".into(),
            },
            HostFrame::Connected {
                stream: 4,
                ok: true,
                reason: String::new(),
            },
            HostFrame::Data {
                stream: 4,
                bytes: vec![9],
            },
            HostFrame::Close { stream: 4 },
            HostFrame::Shutdown,
        ];
        for f in &guest {
            let mut buf = Vec::new();
            f.write(&mut buf).unwrap();
            assert_eq!(
                GuestFrame::read(&mut buf.as_slice()).unwrap().as_ref(),
                Some(f)
            );
        }
        for f in &host {
            let mut buf = Vec::new();
            f.write(&mut buf).unwrap();
            assert_eq!(
                HostFrame::read(&mut buf.as_slice()).unwrap().as_ref(),
                Some(f)
            );
        }
        let mut forged = Vec::new();
        host[0].write(&mut forged).unwrap();
        assert!(matches!(
            GuestFrame::read(&mut forged.as_slice()),
            Err(WireError::Refused(kind::HELLO))
        ));
        let huge = [kind::DATA, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            GuestFrame::read(&mut huge.as_slice()),
            Err(WireError::TooLarge(_))
        ));
        let short = [kind::CONNECT, 0, 0, 0, 2, 0, 0];
        assert!(matches!(
            GuestFrame::read(&mut short.as_slice()),
            Err(WireError::Malformed(kind::CONNECT))
        ));
        let untyped = [
            kind::OP,
            0,
            0,
            0,
            14,
            0,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            b'{',
            b'}',
        ];
        assert!(matches!(
            GuestFrame::read(&mut untyped.as_slice()),
            Err(WireError::Malformed(kind::OP))
        ));
        let empty: &[u8] = &[];
        assert!(GuestFrame::read(&mut &empty[..]).unwrap().is_none());
    }
}
