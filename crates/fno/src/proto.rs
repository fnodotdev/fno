//! Socket lifecycle for the fno mux, over the wire contract in [`fno_proto`].
//!
//! The wire types, framing and version constants live in the `fno-proto`
//! crate and are re-exported here, so every `crate::proto::` and
//! `fno::proto::` path keeps resolving. What stays in this file is what reads
//! fno's own state: the socket directory (`~/.fno/mux/<session>.sock`, mode
//! 0700, because the socket accepts keystrokes into your shell), the bind
//! that claims the name, and the short-lived startup marker that protects the
//! gap before the server can answer its liveness query. An unresponsive
//! established holder is terminated before its stale socket is unlinked.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use fno_proto::*;

/// The crate version, carried in the handshake purely for the error message.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version-handshake decision with this build as the server. The wire
/// crate holds the rule; fno supplies its own version for the message.
pub fn check_attach_version(client_proto: u32, client_build: &str) -> Result<(), String> {
    fno_proto::check_attach_version(client_proto, client_build, BUILD_VERSION)
}

// ---------------------------------------------------------------------------
// Socket lifecycle
// ---------------------------------------------------------------------------

#[cfg(test)]
struct TestMuxDir(PathBuf);

#[cfg(test)]
impl TestMuxDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fno-mux-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        crate::test_keeper_cleanup::reap(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

#[cfg(test)]
impl Drop for TestMuxDir {
    fn drop(&mut self) {
        crate::test_keeper_cleanup::reap(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
thread_local! {
    static TEST_MUX_DIR: TestMuxDir = TestMuxDir::new();
}

/// The mux socket directory: `$FNO_MUX_DIR` when set (tests point this at a
/// tempdir), else `<state_dir>/mux` off the EXPLICIT config tier
/// (`FNO_CONFIG` sole candidate, else the global config; the cwd-anchored
/// project tier is deliberately absent - see `resolved_state_root`), else
/// `~/.fno/mux`. `FNO_CONFIG` therefore isolates the mux with everything else
/// a demo config's state root gets its own socket dir, and a pinned
/// config that cannot be parsed lands the dir beside the pinned file rather
/// than reaching back to the real fleet's `~/.fno/mux`.
///
/// Resolved once per process (`MUX_DIR`): the daemon binds at startup and
/// one-shot clients ask once.
pub fn mux_dir() -> PathBuf {
    #[cfg(test)]
    return TEST_MUX_DIR.with(|dir| dir.0.clone());
    #[cfg(not(test))]
    return MUX_DIR.get_or_init(mux_dir_uncached).clone();
}

#[cfg(not(test))]
static MUX_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The uncached resolution behind [`mux_dir`].
#[cfg(not(test))]
fn mux_dir_uncached() -> PathBuf {
    if let Some(dir) = std::env::var_os("FNO_MUX_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    resolved_state_root().join("mux")
}

/// The state root the mux (and its non-socket sidecars, via
/// [`mux_sidecar_root`]) resolve under, latched once per process like
/// [`MUX_DIR`]: `<state_dir>` off the EXPLICIT config tier (`FNO_CONFIG` sole
/// candidate, else global; see `digest_overlay::config_explicit_top_str` for
/// why the project tier is deliberately absent).
///
/// Fail closed on an explicit `FNO_CONFIG` that yielded no state_dir at all
/// (yaml-pinned, unreadable, missing key): isolation was requested, so the
/// root stays the pinned file's own directory - standard layouts keep the
/// config inside the state dir. Falling back to `~/.fno` here is the exact
/// leak closes. Note Python's own loader defaults an absent state_dir
/// to `~/.fno/`, so a pinned config with no key leaves the mux MORE isolated
/// than the graph; that asymmetry is deliberate and the warning names it.
#[cfg(not(test))]
fn resolved_state_root() -> PathBuf {
    static STATE_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    STATE_ROOT
        .get_or_init(|| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match config_state_root() {
                Some(StateRoot::Root(root)) => root,
                // A found-but-declined value already said why, once; it lands
                // on the same fallback as a miss without a second,
                // contradictory message.
                Some(StateRoot::Declined) => fallback_state_root(&cwd, false),
                None => fallback_state_root(&cwd, true),
            }
        })
        .clone()
}

/// The root for mux sidecars that are not sockets (`mux-view.json`): the same
/// resolved state root, so a pinned `FNO_CONFIG` isolates them with the
/// sockets. `FNO_MUX_DIR` relocates the sockets alone by design and does not
/// move sidecars. The sidecars themselves resolve under `mux/` (see
/// [`mux_sidecar_path`]); this root stays the state root so the resolver can
/// see both spellings.
#[cfg(not(test))]
pub(crate) fn mux_sidecar_root() -> PathBuf {
    resolved_state_root()
}

/// The server-start move of the `owner = mux` rows: the mux server is their
/// long-lived writer and migrates them at its own start, before it opens
/// them. See [`crate::state_layout::migrate_mux_sidecars_at`]. Pub: the
/// binary's `run_server` calls it; the lib target sees no other caller.
#[cfg(not(test))]
pub fn migrate_mux_sidecars() {
    crate::state_layout::migrate_mux_sidecars_at(&resolved_state_root());
}

/// The resolved sidecar path: `mux/<file>` when it exists, else the gated
/// legacy root spelling when it exists, else `mux/<file>` (fresh install).
/// One seam for both stores; the explicit `legacy_sidecar` read stays as the
/// stores' last step for a file that appears between the two probes.
#[cfg(not(test))]
pub(crate) fn mux_sidecar_path(file: &str) -> PathBuf {
    crate::state_layout::resolve_sidecar(&resolved_state_root(), file, legacy_fallback_allowed())
}

/// The fallback root: a pinned `FNO_CONFIG`'s own directory when one is set
/// (anchored against cwd so a bare relative pin resolves to one dir per
/// invocation site), else the pre-config-chain global root. `warn` names the
/// pin once, for the miss case where nothing else has spoken.
#[cfg(not(test))]
fn fallback_state_root(cwd: &std::path::Path, warn: bool) -> PathBuf {
    if let Some(explicit) = std::env::var_os("FNO_CONFIG").filter(|d| !d.is_empty()) {
        let path = cwd.join(PathBuf::from(&explicit));
        if warn {
            warn_once_pinned_without_state_dir(&path);
        }
        return path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| cwd.to_path_buf());
    }
    legacy_state_root()
}

/// The pre-config-chain global state root, the ONE source for the resolver's
/// final fallback, `mux doctor`'s stranding check, and the sidecar root, so
/// they can never drift apart.
#[cfg(not(test))]
pub(crate) fn legacy_state_root() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fno")
}

/// The pre-config-chain global mux dir (`<legacy_state_root>/mux`), what
/// `mux doctor` compares against when it looks for stranded sessions.
#[cfg(not(test))]
pub(crate) fn legacy_mux_root() -> PathBuf {
    legacy_state_root().join("mux")
}

/// The pre-config-chain global agents home (`<legacy_state_root>/agents`):
/// where `mux doctor` looks for a historical squads store that no code
/// reads (see `mux_cli::agents_squads_orphan_check`).
#[cfg(not(test))]
pub(crate) fn legacy_agents_home() -> PathBuf {
    legacy_state_root().join("agents")
}

/// The pre-state-root sidecar path (squads.json, mux-view.json): a SIBLING of
/// the legacy mux dir. The view and squad stores share this spelling so their
/// fallback locations cannot drift apart.
#[cfg(not(test))]
pub(crate) fn legacy_sidecar_path(file: &str) -> PathBuf {
    legacy_mux_root().with_file_name(file)
}

/// The pre-state-root sidecar read behind the one gate (see
/// [`legacy_fallback_allowed`]). NotFound when absent or deliberately
/// overridden; every caller treats that as "no fallback".
#[cfg(not(test))]
pub(crate) fn legacy_sidecar(file: &str) -> std::io::Result<String> {
    if !legacy_fallback_allowed() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "legacy fallback disabled",
        ));
    }
    std::fs::read_to_string(legacy_sidecar_path(file))
}

