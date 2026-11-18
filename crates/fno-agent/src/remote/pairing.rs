//! Pairing: the daemon's static key, the device store, one-time offers and
//! revoke.
//!
//! Both files live under `<agents home>/remote/` (0700) at mode 0600, and a
//! load refuses a file at any other mode, the way the daemon refuses a
//! loose socket directory.
//!
//! An offer is a URL the Mac prints as a QR:
//! `fno-remote://<tailnet-dns>:<port>#<base64url(daemon_pub || secret || expiry)>`,
//! with a 32-byte key, a 32-byte secret and an 8-byte big-endian unix expiry.
//! The device opens Noise IK to the door with the secret in its first
//! message. The store keeps only the secret's SHA-256, marks it used on
//! first success, and never accepts it again.

use std::io::{Read, Write};
use std::path::Path;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::channel::{key32, Keypair, KEY_LEN};
use crate::paths::{self, AgentsHome};

/// URL scheme of a pairing offer.
pub const OFFER_SCHEME: &str = "fno-remote://";
const SECRET_LEN: usize = 32;
/// Used and expired offers are kept this long so a late device reads
/// "used" or "expired" instead of "unknown", then pruned.
const OFFER_KEEP_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("io on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{0} is not mode 0600; refusing to read a key or device file others can open")]
    LooseFile(String),
    #[error("{path} is unreadable: {detail}")]
    Corrupt { path: String, detail: String },
    #[error("noise: {0}")]
    Channel(#[from] super::channel::ChannelError),
    #[error("os random source: {0}")]
    Random(String),
    #[error("bad offer: {0}")]
    BadOffer(String),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> PairError + '_ {
    move |source| PairError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Seconds since the unix epoch.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Create the remote dir at 0700 under a 0700 agents root.
fn ensure_dir(home: &AgentsHome) -> Result<(), PairError> {
    home.ensure_root().map_err(io_err(home.root()))?;
    let dir = home.remote_dir();
    std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    paths::set_dir_mode_0700(&dir).map_err(io_err(&dir))
}

/// Read a 0600 file; `None` when it does not exist.
fn read_private(path: &Path) -> Result<Option<Vec<u8>>, PairError> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path)(e)),
    };
    // fstat the open handle, so a swap between check and read cannot pass.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file.metadata().map_err(io_err(path))?.permissions().mode();
        if mode & 0o777 != 0o600 {
            return Err(PairError::LooseFile(path.display().to_string()));
        }
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(io_err(path))?;
    Ok(Some(bytes))
}

/// Write a file at 0600 through a temp file and rename.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), PairError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(dir))?;
    paths::set_file_mode_0600(tmp.path()).map_err(io_err(path))?;
    tmp.write_all(bytes).map_err(io_err(path))?;
    tmp.as_file().sync_all().map_err(io_err(path))?;
    tmp.persist(path).map_err(|e| io_err(path)(e.error))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    private: String,
    public: String,
}

/// Take the remote dir's exclusive lock. The door and the pair/revoke
/// commands are separate processes; the lock keeps a pairing and a revoke
/// from losing each other's write, and keeps a first-run door and pair
/// from minting two different daemon keys.
fn lock_remote(home: &AgentsHome) -> Result<std::fs::File, PairError> {
    ensure_dir(home)?;
    let lock_path = home.remote_devices_json().with_extension("json.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(io_err(&lock_path))?;
    paths::set_file_mode_0600(&lock_path).map_err(io_err(&lock_path))?;
    lock.lock().map_err(io_err(&lock_path))?;
    Ok(lock)
}

/// The daemon's static key, made on first use.
pub fn load_or_create_daemon_key(home: &AgentsHome) -> Result<Keypair, PairError> {
    let _lock = lock_remote(home)?;
    let path = home.remote_daemon_key();
    if let Some(bytes) = read_private(&path)? {
        let corrupt = |detail: String| PairError::Corrupt {
            path: path.display().to_string(),
            detail,
        };
        let file: KeyFile = serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))?;
        let decode = |s: &str| STANDARD.decode(s).ok().as_deref().and_then(key32);
        return match (decode(&file.private), decode(&file.public)) {
            (Some(private), Some(public)) => Ok(Keypair { private, public }),
            _ => Err(corrupt("a key is not 32 base64 bytes".into())),
        };
    }
    let kp = super::channel::generate_keypair()?;
    let file = KeyFile {
        private: STANDARD.encode(kp.private),
        public: STANDARD.encode(kp.public),
    };
    let body = serde_json::to_vec_pretty(&file).expect("key file serializes");
    write_private(&path, &body)?;
    Ok(kp)
}

/// One paired device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Base64 of the device's X25519 static public key.
    pub public_key: String,
    pub paired_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

/// One pairing offer. Only the secret's hash is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub secret_sha256: String,
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_at: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceStore {
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub offers: Vec<Offer>,
}

