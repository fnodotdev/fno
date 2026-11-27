//! The sandbox launch seam on the spawn side: the `[sandbox]` policy, the plan
//! the daemon answers (`agent.sandbox-plan`), and the wrap that hands a
//! one-shot lane's command to `fno-agent hook sandbox-run`.
//!
//! Ruling Q7 orders the harms: shared state, then host, then resource
//! fights. A sandboxed worker gets no mount at all: no worktree bind, no
//! daemon socket, no agents home. It reaches the host only through the relay
//! that owns its exec stream, which stamps identity and forwards typed
//! frames. The host client keeps every gate and every claim. A spawn that
//! carries `FNO_NODE` is a target worker: the relay binds that node's
//! worktree, branch, claim and plan on the host before the clone.

use crate::sandbox::{Refusal, Res};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::sandbox::engine::Provider;

/// The daemon's command for the launch plan.
pub const SANDBOX_PLAN_COMMAND: &str = "sandbox-plan";

/// The one-shot lanes that call [`sandboxed`]. Any other harness or substrate
/// would launch on the host while the config says it is sandboxed, so the
/// spawn refuses it.
pub const SANDBOXED_HARNESSES: [&str; 5] = ["claude", "codex", "gemini", "opencode", "agy"];

/// What one sandboxed spawn runs: the provider, the image (`None` is the
/// embedded recipe), and the env pins the inside reads. No mounts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPlan {
    pub provider: String,
    pub image: Option<String>,
    /// `sandbox.host`: the user's own VM, reached by ssh. Global only.
    #[serde(default)]
    pub host: Option<String>,
    pub env: BTreeMap<String, String>,
}

/// One config layer's `[sandbox]` values: provider, image and host.
type Layer = (Option<String>, Option<String>, Option<String>);

/// One config layer's `[sandbox]` values, or the reason it refuses.
fn layer(path: &Path) -> Res<Option<Layer>> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let table = crate::agents_config::parse_config(&body).ok_or_else(|| {
        format!(
            "{} does not parse; the sandbox policy refuses to guess",
            path.display()
        )
    })?;
    let Some(sb) = table.get("sandbox") else {
        return Ok(Some((None, None, None)));
    };
    let sb = sb
        .as_table()
        .ok_or_else(|| format!("{}: [sandbox] is not a table", path.display()))?;
    let text = |key: &str| -> Res<Option<String>> {
        match sb.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.trim().to_string()))
                .ok_or_else(|| {
                    Refusal(format!(
                        "{}: sandbox.{key} must be a string",
                        path.display()
                    ))
                }),
        }
    };
    let provider = text("provider")?;
    if let Some(p) = &provider {
        if Provider::parse(p).is_none() {
            return Err(Refusal(format!(
                "{}: sandbox.provider = {p:?} is not one of {}",
                path.display(),
                Provider::VALUES.join(", ")
            )));
        }
    }
    let host = text("host")?.filter(|h| !h.is_empty());
    if let Some(h) = &host {
        crate::sandbox::engine::SshHost::parse(h)
            .map_err(|e| Refusal(format!("{}: {e}", path.display())))?;
    }
    Ok(Some((provider, text("image")?, host)))
}

/// The resolved policy and the notices it owes the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub provider: Provider,
    pub image: Option<String>,
    pub host: Option<String>,
    pub notices: Vec<String>,
}