/// What the explicit config tier said about `state_dir`.
#[cfg(not(test))]
enum StateRoot {
    /// The state root, resolved and expanded.
    Root(PathBuf),
    /// A value was found but this mirror cannot use it (`{vault}` templates,
    /// `$VAR` references, `~user` prefixes, or a RELATIVE path - see
    /// [`expand_state_dir`] for why relative anchors are refused). Python
    /// expands the first three and anchors the fourth; this mirror declines
    /// rather than fork the fleet across cwds, and has already warned once.
    Declined,
}

/// The state root from the explicit config tier. `None` means no candidate
/// carried the key at all; [`StateRoot::Declined`] means one carried a value
/// this mirror cannot use, and the caller must not re-report that as a
/// missing key.
#[cfg(not(test))]
fn config_state_root() -> Option<StateRoot> {
    let Some(raw) = crate::digest_overlay::config_explicit_top_str("state_dir") else {
        warn_once_legacy_yaml_state_dir();
        return None;
    };
    match expand_state_dir(&raw) {
        Some(root) => Some(StateRoot::Root(root)),
        None => {
            warn_once_unexpandable_state_dir(&raw);
            Some(StateRoot::Declined)
        }
    }
}

/// A state_dir visible only to Python (legacy global yaml) while the mux
/// resolves the default root. The divergence has no clean resolution here
/// (this crate is TOML-only by convention), so it warns like the other
/// decline cases instead of splitting silently.
#[cfg(not(test))]
pub(crate) fn warn_once_legacy_yaml_state_dir() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        if let Some(yaml) = legacy_global_yaml_state_dir_hint() {
            record_config_warning(
                format!(
                    "fno: the global {} mentions a state_dir this mux cannot read \
                     (TOML-only); every Python surface follows it while the mux \
                     stays on the default root.",
                    yaml.display()
                ),
                "move state_dir into the global config.toml, or unset it in the yaml".into(),
            );
        }
    });
}

