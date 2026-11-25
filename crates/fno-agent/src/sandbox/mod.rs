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
//! - [`ops`]: the loop ops a sandboxed target worker asks the host for.
//! - [`github`]: guest text made inert before the host posts it.
//!
//! The relay stamps identity; the guest never does. The host never checks
//! out, extracts or executes sandbox-written content, and no failure falls
//! back to a host launch.

pub mod egress;
pub mod engine;
pub mod github;
pub mod guest;
pub mod ops;
pub mod relay;
pub mod repo;
pub mod transcript;
pub mod wire;

/// The sandbox a host Stop decision runs for. `hook stop --sandbox <id>`
/// sets it once, after the export record proves the id names a sandbox of
/// this worktree. Readers that would take host HEAD or the host tree read
/// the sandbox head and the export's dirty reading instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub id: String,
    /// `refs/sandbox/<id>/head`, or `None` when the worker has no commit.
    pub head: Option<String>,
    pub dirty: bool,
    /// The engine that runs it, from the export record.
    pub provider: String,
    pub host: Option<&'static str>,
}

static ORIGIN: std::sync::OnceLock<Origin> = std::sync::OnceLock::new();

/// The sandbox this process decides for, if any.
pub fn origin() -> Option<&'static Origin> {
    ORIGIN.get()
}

/// The sandbox whose export record names `cwd` as its clone source, if any.
pub fn clone_source_of(cwd: &std::path::Path) -> Option<String> {
    if let Some(o) = origin() {
        return Some(o.id.clone());
    }
    #[derive(serde::Deserialize)]
    struct Meta {
        id: String,
        cwd: std::path::PathBuf,
        #[serde(default)]
        node: Option<String>,
    }
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let want = canon(cwd);
    let dir = crate::paths::AgentsHome::from_env().sandbox_exports_dir();
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let meta: Meta =
            serde_json::from_str(&std::fs::read_to_string(e.path().join("meta.json")).ok()?)
                .ok()?;
        // Only a target spawn's bound worktree: a plain spawn's clone
        // source is the caller's own checkout, which keeps its rescue.
        (meta.node.is_some() && canon(&meta.cwd) == want).then_some(meta.id)
    })
}

/// Bind this process to sandbox `id` for worktree `cwd`. Refuses an id with
/// no export record, or one whose record names another worktree.
pub fn bind_origin(id: &str, cwd: &std::path::Path, dirty: bool) -> Res<&'static Origin> {
    if !valid_id(id) {
        return Err(Refusal(format!("{} is not a sandbox id", escape(id, 80))));
    }
    let home = crate::paths::AgentsHome::from_env();
    let meta = home.sandbox_exports_dir().join(id).join("meta.json");
    #[derive(serde::Deserialize)]
    struct Meta {
        cwd: std::path::PathBuf,
        provider: String,
        #[serde(default)]
        host: Option<String>,
    }
    let recorded: Meta = std::fs::read_to_string(&meta)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or_else(|| format!("sandbox {id} has no export record at {}", meta.display()))?;
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon(&recorded.cwd) != canon(cwd) {
        return Err(Refusal(format!(
            "sandbox {id} was cloned from {}, not {}",
            recorded.cwd.display(),
            cwd.display()
        )));
    }
    let refname = format!("refs/sandbox/{id}/head");
    let mut c = std::process::Command::new("git");
    c.arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "-q", &refname]);
    let head = crate::bounded_cmd::output_with_timeout_result(c, 60)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty());
    ORIGIN
        .set(Origin {
            id: id.to_string(),
            head,
            dirty,
            provider: recorded.provider,
            host: recorded.host.map(leak_host),
        })
        .ok();
    ORIGIN
        .get()
        .ok_or_else(|| "the sandbox origin did not bind".into())
}

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

/// A checked engine host, kept for the life of the process so [`Engine`]
/// stays `Copy`. One process binds one host.
///
/// [`Engine`]: engine::Engine
pub fn leak_host(host: String) -> &'static str {
    Box::leak(host.into_boxed_str())
}

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