/// Resolve `[sandbox]` across the layers. Any layer can raise `none` to a
/// sandbox. A project layer cannot lower or switch a provider the global
/// config sets. `image` reads from global config only. An unparsable layer or
/// an unknown value refuses.
pub fn policy(cwd: &Path) -> Res<Policy> {
    let candidates = crate::agents_config::config_candidates(cwd);
    let global = crate::agents_config::global_config_path();
    let pinned = std::env::var_os("FNO_CONFIG").is_some_and(|v| !v.is_empty());
    let mut notices = Vec::new();
    let (mut g_provider, mut g_image, mut g_host) = (None, None, None);
    let mut projects: Vec<(PathBuf, Option<String>, Option<String>)> = Vec::new();
    for path in &candidates {
        let Some((provider, image, host)) = layer(path)? else {
            continue;
        };
        // A pinned FNO_CONFIG is the one layer, and reads as global.
        if Some(path) == global.as_ref() || pinned {
            (g_provider, g_image, g_host) = (provider, image, host);
        } else {
            if host.is_some() {
                notices.push(format!(
                    "{} sets sandbox.host; the host is read from global config only, so it is ignored",
                    path.display()
                ));
            }
            projects.push((path.clone(), provider, image));
        }
    }
    let global_provider = g_provider
        .as_deref()
        .and_then(Provider::parse)
        .unwrap_or(Provider::None);
    let mut provider = global_provider;
    for (path, p, image) in &projects {
        if image.is_some() {
            notices.push(format!(
                "{} sets sandbox.image; the image is read from global config only, so it is ignored",
                path.display()
            ));
        }
        let Some(p) = p.as_deref().and_then(Provider::parse) else {
            continue;
        };
        if provider == Provider::None {
            provider = p;
        } else if p != provider {
            notices.push(format!(
                "{} sets sandbox.provider = \"{}\"; a project layer cannot lower or switch \
                 the provider, so \"{}\" stays",
                path.display(),
                p.as_str(),
                provider.as_str()
            ));
        }
    }
    Ok(Policy {
        provider,
        image: g_image.filter(|i| !i.is_empty()),
        host: g_host,
        notices,
    })
}

/// The plan the daemon answers for one worktree.
pub fn plan_for(cwd: &Path) -> Res<SandboxPlan> {
    let p = policy(cwd)?;
    let mut env = BTreeMap::new();
    if p.provider != Provider::None {
        env.insert(
            "FNO_SUPERVISOR_SOCKET".to_string(),
            crate::sandbox::CONTAINER_SOCK.to_string(),
        );
    }
    Ok(SandboxPlan {
        provider: p.provider.as_str().to_string(),
        image: p.image,
        host: p.host,
        env,
    })
}

/// The spawn's first sandbox leg, before the guard and the gate: does this
/// spawn run in a sandbox? `Ok(false)` under `none`. An exit code and a reason
/// for a spawn the sandbox cannot carry, so nothing launches on the host while
/// the config says sandboxed. A caller inside a sandbox has a pinned socket and
/// refuses: a sandboxed worker never spawns a peer.
pub fn check(cwd: &Path, harness: &str, substrate: &str) -> Result<bool, (i32, String)> {
    if std::env::var_os("FNO_AGENT_SANDBOXED").is_some() {
        return Err((
            2,
            "a sandboxed worker cannot spawn a peer; spawn from the host".to_string(),
        ));
    }
    let p = policy(cwd).map_err(|e| (2, format!("[sandbox] refused: {e}")))?;
    for n in &p.notices {
        eprintln!("fno-agent spawn: [sandbox] notice: {n}");
    }
    match p.provider {
        Provider::None => Ok(false),
        provider => {
            provider
                .engine()
                .map_err(|e| (2, format!("[sandbox] refused: {e}")))?;
            if substrate == "headless" && SANDBOXED_HARNESSES.contains(&harness) {
                Ok(true)
            } else {
                Err((
                    2,
                    format!(
                        "[sandbox] provider = \"{}\" sandboxes only --as headless spawns of {}; \
                         a {harness} spawn on the {substrate} substrate would run on the host. \
                         Spawn with --as headless, or set [sandbox] provider = \"none\".",
                        provider.as_str(),
                        SANDBOXED_HARNESSES.join(", ")
                    ),
                ))
            }
        }
    }
}

static ACTIVE: OnceLock<(SandboxPlan, PathBuf)> = OnceLock::new();