/// Expand a leading `~` for an otherwise-absolute value. `None` when the
/// value is empty or RELATIVE: Python's own cross-project surfaces
/// (`paths.ledger_json`, `operator_lane`) follow `state_dir` only when it is
/// an absolute anchor, because a cwd-anchored root would fork shared
/// per-machine state into whichever checkout a process started in - and the
/// mux is the most cross-project surface there is. Also `None` for
/// `{template}` variables and `$VAR` references (Python's expandvars pass
/// runs first there; this mirror expands neither) and `~user` forms (Python
/// resolves them through the passwd database; `$HOME/user` would be a
/// different root).
pub(crate) fn expand_state_dir(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('{') || raw.contains('$') {
        return None;
    }
    if let Some(rest) = raw.strip_prefix('~') {
        if !rest.is_empty() && !rest.starts_with('/') {
            return None;
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        // ALL leading slashes, not one: a sloppy "~//x" must join under HOME,
        // and PathBuf::push of a still-absolute rest would REPLACE the home
        // buffer outright (std semantics), landing the mux on /x.
        let rest = rest.trim_start_matches('/');
        let mut p = PathBuf::from(home);
        if !rest.is_empty() {
            p.push(rest);
        }
        return Some(p);
    }
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

/// A pinned `$FNO_CONFIG` that yielded no usable state_dir (a yaml-pinned
/// path Python parses by suffix but this reader cannot, an unreadable file,
/// or a valid TOML with no top-level state_dir). The socket dir has already
/// fallen back beside the file; say so once instead of diverging in silence
/// (same shape as `fno_agents::agents_config::warn_once_if_yaml`).
#[cfg(not(test))]
fn warn_once_pinned_without_state_dir(path: &std::path::Path) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        let yaml = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        );
        let why = if yaml {
            "which the mux reads as TOML and cannot parse"
        } else {
            "which carries no usable top-level state_dir"
        };
        record_config_warning(
            format!(
                "fno: $FNO_CONFIG points at {}, {why}; the mux dir lands beside that \
                 file instead of your state root.",
                path.display()
            ),
            "point $FNO_CONFIG at a config.toml with a usable state_dir".into(),
        );
    });
}

/// A configured state_dir this mirror cannot expand was DECLINED, so the mux
/// root falls back (beside a pinned config, or to the global default) while
/// every Python surface expands it and moves elsewhere. Say so once instead
/// of leaving the split silent.
#[cfg(not(test))]
pub(crate) fn warn_once_unexpandable_state_dir(raw: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        record_config_warning(
            format!(
                "fno: config state_dir {raw:?} is not a value the mux can follow (it \
                 must be an absolute or ~ path, free of template variables, $VAR \
                 references, and ~user forms); the mux dir falls back instead of \
                 following it."
            ),
            "set state_dir to an absolute or ~ path".into(),
        );
    });
}