/// A device's id: 12 hex chars of BLAKE3 over its public key. Stable for a
/// key, so logs name the same device across sessions.
pub fn device_id(public: &[u8; KEY_LEN]) -> String {
    blake3::hash(public).to_hex().as_str()[..12].to_string()
}

fn secret_hash(secret: &[u8]) -> String {
    Sha256::digest(secret)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Why the door turned a device away. The door logs it; msg2 carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Revoked { device: String },
    UnknownDevice { device: String },
    OfferUnknown { device: String },
    OfferUsed { device: String },
    OfferExpired { device: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Revoked { device } => write!(f, "device {device}: revoked"),
            Refusal::UnknownDevice { device } => {
                write!(
                    f,
                    "device {device}: not paired and no pairing secret offered"
                )
            }
            Refusal::OfferUnknown { device } => {
                write!(f, "device {device}: pairing secret matches no offer")
            }
            Refusal::OfferUsed { device } => {
                write!(f, "device {device}: pairing offer already used")
            }
            Refusal::OfferExpired { device } => write!(f, "device {device}: pairing offer expired"),
        }
    }
}

/// What a new device sends in msg1.
#[derive(Debug, Serialize, Deserialize)]
pub struct PairClaim {
    /// Base64url (no padding) of the offer's secret.
    pub pair: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Decide one handshake against the store. A paired key passes and its
/// claim is ignored. An unknown key passes only with a live offer's
/// secret, which this call consumes. A revoked key never passes, even with
/// a fresh offer: that device must make a new key.
pub fn authorize(
    store: &mut DeviceStore,
    key: &[u8; KEY_LEN],
    claim: &[u8],
    now: u64,
) -> Result<Device, Refusal> {
    let id = device_id(key);
    let encoded = STANDARD.encode(key);
    if let Some(dev) = store.devices.iter().find(|d| d.public_key == encoded) {
        return match dev.revoked_at {
            Some(_) => Err(Refusal::Revoked { device: id }),
            None => Ok(dev.clone()),
        };
    }
    let unknown = || Refusal::UnknownDevice { device: id.clone() };
    let claim: PairClaim = serde_json::from_slice(claim).map_err(|_| unknown())?;
    let secret = URL_SAFE_NO_PAD
        .decode(claim.pair.as_bytes())
        .map_err(|_| unknown())?;
    let hash = secret_hash(&secret);
    let Some(offer) = store.offers.iter_mut().find(|o| o.secret_sha256 == hash) else {
        return Err(Refusal::OfferUnknown { device: id });
    };
    if offer.used_at.is_some() {
        return Err(Refusal::OfferUsed { device: id });
    }
    if offer.expires_at <= now {
        return Err(Refusal::OfferExpired { device: id });
    }
    offer.used_at = Some(now);
    let device = Device {
        id: id.clone(),
        name: claim.name.unwrap_or_else(|| format!("device-{id}")),
        public_key: encoded,
        paired_at: now,
        revoked_at: None,
    };
    store.devices.push(device.clone());
    Ok(device)
}

/// Run `f` on the device store under the remote lock, writing it back
/// when `f` changed it.
pub fn with_store<R>(
    home: &AgentsHome,
    f: impl FnOnce(&mut DeviceStore) -> R,
) -> Result<R, PairError> {
    let _lock = lock_remote(home)?;
    let path = home.remote_devices_json();
    let before: DeviceStore = match read_private(&path)? {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| PairError::Corrupt {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?,
        None => DeviceStore::default(),
    };
    let mut store = before.clone();
    let out = f(&mut store);
    if store != before {
        let body = serde_json::to_vec_pretty(&store).expect("device store serializes");
        write_private(&path, &body)?;
    }
    Ok(out)
}

/// A fresh offer: its URL and expiry. The secret leaves this process only
/// inside the URL.
pub struct Ticket {
    pub url: String,
    pub expires_at: u64,
}

/// Mint an offer for `host:port` that lives `ttl_secs`.
pub fn new_offer(
    home: &AgentsHome,
    host: &str,
    port: u16,
    ttl_secs: u64,
    now: u64,
) -> Result<Ticket, PairError> {
    let daemon = load_or_create_daemon_key(home)?;
    let mut secret = [0u8; SECRET_LEN];
    getrandom::fill(&mut secret).map_err(|e| PairError::Random(e.to_string()))?;
    let expires_at = now + ttl_secs;
    with_store(home, |store| {
        store.offers.retain(|o| {
            let ended = o.used_at.unwrap_or(o.expires_at).min(o.expires_at);
            ended + OFFER_KEEP_SECS > now
        });
        store.offers.push(Offer {
            secret_sha256: secret_hash(&secret),
            expires_at,
            used_at: None,
        });
    })?;
    let mut blob = Vec::with_capacity(KEY_LEN + SECRET_LEN + 8);
    blob.extend_from_slice(&daemon.public);
    blob.extend_from_slice(&secret);
    blob.extend_from_slice(&expires_at.to_be_bytes());
    Ok(Ticket {
        url: format!(
            "{OFFER_SCHEME}{host}:{port}#{}",
            URL_SAFE_NO_PAD.encode(blob)
        ),
        expires_at,
    })
}

/// An offer URL, read back: what a device scans.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOffer {
    pub host: String,
    pub port: u16,
    pub daemon_public: [u8; KEY_LEN],
    pub secret: [u8; SECRET_LEN],
    pub expires_at: u64,
}

impl ParsedOffer {
    /// The msg1 claim a new device sends with this offer.
    pub fn claim(&self, name: &str) -> Vec<u8> {
        serde_json::to_vec(&PairClaim {
            pair: URL_SAFE_NO_PAD.encode(self.secret),
            name: Some(name.to_string()),
        })
        .expect("claim serializes")
    }
}

/// Parse an offer URL.
pub fn parse_offer(url: &str) -> Result<ParsedOffer, PairError> {
    let bad = |m: &str| PairError::BadOffer(m.to_string());
    let rest = url
        .strip_prefix(OFFER_SCHEME)
        .ok_or_else(|| bad("not a fno-remote:// url"))?;
    let (addr, frag) = rest.split_once('#').ok_or_else(|| bad("no # fragment"))?;
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| bad("no :port"))?;
    let port: u16 = port.parse().map_err(|_| bad("port is not a number"))?;
    let blob = URL_SAFE_NO_PAD
        .decode(frag.as_bytes())
        .map_err(|_| bad("fragment is not base64url"))?;
    let short = || bad("fragment is not 72 bytes");
    let (key, rest) = blob.split_first_chunk::<KEY_LEN>().ok_or_else(short)?;
    let (secret, expiry) = rest.split_first_chunk::<SECRET_LEN>().ok_or_else(short)?;
    let expiry: [u8; 8] = expiry.try_into().map_err(|_| short())?;
    Ok(ParsedOffer {
        host: host.to_string(),
        port,
        daemon_public: *key,
        secret: *secret,
        expires_at: u64::from_be_bytes(expiry),
    })
}