/// The spawn's second sandbox leg, after the gate: ask the daemon for the
/// plan and arm [`sandboxed`]. The daemon reads the policy with its own env;
/// a different answer refuses rather than launch under the wrong one.
pub fn arm(home: &crate::paths::AgentsHome, cwd: &Path) -> Result<(), (i32, String)> {
    let sock = crate::client::client_sock(home);
    let ask = || {
        crate::protocol::blocking_response(
            &sock,
            &format!("agent.{SANDBOX_PLAN_COMMAND}"),
            serde_json::json!({ "worktree": cwd.to_string_lossy() }),
            std::time::Duration::from_secs(5),
        )
    };
    let reply = ask()
        .or_else(|_| {
            start_daemon(home)?;
            ask()
        })
        .map_err(|e| {
            (
                13,
                format!(
                    "no daemon answered at {} ({e}), and starting one failed",
                    sock.display()
                ),
            )
        })?;
    let plan: SandboxPlan = match reply.payload {
        crate::protocol::ResponsePayload::Ok(v) => serde_json::from_value(v)
            .map_err(|e| (13, format!("the daemon's sandbox plan does not parse: {e}")))?,
        crate::protocol::ResponsePayload::Err(e) => {
            return Err((
                2,
                format!("the daemon refused the sandbox plan: {}", e.message),
            ))
        }
    };
    let local = policy(cwd).map_err(|e| (2, e.0))?;
    if plan.provider != local.provider.as_str() {
        return Err((
            2,
            format!(
                "the daemon reads [sandbox] provider = \"{}\" for {}, the spawn reads \"{}\"; \
                 restart the daemon under the same config",
                plan.provider,
                cwd.display(),
                local.provider.as_str()
            ),
        ));
    }
    ACTIVE.set((plan, cwd.to_path_buf())).ok();
    Ok(())
}

/// Start the daemon the way every other client command does. The spawn path
/// is sync inside the client's runtime, so the async start runs on a scoped
/// thread with its own runtime.
fn start_daemon(home: &crate::paths::AgentsHome) -> std::io::Result<()> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(crate::client::ensure_daemon(
                home,
                &crate::client::resolve_daemon_bin(),
            ))
            .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the daemon start thread panicked")))
    })
}

/// Where the harness writes its transcript inside, for the harnesses whose
/// file layout fno reads. The other harnesses keep their host stdout tee.
fn transcript_dir(harness: &str) -> Option<String> {
    match harness {
        "claude" => Some("/home/fno/.claude/projects".into()),
        "codex" => Some("/home/fno/.codex/sessions".into()),
        _ => None,
    }
}

/// Re-home a lane's composed command inside the sandbox. Call it after the
/// lane sets its env and before it sets stdio or `pre_exec`: those do not carry
/// over to the new command. Unarmed, the command comes back untouched.
///
/// The new command is `fno-agent hook sandbox-run`. Its argv carries env key
/// names only; the values stay on this command's env, where the wrapper reads
/// them and sends them to the guest over exec stdin.
pub fn sandboxed(cmd: Command) -> Command {
    let Some((plan, worktree)) = ACTIVE.get() else {
        return cmd;
    };
    let mut argv: Vec<String> = vec![cmd.get_program().to_string_lossy().into_owned()];
    argv.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    let argv = strip_qos(argv);
    let mut set = Vec::new();
    let mut unset = Vec::new();
    let mut name = String::new();
    let mut harness = String::new();
    let mut node = None;
    let mut from = None;
    for (k, v) in cmd.get_envs() {
        let k = k.to_string_lossy().into_owned();
        match v {
            // A sandboxed spawn always keeps its row; PWD names the host checkout.
            _ if k == crate::session_report::NO_ROW_ENV || k == "PWD" => {}
            Some(v) => {
                let text = v.to_string_lossy().into_owned();
                match k.as_str() {
                    "FNO_AGENT_SELF" => name = text,
                    "FNO_AGENT_HARNESS" => harness = text,
                    "FNO_NODE" if !text.is_empty() => node = Some(text),
                    "FNO_AGENT_FROM" if !text.is_empty() => from = Some(text),
                    _ => {}
                }
                set.push(k);
            }
            None => unset.push(k),
        }
    }
    if harness.is_empty() {
        harness = Path::new(argv.first().map(String::as_str).unwrap_or_default())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    // The spawn client exports FNO_NODE into its own env, which the lane
    // inherits without naming it.
    let node = node.or_else(|| std::env::var("FNO_NODE").ok().filter(|v| !v.is_empty()));
    let (argv, session) = bind_session(&harness, argv);
    let args = crate::sandbox::relay::Args {
        id: crate::sandbox::mint_id(if name.is_empty() { &harness } else { &name }),
        name,
        transcript_dir: transcript_dir(&harness),
        harness,
        provider: plan.provider.clone(),
        image: plan.image.clone(),
        cwd: worktree.clone(),
        session,
        set,
        unset,
        node,
        from,
        engine_host: plan.host.clone(),
        argv,
    };
    let exe = crate::client_exe::current()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "fno-agent".to_string());
    let wrapped = args.to_argv(&exe);
    let mut out = Command::new(&exe);
    out.args(wrapped.get(1..).unwrap_or(&[]));
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => {
                out.env(k, v);
            }
            None => {
                out.env_remove(k);
            }
        }
    }
    out.env_remove(crate::session_report::NO_ROW_ENV);
    if let Some(dir) = cmd.get_current_dir() {
        out.current_dir(dir);
    }
    out
}