/// The legacy global settings.yaml sibling, when it exists and names a
/// state_dir VALUE that diverges from the root this resolver would use.
/// Python keeps BOTH global files as read candidates (`_prefer_toml`:
/// config.toml wins per key, the yaml still loads), so a state_dir that
/// lives only in the yaml moves every Python surface while this reader -
/// and the mux - stay on the default root. A yaml whose value AGREES (the
/// explicit pre-migration spelling of the default root) is not a
/// divergence and never warns.
#[cfg(not(test))]
pub(crate) fn legacy_global_yaml_state_dir_hint() -> Option<std::path::PathBuf> {
    if non_empty_env_is_set("FNO_CONFIG") {
        return None;
    }
    let yaml = crate::digest_overlay::global_settings_yaml_sibling()?;
    let body = std::fs::read_to_string(&yaml).ok()?;
    // The top-level VALUE, not a substring: a trailing `# comment` is not
    // part of the value yaml hands Python, and a nested mapping's key is not
    // the global one. The one nested shape that IS canonical is the legacy
    // `config:` wrapper Python unwraps, so a state_dir directly under a
    // top-level `config:` key counts. Any other misread would warn on every
    // command forever over no divergence.
    let mut under_config = false;
    let value = body.lines().find_map(|l| {
        let indented = l.starts_with([' ', '\t']);
        let (key, val) = l.split_once(':')?;
        let key = key.trim();
        if !indented {
            if key.eq_ignore_ascii_case("config") {
                // The flow spelling `config: {state_dir: /x}` carries the
                // wrapped key inline; Python unwraps it like the block form.
                if let Some(inner) = extract_flow_state_dir(val) {
                    return Some(inner);
                }
                under_config = val.trim().is_empty();
                return None;
            }
            return key.eq_ignore_ascii_case("state_dir").then_some(val);
        }
        (under_config && key.eq_ignore_ascii_case("state_dir")).then_some(val)
    })?;
    let value = value
        .split(" #")
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches(|c| c == '\'' || c == '"');
    if value.is_empty() {
        return None;
    }
    match expand_state_dir(value) {
        // Agreement with the root this resolver would use anyway.
        Some(root) => (root != legacy_state_root()).then_some(yaml),
        None => {
            // A `$HOME`/`${HOME}` spelling is unexpandable by this mirror
            // but Python's expandvars resolves it; compare it rather than
            // warn when it names the same root. Any other unexpandable
            // value (relative, template) leaves the warning standing:
            // Python still follows it somewhere this mirror cannot look.
            let rest = value
                .strip_prefix("${HOME}")
                .or_else(|| value.strip_prefix("$HOME"));
            match (rest, std::env::var_os("HOME").map(PathBuf::from)) {
                (Some(rest), Some(home)) => {
                    let joined = home.join(rest.trim_start_matches('/'));
                    (joined != legacy_state_root()).then_some(yaml)
                }
                _ => Some(yaml),
            }
        }
    }
}

/// The `state_dir` value inside a flow mapping value (`{state_dir: /x}`),
/// raw and untrimmed: the caller applies the same comment-strip and quote
/// handling as the block form. None when the braces carry no such key.
#[cfg(not(test))]
fn extract_flow_state_dir(flow: &str) -> Option<&str> {
    let inner = flow.trim().strip_prefix('{')?.strip_suffix('}')?;
    inner.split(',').find_map(|pair| {
        let (k, v) = pair.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("state_dir")
            .then(|| v.trim())
            .filter(|v| !v.is_empty())
    })
}

fn non_empty_env_is_set(key: &str) -> bool {
    crate::digest_overlay::non_empty_env_os(key).is_some()
}

/// Whether a legacy-location read fallback is allowed at all: only under
/// fully AMBIENT state resolution, where no explicit override names where
/// state lives. A pinned `FNO_CONFIG` (isolation requested) or a set
/// `FNO_AGENTS_HOME` (an explicit agents-state override) both mean the
/// operator pointed state elsewhere on purpose, so no fallback may reach the
/// real root. This is the ONE spelling of the gate - hand-rolled variants in
/// three files once drifted apart (head-review round eight), and one flipped
/// gate would leak the operator's real prefs into an isolated env.
pub fn legacy_fallback_allowed() -> bool {
    !non_empty_env_is_set("FNO_CONFIG") && !non_empty_env_is_set("FNO_AGENTS_HOME")
}

/// The one config-resolution warning this process recorded, if any: a pinned
/// `FNO_CONFIG` with no usable state_dir, a state_dir value this mirror had
/// to decline, or a state_dir that lives only where this reader cannot look.
/// Carries its own remedy so `mux doctor` never names the wrong fix. Emitted
/// to stderr by the non-TUI commands (`mux ls`) and as a `mux doctor` row; the
/// TUI client appends it to its client log instead. proto itself never
/// writes stderr: resolution runs before the client enters the alternate
/// screen, and any pre-TUI stderr byte lands in the PTY the harness reads
/// (client.rs, NEVER-stderr rule).
static CONFIG_WARNING: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

// Every caller sits behind cfg(not(test)), so the test leg sees no call.
#[cfg_attr(test, allow(dead_code))]
fn record_config_warning(msg: String, remedy: String) {
    let _ = CONFIG_WARNING.set((msg, remedy));
}

pub fn pending_config_warning() -> Option<(&'static str, &'static str)> {
    CONFIG_WARNING.get().map(|(m, r)| (m.as_str(), r.as_str()))
}

