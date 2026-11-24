//! The sandbox seam: one container per headless spawn, reached only through
//! the exec stdio stream the host relay owns.
//!
//! - [`wire`]: the typed frames, default deny.
//! - [`guest`]: `fno-agent hook sandbox-guest`, inside the container.
//! - [`relay`]: `fno-agent hook sandbox-run`, the host lifecycle.
//! - [`engine`]: docker and podman calls, each under a timeout.
//! - [`repo`]: the clone inside, and the bundle back to `refs/sandbox/<id>/`.
//! - [`egress`]: the host half of the proxy, private ranges closed.
//! - [`transcript`]: the one host file per sandbox.
//!
//! The relay stamps identity; the guest never does. The host never checks
//! out, extracts or executes sandbox-written content, and no failure falls
//! back to a host launch.

pub mod egress;
pub mod engine;
pub mod guest;
pub mod relay;
pub mod repo;
pub mod transcript;
pub mod wire;

/// Why the sandbox refused a step. Every refusal names what it refused and
/// why, and no failure falls back to a host launch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Refusal(pub String);

impl From<String> for Refusal {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Refusal {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<Refusal> for String {
    fn from(r: Refusal) -> Self {
        r.0
    }
}

/// A sandbox step's result.
pub type Res<T> = Result<T, Refusal>;

/// The pinned client socket inside the container. `FNO_SUPERVISOR_SOCKET`
/// points here, so a client inside needs no change.
pub const CONTAINER_SOCK: &str = "/tmp/fno/supervisor.sock";
/// The guest copy of route `--settings` content.
pub const CONTAINER_SETTINGS: &str = "/tmp/fno/settings.json";
/// Where the clone lives inside.
pub const CONTAINER_WORK: &str = "/work";
/// The guest's proxy, on the container's own loopback.
pub const PROXY_ADDR: &str = "127.0.0.1:3128";
/// The guest-local method the launcher calls for its env. It never crosses
/// the stream.
pub const LAUNCH_METHOD: &str = "sandbox.launch";
/// The label every fno sandbox container carries, with the wrapper pid
/// beside it so a sweep can tell a live one from a leftover.
pub const LABEL: &str = "dev.fno.sandbox";
pub const LABEL_PID: &str = "dev.fno.sandbox.pid";

/// A host-minted sandbox id: `<spawn name, cleaned>-<8 hex>`. Only
/// `[a-z0-9-]`, so it is safe in a ref, a container name and a path.
pub fn mint_id(name: &str) -> String {
    let base: String = name
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "worker" } else { base };
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).ok();
    format!("{base}-{:08x}", u32::from_be_bytes(b))
}

/// Whether `id` is one [`mint_id`] could have made.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// Guest text for a host terminal: control characters escaped, length cut.
pub fn escape(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .take(max)
        .flat_map(char::escape_default)
        .collect();
    if text.chars().count() > max {
        out.push_str("...");
    }
    out
}