/// Mark a device revoked. False when no live device has that id.
pub fn revoke(home: &AgentsHome, id: &str, now: u64) -> Result<bool, PairError> {
    with_store(home, |store| {
        match store
            .devices
            .iter_mut()
            .find(|d| d.id == id && d.revoked_at.is_none())
        {
            Some(dev) => {
                dev.revoked_at = Some(now);
                true
            }
            None => false,
        }
    })
}

/// The offer URL as a terminal QR, light modules drawn dark so it scans on
/// a dark terminal.
pub fn render_qr(url: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, AgentsHome) {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path().join("agents"));
        (dir, home)
    }

    // AC3 + AC4: a pairing lands in a 0600 store that keeps only the
    // secret's hash; a used, expired or revoked pairing is refused with its
    // reason; a loose store file is refused outright. The end-to-end leg
    // is tests/remote_door.rs.
    #[test]
    fn used_expired_and_revoked_pairings_are_refused() {
        let (_dir, home) = home();
        let gen = super::super::channel::generate_keypair;
        let offer = parse_offer(&new_offer(&home, "h", 1, 600, 1_000).unwrap().url).unwrap();
        let (a, b, c) = (gen().unwrap(), gen().unwrap(), gen().unwrap());
        let run = |k: &Keypair, claim: &[u8], now| {
            with_store(&home, |s| authorize(s, &k.public, claim, now)).unwrap()
        };
        assert!(run(&a, &offer.claim("a"), 1_100).is_ok());
        assert!(paths::is_file_mode_0600(&home.remote_devices_json()));
        assert!(paths::is_file_mode_0600(&home.remote_daemon_key()));
        assert!(paths::is_dir_mode_0700(&home.remote_dir()));
        let on_disk = std::fs::read_to_string(home.remote_devices_json()).unwrap();
        assert!(!on_disk.contains(&URL_SAFE_NO_PAD.encode(offer.secret)));
        assert_eq!(
            run(&b, &offer.claim("b"), 1_100),
            Err(Refusal::OfferUsed {
                device: device_id(&b.public)
            })
        );
        let late = parse_offer(&new_offer(&home, "h", 1, 600, 2_000).unwrap().url).unwrap();
        assert_eq!(
            run(&c, &late.claim("c"), 2_600),
            Err(Refusal::OfferExpired {
                device: device_id(&c.public)
            })
        );
        assert_eq!(
            run(&c, b"", 2_600),
            Err(Refusal::UnknownDevice {
                device: device_id(&c.public)
            })
        );
        assert!(revoke(&home, &device_id(&a.public), 3_000).unwrap());
        assert_eq!(
            run(&a, b"", 3_100),
            Err(Refusal::Revoked {
                device: device_id(&a.public)
            })
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let loose = std::fs::Permissions::from_mode(0o644);
            std::fs::set_permissions(home.remote_devices_json(), loose).unwrap();
            assert!(matches!(
                with_store(&home, |_| ()),
                Err(PairError::LooseFile(_))
            ));
        }
    }
}