/// The session the relay stamps. Claude takes a pre-minted id; a caller id
/// already on the argv wins. Other harnesses bind none here.
fn bind_session(harness: &str, mut argv: Vec<String>) -> (Vec<String>, Option<String>) {
    if harness != "claude" {
        return (argv, None);
    }
    if let Some(i) = argv.iter().position(|a| a == "--session-id") {
        let sid = argv.get(i + 1).cloned();
        return (argv, sid);
    }
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).ok();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let sid = format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    );
    // Before a `--` prompt fence, so the id stays a flag.
    let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
    argv.splice(at..at, ["--session-id".to_string(), sid.clone()]);
    (argv, Some(sid))
}

/// The QoS prefix (`spawn_gate::qos_wrap`) names a host binary the sandbox may
/// not carry, and it would lower the priority of the host's engine client,
/// not the worker. Drop it.
fn strip_qos(argv: Vec<String>) -> Vec<String> {
    let prefix: &[&str] = match argv.first().map(String::as_str) {
        Some("/usr/sbin/taskpolicy") => &["/usr/sbin/taskpolicy", "-c", "utility", "--"],
        Some("/usr/bin/nice") => &["/usr/bin/nice", "-n", "10"],
        _ => return argv,
    };
    if argv.len() > prefix.len() && argv.iter().zip(prefix).all(|(a, p)| a == p) {
        argv[prefix.len()..].to_vec()
    } else {
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layer rule: a project can raise `none`, never lower or switch a
    /// global provider, and never set the image; an unparsable layer or an
    /// unknown value refuses. A claude argv gains a session id before the
    /// prompt fence, and the QoS prefix drops.
    #[test]
    fn policy_layers_raise_only_and_claude_binds_a_session() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let read = |g: &str, p: &str| {
            std::fs::write(&global, g).unwrap();
            std::fs::write(&project, p).unwrap();
            let mut out = Vec::new();
            for path in [&project, &global] {
                out.push(layer(path));
            }
            out
        };
        let layers = read(
            "[sandbox]\nprovider = \"docker\"\nimage = \"img\"\n",
            "[sandbox]\nprovider = \"none\"\n",
        );
        assert_eq!(
            layers.get(1).cloned().unwrap().unwrap(),
            Some((Some("docker".into()), Some("img".into()), None))
        );
        let layers = read("[sandbox]\nhost = \"ssh://-oProxyCommand=x\"\n", "");
        assert!(layers.get(1).cloned().unwrap().is_err());
        let layers = read("", "[sandbox]\nprovider = \"nope\"\n");
        assert!(layers
            .first()
            .cloned()
            .unwrap()
            .unwrap_err()
            .0
            .contains("not one of"));
        let layers = read("", "not = [toml");
        assert!(layers
            .first()
            .cloned()
            .unwrap()
            .unwrap_err()
            .0
            .contains("does not parse"));

        let (argv, sid) = bind_session(
            "claude",
            strip_qos(
                ["/usr/bin/nice", "-n", "10", "claude", "-p", "--", "hi"]
                    .map(String::from)
                    .to_vec(),
            ),
        );
        let sid = sid.unwrap();
        assert_eq!(
            argv,
            ["claude", "-p", "--session-id", sid.as_str(), "--", "hi"]
        );
        assert_eq!(sid.len(), 36);
        assert_eq!(bind_session("codex", vec!["codex".into()]).1, None);
    }
}