/// Create `dir` (and parents) born 0700, then force 0700 on a pre-existing
/// one. `DirBuilder::mode` makes fresh directories private atomically -
/// `create_dir_all` + `set_permissions` leaves a window where the dir exists
/// with umask-loosened permissions (gemini security-medium). The follow-up
/// `set_permissions` only tightens a dir that already existed.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Create the mux dir if needed and force 0700 either way: the sockets in it
/// accept keystrokes into your shell, so group/world access is never OK.
pub fn ensure_mux_dir() -> std::io::Result<PathBuf> {
    let dir = mux_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Socket path for a session name. Path separators are rejected rather than
/// sanitized so a session name can never escape the 0700 mux dir.
pub fn socket_path(session: &str) -> Result<PathBuf, String> {
    if session.is_empty() || session.contains('/') || session.contains('\0') {
        return Err(format!("invalid session name: {session:?}"));
    }
    Ok(mux_dir().join(format!("{session}.sock")))
}

/// The wire-version sidecar for a session socket (`<name>.sock` -> `<name>.ver`),
/// written by the server at startup with its [`PROTO_VERSION`]. It lets
/// `fno mux ls` tell a below-floor server from one whose wire is compatible - a
/// distinction the FROZEN, version-agnostic `Query`/`Info` probe cannot make.
/// A `.ver` file is skipped by [`session_names`]'s `.sock` filter, so it never
/// reads as a session.
pub fn version_sidecar_path(socket: &Path) -> PathBuf {
    socket.with_extension("ver")
}

/// The pid sidecar for a session socket (`<name>.sock` -> `<name>.pid`),
/// written by the server at bind with its own pid so `kill-server`
/// can signal a wedged holder without an accepted connection - the
/// connected-peer route cannot serve a connect that never completes. Skipped
/// by [`session_names`]'s `.sock` filter, the same reason `.ver` is. Content is
/// `"<pid>:<start_time>"` (see [`pid_start_time`]) when the platform can supply
/// a start time, else bare `"<pid>"`.
pub fn pid_sidecar_path(socket: &Path) -> PathBuf {
    socket.with_extension("pid")
}

/// Process start time in this OS's own units: a per-host, per-boot quantity
/// meaningful only compared for equality against a value captured for the
/// SAME pid, never converted to wall-clock time. One delegate to the pid
/// module, which owns the per-platform bodies the three old arms carried.
pub fn pid_start_time(pid: u32) -> Option<u64> {
    Pid::new(pid).and_then(|p| p.start_time())
}

/// Write the pid sidecar beside a freshly bound socket: the server's own pid
/// with its start time when this platform can supply one, else a bare pid
/// (the two shapes `parse_pid_sidecar` reads). The write/read pair lives here
/// so writer and parser cannot drift on the format.
pub fn write_pid_sidecar(socket: &Path) -> std::io::Result<()> {
    let own_pid = std::process::id();
    let contents = match pid_start_time(own_pid) {
        Some(start) => format!("{own_pid}:{start}"),
        None => own_pid.to_string(),
    };
    std::fs::write(pid_sidecar_path(socket), contents)
}

/// Parse a `.pid` sidecar's contents: `"<pid>:<start_time>"` (the
/// identity-checked format) or bare `"<pid>"` (a platform `pid_start_time`
/// cannot supply one for). Tolerant of trailing whitespace/newline. A `:` is
/// present only when the writer meant to supply a start time, so a present
/// but unparseable one (a write torn mid-`fs::write`) fails the whole parse
/// rather than silently degrading to the unverified bare-pid shape - a torn
/// write must read as corrupt, not as a platform that never verifies.
pub fn parse_pid_sidecar(s: &str) -> Option<(i32, Option<u64>)> {
    let s = s.trim();
    match s.split_once(':') {
        Some((p, t)) => Some((p.parse().ok()?, Some(t.parse().ok()?))),
        None => Some((s.parse().ok()?, None)),
    }
}

/// The zombie read; extracted beside its siblings for the file budget.
mod pid_state;
pub use pid_state::pid_is_zombie;

/// True only when `kill(pid, 0)` proves `pid` is dead: ESRCH is the sole
/// unambiguous signal. Any other outcome - alive, or an error like EPERM
/// that cannot distinguish "alive but not ours to signal" from "gone" -
/// reads as not-provably-dead. The single implementation for a read that
/// three call sites (kill-server's pre-check, its poll loop, and the
/// server's own respawn-vs-alive check) each duplicated inline before
///, which is exactly the shape that lets one of them drift. A reachable
/// ZOMBIE also reads as dead: it is unreaped (its parent may be slow, or a
/// bare-init container never reaps at all), holds no fds, and serves
/// nothing, so waiting out ESRCH would never converge.
pub fn pid_confirmed_dead(pid: i32) -> bool {
    let Some(pid) = Pid::try_from(i64::from(pid)).ok() else {
        return false;
    };
    if !matches!(pid.signal(0), SignalResult::Sent) {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    pid.zombie()
}

mod owner_sidecar;
pub use owner_sidecar::{owner_sidecar_path, write_owner_sidecar};
mod session_files;
pub use session_files::{remove_session_files, session_files, visible_sidecar_path};

// The startup-marker family lives in the child module below, named by the
// question it answers; the file-budget gate keeps this over-budget file
// shrink-only.
mod startup_guard;
use startup_guard::{claim_startup, StartupClaim};
pub use startup_guard::{remove_startup_guard, startup_sidecar_path};

pub const DEFAULT_SESSION: &str = "main";
use crate::pid::{Pid, SignalResult};

/// Outcome of [`bind_or_probe`].
pub enum BindOutcome {
    /// We own the socket: this process should run the server.
    Bound(UnixListener),
    /// A live server already owns it: attach instead.
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    Alive,
    Dead,
    Unknown,
}

const PROBE_HOLDER_TERM_GRACE: Duration = Duration::from_secs(3);
const PROBE_HOLDER_KILL_GRACE: Duration = Duration::from_secs(1);

/// Claim the session socket through the startup handshake, treating the bind
/// itself as the lock.
///
/// - Fresh path: the claim owns the marker and the bind wins atomically.
/// - A live starter holds the marker: the claim WAITS it out - the winner
///   answers (`AlreadyRunning`), or its marker clears or dies and the claim
///   is retried - so no starter ever binds over a peer mid-startup, and a
///   marker owner can never lose its bind to one. A wedged startup refuses
///   at the claim's deadline.
/// - `AddrInUse` once we hold the claim: query-probe. A `ServerMsg::Info`
///   response means a live server (`AlreadyRunning`); anything else means a
///   dead or legacy holder, which is terminated or proven gone before the
///   path is rebound.
///
/// ponytail: unlink-then-rebind keeps a tiny two-racers-over-a-stale-socket
/// window (both probe dead, both unlink+bind). If it ever bites, the upgrade
/// is an O_EXCL sidecar lock around the unlink.
pub fn bind_or_probe(path: &Path) -> std::io::Result<BindOutcome> {
    match claim_startup(path)? {
        StartupClaim::Running => return Ok(BindOutcome::AlreadyRunning),
        StartupClaim::Own => {}
    }
    match UnixListener::bind(path) {
        Ok(l) => Ok(BindOutcome::Bound(l)),
        Err(e) if socket_in_use(&e) => match probe_status(path) {
            ProbeOutcome::Alive => {
                remove_startup_guard(path);
                Ok(BindOutcome::AlreadyRunning)
            }
            ProbeOutcome::Unknown | ProbeOutcome::Dead if pid_sidecar_path(path).exists() => {
                reclaim_unresponsive_holder(path)?;
                rebind_after_release(path)
            }
            // No holder identity: the legacy stale-socket recovery. The claim
            // we hold says no startup is in flight, so nothing live claimed
            // the path between our probes.
            ProbeOutcome::Unknown | ProbeOutcome::Dead => rebind_after_release(path),
        },
        Err(e) => {
            remove_startup_guard(path);
            Err(e)
        }
    }
}

/// Take the path from a holder that is dead or has been terminated. Session
/// files, not just the socket, so a dead holder's .pid cannot outlive the
/// rebind and name an unrelated pid. Losing the rebind means a peer won it;
/// that peer is the server. Not Bound means the marker we claimed must not
/// outlive the claim: a starter after us would wait out its dead pid.
fn rebind_after_release(path: &Path) -> std::io::Result<BindOutcome> {
    remove_session_files(path)?;
    match UnixListener::bind(path) {
        Ok(l) => Ok(BindOutcome::Bound(l)),
        Err(e) if socket_in_use(&e) => {
            remove_startup_guard(path);
            Ok(BindOutcome::AlreadyRunning)
        }
        Err(e) => {
            remove_startup_guard(path);
            Err(e)
        }
    }
}

/// Bind failed because the path is taken. Linux reports `EADDRINUSE`
/// (`AddrInUse`); macOS reports `EEXIST` (`AlreadyExists`) when the socket
/// file already exists. Both mean the same thing here.
fn socket_in_use(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
    )
}

/// Query timeout for liveness probes at bind time. Generous next to a socket
/// round-trip, but bounded so a markerless predecessor never blocks startup.
const PROBE_ALIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Connect to an AF_UNIX SOCK_STREAM path with a bounded timeout. std's
/// `UnixStream::connect` has no connect-timeout knob, so a live-but-wedged
/// listener (dead accept loop, full backlog) blocks it indefinitely - the
/// `fno agents restart --mux` hang. Nonblocking connect + `poll` for writability,
/// then blocking mode restored so the caller's read/write timeouts apply.
///
/// The `ErrorKind::TimedOut` this returns means "wedged server" and callers
/// match on it to keep a wedged server out of the "stale/dead" bucket. That
/// signal is only sound for the CONNECT: a post-connect read/write timeout
/// (a server that accepted then stopped answering) is a different state and
/// must NOT be kind-matched against `TimedOut` here - callers treat those
/// with a bare `Err(_)` arm instead.
pub fn connect_unix_timeout(path: &Path, timeout: Duration) -> std::io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "socket path contains NUL")
    })?;
    // SAFETY: a standard libc socket/connect/poll sequence. The fd is closed on
    // every error path and wrapped into a UnixStream (which owns + closes it)
    // on success.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Close-on-exec: std's UnixStream::connect sets it, so replacing that
        // call must too - otherwise a raw fd leaks into every child the mux
        // spawns (PTYs, the digest shell-out). fcntl(F_SETFD) is the portable
        // path (macOS has no SOCK_CLOEXEC socket-type flag); the tiny
        // create->set window is acceptable here (no fork+exec races on it).
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = c_path.as_bytes();
        if bytes.len() >= std::mem::size_of_val(&addr.sun_path) {
            libc::close(fd);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket path too long",
            ));
        }
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            bytes.len(),
        );
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let rc = libc::connect(fd, &addr as *const _ as *const libc::sockaddr, addr_len);
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            // AF_UNIX with a FULL accept backlog: Linux reports EAGAIN from a
            // nonblocking connect (macOS reports EINPROGRESS and the poll
            // below times out). Same wedged-server meaning - normalize to
            // TimedOut so every caller sees one signal on both platforms.
            if err.raw_os_error() == Some(libc::EAGAIN) {
                libc::close(fd);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out (accept backlog full)",
                ));
            }
            // A connect interrupted by a signal (EINTR) is NOT a failure: POSIX
            // says the connection continues asynchronously, exactly like
            // EINPROGRESS, so poll for its completion rather than reporting the
            // signal as a dead server (which the bind path would unlink).
            if err.raw_os_error() != Some(libc::EINPROGRESS)
                && err.raw_os_error() != Some(libc::EINTR)
            {
                libc::close(fd);
                return Err(err);
            }
            // Poll against a deadline, retrying on EINTR with the remaining
            // budget so a signal storm can neither abort the connect early nor
            // let it outlast `timeout`.
            let deadline = std::time::Instant::now() + timeout;
            let writable = loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let pr = libc::poll(&mut pfd, 1, ms);
                if pr < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EINTR) {
                        continue; // interrupted: re-poll with recomputed remaining
                    }
                    libc::close(fd);
                    return Err(e);
                }
                break pr;
            };
            if writable == 0 {
                libc::close(fd);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                ));
            }
            let mut soerr: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            if libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut soerr as *mut _ as *mut libc::c_void,
                &mut len,
            ) < 0
            {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
            if soerr != 0 {
                libc::close(fd);
                return Err(std::io::Error::from_raw_os_error(soerr));
            }
        }
        // Restore blocking mode for the subsequent read/write timeouts.
        if libc::fcntl(fd, libc::F_SETFL, flags) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(UnixStream::from_raw_fd(fd))
    }
}

/// Probe the frozen pre-Attach query. A bare connect is not enough: a stale
/// socket can still accept one residual connection after its listener dies. A
/// timeout or a connected peer without `Info` is unknown, not proof of death.
pub(crate) fn probe_status(path: &Path) -> ProbeOutcome {
    let mut uncertain = false;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut stream = match connect_unix_timeout(path, PROBE_ALIVE_CONNECT_TIMEOUT) {
            Ok(stream) => stream,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                continue;
            }
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        uncertain = true;
        let _ = stream.set_read_timeout(Some(PROBE_ALIVE_CONNECT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(PROBE_ALIVE_CONNECT_TIMEOUT));
        if write_msg_sync(&mut stream, &ClientMsg::Query).is_err() {
            continue;
        }
        if matches!(
            read_msg_sync::<_, ServerMsg>(&mut stream),
            Ok(ServerMsg::Info { .. })
        ) {
            return ProbeOutcome::Alive;
        }
    }
    if uncertain {
        ProbeOutcome::Unknown
    } else {
        ProbeOutcome::Dead
    }
}

/// Stop an unresponsive holder before takeover. Without this coordination, its
/// later `SocketGuard` can unlink the replacement server's socket and sidecars.
/// The pid sidecar is identity-checked before each signal. An unreadable
/// identity refuses takeover, while a positive start-time mismatch proves the
/// recorded holder exited and its pid was reused, so takeover is safe.
/// A sidecar that vanished between the caller's exists() gate and this read
/// means the holder exited in that gap: there is nothing left to terminate,
/// so Ok sends the caller down the no-holder takeover path.
fn reclaim_unresponsive_holder(path: &Path) -> std::io::Result<()> {
    let pid_path = pid_sidecar_path(path);
    let raw = match std::fs::read_to_string(&pid_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "cannot take over unresponsive socket: read {}: {e}",
                    pid_path.display()
                ),
            ));
        }
    };
    let (pid, recorded_start) = parse_pid_sidecar(&raw).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cannot take over unresponsive socket: invalid {}",
                pid_path.display()
            ),
        )
    })?;
    // The pid type refuses 0, 1 and anything a pid_t cannot hold, so the
    // old pid <= 1 refusal and the self-pid refusal below are one parse.
    if pid as u32 == std::process::id() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("cannot signal holder pid {pid} from {}", pid_path.display()),
        ));
    }
    let pid = Pid::try_from(i64::from(pid)).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("cannot signal holder pid from {}", pid_path.display()),
        )
    })?;

    let identity_matches = || -> std::io::Result<bool> {
        match recorded_start {
            None => Ok(true),
            Some(recorded) => pid
                .start_time()
                .map(|observed| observed == recorded)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("cannot verify holder pid from {}", pid_path.display()),
                    )
                }),
        }
    };
    let gone = || pid_confirmed_dead(pid.as_raw()) || pid_is_zombie(pid.as_raw());
    if gone() {
        return Ok(());
    }
    if !identity_matches()? {
        return Ok(());
    }
    if let SignalResult::Failed(e) = pid.signal(libc::SIGTERM) {
        return Err(e);
    }
    let term_deadline = Instant::now() + PROBE_HOLDER_TERM_GRACE;
    while Instant::now() < term_deadline {
        if gone() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if gone() {
        return Ok(());
    }
    if !identity_matches()? {
        // The recorded process exited and its pid was reused. That is safe for
        // takeover: the old SocketGuard cannot run in the replacement process.
        return Ok(());
    }
    if let SignalResult::Failed(e) = pid.signal(libc::SIGKILL) {
        return Err(e);
    }
    let kill_deadline = Instant::now() + PROBE_HOLDER_KILL_GRACE;
    while Instant::now() < kill_deadline {
        if gone() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if gone() {
        Ok(())
    } else if !identity_matches()? {
        // As above, a changed start time proves the holder we needed to stop is
        // gone, even if the OS has already reused its pid.
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("holder pid {pid} survived takeover signals"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_rows() {
        assert!(socket_path("../evil").is_err());
        assert!(socket_path("").is_err());
        assert!(socket_path("ok-name_1").is_ok());

        assert_eq!(
            expand_state_dir("/demo/state"),
            Some(PathBuf::from("/demo/state"))
        );
        assert_eq!(
            expand_state_dir("  /demo/state  "),
            Some(PathBuf::from("/demo/state"))
        );
        // `~` expands against HOME like Python's expanduser; trailing slashes
        // and a bare `~` both land on HOME itself.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home {
            assert_eq!(expand_state_dir("~/.fno/"), Some(home.join(".fno")));
            assert_eq!(expand_state_dir("~"), Some(home.clone()));
            // A sloppy double slash joins under HOME; push() of an absolute
            // rest would otherwise REPLACE the home buffer.
            assert_eq!(expand_state_dir("~//fno-demo"), Some(home.join("fno-demo")));
        }
        // Relative values decline, matching Python's own cross-project
        // surfaces (ledger, operator lane): a cwd-anchored root would fork
        // the shared fleet into whichever checkout a process started in.
        assert_eq!(expand_state_dir("rel/state"), None);
        assert_eq!(expand_state_dir("state"), None);
        // Template variables, $VAR references, and ~user forms decline too
        // (Python resolves ~user through the passwd database, not
        // $HOME/user).
        assert_eq!(expand_state_dir("{vault}/.fno"), None);
        assert_eq!(expand_state_dir("$HOME/demo"), None);
        assert_eq!(expand_state_dir("${HOME}/demo"), None);
        assert_eq!(expand_state_dir("~demo/.fno"), None);
        assert_eq!(expand_state_dir(""), None);
        assert_eq!(expand_state_dir("   "), None);
    }

    // The zombie-read family: an unreaped exit reads as dead on both
    // helpers pinned here, so the two-call `|| pid_is_zombie` dance at the
    // callers never needs repeating.
    #[path = "pid_zombie_tests.rs"]
    mod pid_zombie_tests;
}
