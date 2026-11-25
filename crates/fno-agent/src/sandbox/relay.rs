//! `fno-agent hook sandbox-run`: the host wrapper a sandboxed lane runs in
//! place of its harness. It owns the whole lifecycle, so the row's pid is
//! this pid:
//!
//! 1. resolve the image (a devcontainer is only an image source);
//! 2. start a labelled, isolated container;
//! 3. start the guest and prove it with a status frame;
//! 4. clone committed HEAD inside;
//! 5. rewrite host paths in the harness argv;
//! 6. run the harness with stdio and exit code passed through;
//! 7. export commits, dirty patch and untracked archive;
//! 8. fetch the commits into `refs/sandbox/<id>/head`;
//! 9. remove the container, or keep and name it when the export failed.
//!
//! Argv carries env key names only. The values come from this process's own
//! env, set by the lane, and cross to the guest over exec stdin.

use super::{Refusal, Res};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::engine::{DevcontainerImage, Engine, Provider};
use super::transcript::Store;
use super::wire::{GuestFrame, HostFrame, CHUNK};
use super::{CONTAINER_SETTINGS, CONTAINER_SOCK, CONTAINER_WORK, PROXY_ADDR};

const DAEMON_BOUND: Duration = Duration::from_secs(10);
/// How long one proxied stream may block on a peer that stops reading.
const PEER_WRITE_BOUND: Duration = Duration::from_secs(30);
const FLUSH_BOUND: Duration = Duration::from_secs(20);
const REFUSED: i32 = 84;

/// The parsed `sandbox-run` argv.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Args {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub provider: String,
    pub image: Option<String>,
    pub cwd: PathBuf,
    pub session: Option<String>,
    pub set: Vec<String>,
    pub unset: Vec<String>,
    pub transcript_dir: Option<String>,
    /// The node a target spawn carries (`FNO_NODE`). The host binds its
    /// worktree, branch, claim and plan before the clone.
    pub node: Option<String>,
    /// The spawner (`FNO_AGENT_FROM`), the only mail recipient.
    pub from: Option<String>,
    /// `sandbox.host`: a remote engine on the user's own VM.
    pub engine_host: Option<String>,
    pub argv: Vec<String>,
}

impl Args {
    /// The argv that [`Args::parse`] reads back. The lane builds it.
    pub fn to_argv(&self, exe: &str) -> Vec<String> {
        let mut v = vec![exe.to_string(), "hook".into(), "sandbox-run".into()];
        let mut push = |k: &str, val: &str| {
            v.push(k.to_string());
            v.push(val.to_string());
        };
        push("--id", &self.id);
        push("--name", &self.name);
        push("--harness", &self.harness);
        push("--provider", &self.provider);
        push("--cwd", &self.cwd.display().to_string());
        if let Some(i) = &self.image {
            push("--image", i);
        }
        if let Some(s) = &self.session {
            push("--session", s);
        }
        if let Some(d) = &self.transcript_dir {
            push("--transcript-dir", d);
        }
        if let Some(n) = &self.node {
            push("--node", n);
        }
        if let Some(f) = &self.from {
            push("--from", f);
        }
        if let Some(h) = &self.engine_host {
            push("--engine-host", h);
        }
        for k in &self.set {
            push("--set", k);
        }
        for k in &self.unset {
            push("--unset", k);
        }
        v.push("--".into());
        v.extend(self.argv.iter().cloned());
        v
    }

    pub fn parse(args: &[String]) -> Res<Self> {
        let mut a = Args::default();
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            if flag == "--" {
                a.argv = it.cloned().collect();
                break;
            }
            let val = it
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))?
                .clone();
            match flag.as_str() {
                "--id" => a.id = val,
                "--name" => a.name = val,
                "--harness" => a.harness = val,
                "--provider" => a.provider = val,
                "--image" => a.image = Some(val),
                "--cwd" => a.cwd = PathBuf::from(val),
                "--session" => a.session = Some(val),
                "--transcript-dir" => a.transcript_dir = Some(val),
                "--node" => a.node = Some(val),
                "--from" => a.from = Some(val),
                "--engine-host" => a.engine_host = Some(val),
                "--set" => a.set.push(val),
                "--unset" => a.unset.push(val),
                other => return Err(Refusal(format!("unknown flag {other}"))),
            }
        }
        if !super::valid_id(&a.id) {
            return Err(Refusal(format!(
                "--id {} is not a sandbox id",
                super::escape(&a.id, 80)
            )));
        }
        if a.argv.is_empty() {
            return Err("no harness command after --".into());
        }
        if let Some(n) = &a.node {
            let ok = n.len() <= 40
                && n.split_once('-').is_some_and(|(p, h)| {
                    !p.is_empty()
                        && p.bytes().all(|b| b.is_ascii_lowercase())
                        && !h.is_empty()
                        && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
            if !ok {
                return Err(Refusal(format!(
                    "--node {} is not a node id",
                    super::escape(n, 40)
                )));
            }
        }
        Ok(a)
    }
}

pub fn run(args: &[String]) -> i32 {
    let a = match Args::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fno-agent hook sandbox-run: {e}");
            return 2;
        }
    };
    match lifecycle(&a) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fno sandbox {}: refused: {e}", a.id);
            REFUSED
        }
    }
}

/// Rewrite host paths for the inside: the worktree and its subpaths move to
/// the work path, `--settings` points at the guest copy, host `--add-dir`
/// grants drop, and any other host path refuses by name. Returns the argv
/// and the settings file to send.
pub fn rewrite_argv(argv: &[String], worktree: &Path) -> Res<(Vec<String>, Option<PathBuf>)> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let wt = canonical(worktree);
    let inside = |a: &str| -> Option<String> {
        let p = Path::new(a);
        if !p.is_absolute() {
            return None;
        }
        let c = canonical(p);
        c.strip_prefix(&wt).ok().map(|rest| {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                CONTAINER_WORK.to_string()
            } else {
                format!("{CONTAINER_WORK}/{rest}")
            }
        })
    };
    let host = |a: &str| Path::new(a).is_absolute() && Path::new(a).exists();
    // The harness resolves on the container's PATH.
    let Some((prog, rest)) = argv.split_first() else {
        return Err("no harness command".into());
    };
    let name = Path::new(prog)
        .file_name()
        .map_or_else(|| prog.clone(), |n| n.to_string_lossy().into_owned());
    let codex = name == "codex";
    let mut out = vec![name];
    let mut settings = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // Codex's own bwrap sandbox cannot start in a container with no
            // capabilities and no user namespaces; the container is the
            // boundary there.
            "--sandbox" | "-s" if codex => {
                it.next();
                out.extend([a.clone(), "danger-full-access".to_string()]);
            }
            "--add-dir" => match it.next() {
                Some(d) => match inside(d) {
                    Some(p) => out.extend([a.clone(), p]),
                    None if host(d) => {}
                    None => out.extend([a.clone(), d.clone()]),
                },
                None => out.push(a.clone()),
            },
            "--settings" => match it.next() {
                Some(f) if host(f) => {
                    settings = Some(PathBuf::from(f));
                    out.extend([a.clone(), CONTAINER_SETTINGS.to_string()]);
                }
                Some(f) => out.extend([a.clone(), f.clone()]),
                None => out.push(a.clone()),
            },
            _ => match inside(a) {
                Some(p) => out.push(p),
                None if host(a) => {
                    return Err(Refusal(format!(
                    "the command names {}, a host path outside the worktree the sandbox cannot see",
                    super::escape(a, 200)
                )))
                }
                None => out.push(a.clone()),
            },
        }
    }
    Ok((out, settings))
}

fn home() -> crate::paths::AgentsHome {
    crate::paths::AgentsHome::from_env()
}

fn lifecycle(a: &Args) -> Res<i32> {
    let provider = Provider::parse(&a.provider)
        .ok_or_else(|| format!("unknown provider {}", super::escape(&a.provider, 40)))?;
    let mut engine = provider.engine()?;
    if let Some(h) = &a.engine_host {
        super::engine::SshHost::parse(h)?.preflight()?;
        engine = engine.on(Some(super::leak_host(h.clone())));
    }
    engine.probe()?;
    let (argv, settings_file) = rewrite_argv(&a.argv, &a.cwd)?;
    let home = home();
    sweep(&engine, &home);
    let image = resolve_image(&engine, provider, a)?;
    let binding = bind_target(a)?;
    let bound_args;
    let a = match &binding {
        Some(t) => {
            bound_args = Args {
                cwd: t.worktree.clone(),
                ..a.clone()
            };
            &bound_args
        }
        None => a,
    };
    let scratch = tempfile::tempdir().map_err(|e| format!("scratch dir: {e}"))?;
    let store_dir = home.sandbox_exports_dir().join(&a.id);
    std::fs::create_dir_all(&store_dir).map_err(|e| format!("{}: {e}", store_dir.display()))?;
    engine.start(&a.id, &image)?;
    let mut sandbox = Sandbox {
        engine,
        id: a.id.clone(),
        kept: false,
    };
    let intake = super::repo::clone_in(&engine, &a.id, &a.cwd, scratch.path())?;
    write_meta(&store_dir, a, &intake.base);
    let transcript = Store::create(
        &home.sandbox_transcripts_dir(),
        &a.id,
        super::transcript::CAP,
    )
    .map_err(|e| format!("transcript file: {e}"))?;
    let transcript_path = transcript.path().to_path_buf();
    let settings = match &settings_file {
        Some(f) => Some(std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?),
        None => None,
    };
    let env = guest_env(a);

    let mut guest = engine
        .exec_piped(&a.id, &["fno-agent", "hook", "sandbox-guest"])
        .spawn()
        .map_err(|e| format!("start the guest: {e}"))?;
    let bound = bound_for(
        a,
        binding.as_ref(),
        &intake,
        &store_dir,
        &transcript_path,
        engine,
    );
    let (tx, flushed) = start_relay(&mut guest, a, transcript, bound)?;
    tx.send(HostFrame::Hello {
        env,
        settings,
        transcript_dir: a.transcript_dir.clone(),
    })
    .map_err(|_| "the guest stream closed before hello")?;
    engine
        .exec_out(&a.id, &["fno-agent", "hook", "sandbox-guest", "probe"])
        .map_err(|e| format!("the guest did not answer a status frame: {e}"))?;

    banner(a, engine.bin, &image, &intake);
    start_report(a, &home, &transcript_path);
    let mut harness_argv = vec![
        "fno-agent".to_string(),
        "hook".into(),
        "sandbox-guest".into(),
        "exec".into(),
        "--".into(),
    ];
    harness_argv.extend(argv);
    let status = engine
        .exec_inherit(&a.id, &harness_argv)
        .status()
        .map_err(|e| format!("run the harness: {e}"))?;

    tx.send(HostFrame::Shutdown).ok();
    flushed.recv_timeout(FLUSH_BOUND).ok();
    drop(tx);
    guest.kill().ok();
    guest.wait().ok();

    finish(&mut sandbox, a, &intake, &store_dir, &transcript_path);
    Ok(exit_code(status))
}

/// The host side every op answers from, fixed at launch.
fn bound_for(
    a: &Args,
    binding: Option<&Target>,
    intake: &super::repo::Intake,
    store_dir: &Path,
    transcript_path: &Path,
    engine: super::engine::Engine,
) -> Arc<super::ops::Bound> {
    Arc::new(super::ops::Bound {
        id: a.id.clone(),
        name: a.name.clone(),
        harness: a.harness.clone(),
        session: a.session.clone(),
        from: a.from.clone(),
        node: a.node.clone(),
        branch: binding.and_then(|t| t.branch.clone()),
        worktree: a.cwd.clone(),
        plan: binding.and_then(|t| t.plan.clone()),
        base: intake.base.clone(),
        store: store_dir.to_path_buf(),
        transcript: transcript_path.to_path_buf(),
        engine,
        export: Mutex::new(()),
    })
}

/// Export the sandbox's work and remove the container; a failed export
/// keeps the container with its work.
fn finish(
    sandbox: &mut Sandbox,
    a: &Args,
    intake: &super::repo::Intake,
    store_dir: &Path,
    transcript_path: &Path,
) {
    let engine = sandbox.engine;
    match super::repo::export(&engine, &a.id, &a.id, &intake.base, &a.cwd, store_dir) {
        Ok(out) => {
            receipt(&a.id, &out, transcript_path);
            if let Err(e) = sandbox.engine.remove(&a.id) {
                eprintln!("fno sandbox {}: remove failed, container kept: {e}", a.id);
            }
            sandbox.kept = true;
        }
        Err(e) => {
            sandbox.kept = true;
            eprintln!(
                "fno sandbox {}: export failed, container {} kept with its work: {e}",
                a.id, a.id
            );
        }
    }
}

/// What the host bound for a target spawn before the clone.
struct Target {
    worktree: PathBuf,
    branch: Option<String>,
    plan: Option<PathBuf>,
}

/// A spawn that carries a node gets the normal host start: a worktree off
/// the default branch, the node claim held for the sandbox session, and the
/// manifest. It runs on the host with a host-built argv, before any sandbox
/// content exists. A spawn with no node binds nothing.
fn bind_target(a: &Args) -> Res<Option<Target>> {
    let Some(node) = &a.node else {
        return Ok(None);
    };
    let mut c = std::process::Command::new(super::ops::sibling("fno"));
    c.args(["do", "target", "start", node, "--no-merge"])
        .current_dir(&a.cwd)
        .env_remove("FNO_AGENT_SANDBOXED");
    // The manifest names the sandbox's session, never an identity the
    // spawning shell carried: the host Stop resolves its owner by this id.
    crate::claims::stamp_command_env(
        &mut c,
        Some(a.name.as_str()),
        &a.harness,
        a.session.as_deref(),
    );
    if let (Some(sid), "claude") = (&a.session, a.harness.as_str()) {
        c.env("CLAUDE_CODE_SESSION_ID", sid);
    }
    let out = crate::bounded_cmd::output_with_timeout_result(c, 600)
        .map_err(|e| format!("fno do target start {node}: {e}"))?;
    let receipt = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(Refusal(format!(
            "the host could not start node {node} for the sandbox (exit {}): {}",
            out.status.code().unwrap_or(-1),
            super::escape(receipt.trim(), 600)
        )));
    }
    let worktree = super::ops::receipt_worktree(&receipt).ok_or_else(|| {
        format!("fno do target start {node} printed no worktree= line; the sandbox binds nothing")
    })?;
    let mut g = std::process::Command::new("git");
    g.arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "--abbrev-ref", "HEAD"]);
    let branch = crate::bounded_cmd::output_with_timeout_result(g, 60)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    let plan = super::ops::manifest_plan(&worktree);
    eprintln!(
        "fno sandbox {}: bound node {node}, branch {}, host worktree {}",
        a.id,
        branch.as_deref().unwrap_or("?"),
        worktree.display()
    );
    Ok(Some(Target {
        worktree,
        branch,
        plan,
    }))
}

/// The env the guest hands the harness: the lane's set and unset keys with
/// their values from this process, the proxy, and the pinned socket.
fn guest_env(a: &Args) -> Vec<(String, Option<String>)> {
    let mut env: Vec<(String, Option<String>)> = a
        .set
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.clone(), Some(v))))
        .collect();
    env.extend(a.unset.iter().map(|k| (k.clone(), None)));
    let proxy = format!("http://{PROXY_ADDR}");
    for k in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "https_proxy",
        "http_proxy",
        "ALL_PROXY",
    ] {
        env.push((k.into(), Some(proxy.clone())));
    }
    env.push(("NO_PROXY".into(), Some(String::new())));
    env.push(("FNO_SUPERVISOR_SOCKET".into(), Some(CONTAINER_SOCK.into())));
    env.push(("FNO_AGENT_SANDBOXED".into(), Some(a.id.clone())));
    env
}

fn banner(a: &Args, engine: &str, image: &str, intake: &super::repo::Intake) {
    eprintln!(
        "fno sandbox {}: {} on {} image {}, clone of {}{}",
        a.id,
        a.harness,
        engine,
        super::escape(image, 120),
        &intake.base[..intake.base.len().min(12)],
        if intake.excluded_dirty.is_empty() {
            String::new()
        } else {
            format!(
                "; dirty host paths not carried: {}",
                intake.excluded_dirty.join(", ")
            )
        }
    );
}

/// The host-side start report: the row exists, and names its sandbox
/// transcript, whether or not the image carries the plugin's hooks.
fn start_report(a: &Args, home: &crate::paths::AgentsHome, transcript_path: &Path) {
    if let Some(sid) = &a.session {
        let params = json!({
            "harness": a.harness,
            "session_id": sid,
            "source": "startup",
            "agent_self": a.name,
            "cwd": a.cwd,
            "sandbox_transcript": transcript_path,
        });
        let method = format!("agent.{}", crate::session_report::SESSION_REPORT_COMMAND);
        let sock = crate::client::client_sock(home);
        if crate::protocol::blocking_call(&sock, &method, params, DAEMON_BOUND).is_none() {
            eprintln!(
                "fno sandbox {}: the daemon took no start report; the row may be missing",
                a.id
            );
        }
    }
}

/// Remove the container when the lifecycle bails before the export. A run
/// that reached the export decides for itself.
struct Sandbox {
    engine: Engine,
    id: String,
    kept: bool,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !self.kept {
            self.engine.remove(&self.id).ok();
        }
    }
}

fn exit_code(s: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(1))
}

fn receipt(id: &str, out: &super::repo::Export, transcript: &Path) {
    match &out.head {
        Some(head) => eprintln!(
            "fno sandbox {id}: commits at refs/sandbox/{id}/head ({}); inspect with: git log -p refs/sandbox/{id}/head",
            &head[..head.len().min(12)]
        ),
        None => eprintln!("fno sandbox {id}: no new commits"),
    }
    if let Some(p) = &out.patch {
        eprintln!(
            "fno sandbox {id}: dirty work stored, never applied: {}",
            p.display()
        );
    }
    if let Some(p) = &out.untracked {
        eprintln!(
            "fno sandbox {id}: untracked files stored, never extracted: {}",
            p.display()
        );
    }
    eprintln!(
        "fno sandbox {id}: transcript (sandbox origin): {}",
        transcript.display()
    );
}

/// The export record a sweep needs to export a container whose wrapper died.
#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    id: String,
    provider: String,
    cwd: PathBuf,
    base: String,
    /// The node a target spawn bound; its `cwd` is then a host worktree
    /// that holds none of the sandbox's work.
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    host: Option<String>,
}

fn write_meta(dir: &Path, a: &Args, base: &str) {
    let meta = Meta {
        id: a.id.clone(),
        provider: a.provider.clone(),
        cwd: a.cwd.clone(),
        base: base.to_string(),
        node: a.node.clone(),
        host: a.engine_host.clone(),
    };
    if let Ok(body) = serde_json::to_string(&meta) {
        std::fs::write(dir.join("meta.json"), body).ok();
    }
}

/// Export, then remove, every labelled container whose wrapper is dead. A
/// failed export keeps the container and names it.
fn sweep(engine: &Engine, home: &crate::paths::AgentsHome) {
    for (name, pid) in engine.labelled() {
        // SAFETY: kill with signal 0 only probes the pid.
        let alive = pid.is_some_and(|p| unsafe { libc::kill(p as i32, 0) } == 0);
        if alive {
            continue;
        }
        let dir = home.sandbox_exports_dir().join(&name);
        let meta: Option<Meta> = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let Some(meta) = meta else {
            eprintln!("fno sandbox sweep: {name} has no export record; kept");
            continue;
        };
        match super::repo::export(engine, &name, &name, &meta.base, &meta.cwd, &dir) {
            Ok(out) => {
                eprintln!("fno sandbox sweep: {name} exported after its wrapper died");
                receipt(
                    &name,
                    &out,
                    &home.sandbox_transcripts_dir().join(format!("{name}.jsonl")),
                );
                engine.remove(&name).ok();
            }
            Err(e) => eprintln!("fno sandbox sweep: {name} kept, export failed: {e}"),
        }
    }
}

fn resolve_image(engine: &Engine, provider: Provider, a: &Args) -> Res<String> {
    if provider == Provider::Devcontainer {
        return devcontainer_image(engine, &a.cwd);
    }
    if let Some(image) = a.image.as_deref().filter(|i| !i.is_empty()) {
        if !engine.image_present(image) {
            engine.call(&["pull", image])?;
        }
        return Ok(image.to_string());
    }
    let rev = env!("FNO_AGENT_CRATES_REV");
    let tag = format!("fno-sandbox:{rev}");
    if engine.image_present(&tag) {
        return Ok(tag);
    }
    let ctx = tempfile::tempdir().map_err(|e| format!("build dir: {e}"))?;
    let bin = ctx.path().join("bin");
    let plugin = ctx.path().join("plugin");
    std::fs::create_dir_all(&plugin).map_err(|e| format!("{}: {e}", plugin.display()))?;
    fetch_binaries(engine, rev, &bin)?;
    stage_plugin(rev, &plugin);
    let dockerfile = ctx.path().join("Dockerfile");
    std::fs::write(&dockerfile, super::engine::RECIPE)
        .map_err(|e| format!("{}: {e}", dockerfile.display()))?;
    engine.build(&tag, &dockerfile, ctx.path(), &[])?;
    Ok(tag)
}

/// Linux binaries for `rev` and the engine's arch from bin-cache, checked
/// by sha256. A dev or dirty rev has none; set `[sandbox] image` to an image
/// built from the recipe with a local binary directory.
fn fetch_binaries(engine: &Engine, rev: &str, bin: &Path) -> Res<()> {
    use sha2::{Digest as _, Sha256};
    let arch = engine
        .call(&["version", "--format", "{{.Server.Arch}}"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let platform = match arch.as_str() {
        "amd64" | "x86_64" => "linux-x64",
        "arm64" | "aarch64" => "linux-arm64",
        other => {
            return Err(Refusal(format!(
                "no Linux build for engine arch {}",
                super::escape(other, 20)
            )))
        }
    };
    let url = format!("https://github.com/fnodotdev/fno/releases/download/bin-cache/fno-bin-{rev}-{platform}.tar.gz");
    std::fs::create_dir_all(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let tarball = bin.join("fno.tar.gz");
    let curl = |u: &str, to: &Path| -> Res<()> {
        let mut c = std::process::Command::new("curl");
        c.args(["-fsSL", "--max-time", "120", "-o"]).arg(to).arg(u);
        match crate::bounded_cmd::output_with_timeout_result(c, 150) {
            Ok(o) if o.status.success() => Ok(()),
            _ => Err(Refusal(format!(
                "bin-cache has no {platform} build for rev {rev}; build an image from the sandbox recipe \
                 with a local binary directory and set [sandbox] image to it"
            ))),
        }
    };
    curl(&format!("{url}.sha256"), &bin.join("sha256"))?;
    curl(&url, &tarball)?;
    let want = std::fs::read_to_string(bin.join("sha256")).unwrap_or_default();
    let want = want.split_whitespace().next().unwrap_or("");
    let bytes = std::fs::read(&tarball).map_err(|e| format!("{}: {e}", tarball.display()))?;
    let got = format!("{:x}", Sha256::digest(&bytes));
    if got != want {
        return Err(Refusal(format!(
            "bin-cache sha256 mismatch for {platform} rev {rev}"
        )));
    }
    let mut c = std::process::Command::new("tar");
    c.arg("-xzf").arg(&tarball).arg("-C").arg(bin);
    let ok =
        crate::bounded_cmd::output_with_timeout_result(c, 120).is_ok_and(|o| o.status.success());
    std::fs::remove_file(&tarball).ok();
    std::fs::remove_file(bin.join("sha256")).ok();
    if ok {
        Ok(())
    } else {
        Err("could not unpack the bin-cache tarball".into())
    }
}

/// Copy the staged plugin whose source head is this rev. Any other stage is
/// left out: a mismatched plugin is worse than none.
fn stage_plugin(rev: &str, into: &Path) {
    let Some(stage) = dirs_home().map(|h| h.join(".fno/install/plugin-stage/fno")) else {
        return;
    };
    #[derive(serde::Deserialize)]
    struct Stamp {
        source_head: Option<String>,
    }
    let head = std::fs::read_to_string(stage.join(".fno-stage.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Stamp>(&s).ok())
        .and_then(|v| v.source_head);
    if head
        .as_deref()
        .is_some_and(|h| h.starts_with(rev) || rev.starts_with(h))
    {
        let mut c = std::process::Command::new("cp");
        c.arg("-R").arg(format!("{}/.", stage.display())).arg(into);
        crate::bounded_cmd::output_with_timeout_result(c, 120).ok();
    } else {
        eprintln!("fno sandbox: no plugin stage matches rev {rev}; the image carries no plugin");
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The devcontainer image source, read from the default-branch commit in a
/// clean export folder, never the worktree.
fn devcontainer_image(engine: &Engine, cwd: &Path) -> Res<String> {
    let git = |args: &[&str]| -> Res<String> {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(cwd).args(args);
        match crate::bounded_cmd::output_with_timeout_result(c, 120) {
            Ok(o) if o.status.success() => {
                Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().into()),
            Err(e) => Err(e.to_string().into()),
        }
    };
    let branch = git(&["rev-parse", "--verify", "-q", "refs/remotes/origin/HEAD"])
        .or_else(|_| git(&["rev-parse", "--verify", "-q", "refs/heads/main"]))
        .map_err(|_| "no default branch to read the devcontainer from".to_string())?;
    let export = tempfile::tempdir().map_err(|e| format!("export dir: {e}"))?;
    let tar = export.path().join("tree.tar");
    git(&[
        "archive",
        "--format=tar",
        "-o",
        &tar.display().to_string(),
        &branch,
    ])?;
    let mut c = std::process::Command::new("tar");
    c.arg("-xf").arg(&tar).arg("-C").arg(export.path());
    crate::bounded_cmd::output_with_timeout_result(c, 120).map_err(|e| format!("tar: {e}"))?;
    let file = export.path().join(".devcontainer/devcontainer.json");
    match super::engine::devcontainer_image(&file, export.path())? {
        DevcontainerImage::Pull(image) => {
            if !engine.image_present(&image) {
                engine.call(&["pull", &image])?;
            }
            Ok(image)
        }
        DevcontainerImage::Build {
            dockerfile,
            context,
        } => {
            let tag = format!("fno-sandbox-dev:{}", &branch[..branch.len().min(12)]);
            engine.build(&tag, &dockerfile, &context, &[])?;
            Ok(tag)
        }
    }
}

/// The relay: one reader thread for guest frames and one writer thread for
/// host frames. Returns the frame sender and the `Flushed` signal.
fn start_relay(
    guest: &mut Child,
    a: &Args,
    mut store: Store,
    bound: Arc<super::ops::Bound>,
) -> Res<(Sender<HostFrame>, mpsc::Receiver<()>)> {
    let mut stdin = guest.stdin.take().ok_or("the guest has no stdin")?;
    let mut stdout = guest.stdout.take().ok_or("the guest has no stdout")?;
    let (tx, rx) = mpsc::channel::<HostFrame>();
    std::thread::spawn(move || {
        for f in rx {
            if f.write(&mut stdin).is_err() {
                return;
            }
        }
    });
    let (flush_tx, flush_rx) = mpsc::channel();
    let ctx = Arc::new(Ctx {
        tx: tx.clone(),
        name: a.name.clone(),
        harness: a.harness.clone(),
        session: a.session.clone(),
        cwd: a.cwd.display().to_string(),
        seq: AtomicU64::new(1),
        streams: Mutex::default(),
        owned: {
            let mut owned = super::egress::host_addresses();
            if let Some(h) = &a.engine_host {
                owned.extend(super::egress::remote_addresses(h));
            }
            owned
        },
        sock: crate::client::client_sock(&home()),
        transcript: store.path().display().to_string(),
        bound,
        seen: Mutex::default(),
    });
    std::thread::spawn(move || loop {
        match GuestFrame::read(&mut stdout) {
            Ok(Some(GuestFrame::Transcript { offset, bytes })) => {
                if let Err(e) = store.append(offset, &bytes) {
                    eprintln!("fno sandbox: transcript write: {e}");
                }
            }
            Ok(Some(GuestFrame::Flushed)) => {
                if store.capped() {
                    eprintln!("fno sandbox: the transcript reached its size cap and was cut");
                }
                flush_tx.send(()).ok();
            }
            Ok(Some(frame)) => ctx.handle(frame),
            Ok(None) => return,
            Err(e) => {
                eprintln!("fno sandbox: guest stream refused: {e}");
                return;
            }
        }
    });
    Ok((tx, flush_rx))
}

struct Ctx {
    tx: Sender<HostFrame>,
    name: String,
    harness: String,
    session: Option<String>,
    cwd: String,
    seq: AtomicU64,
    streams: Mutex<HashMap<u32, TcpStream>>,
    owned: Vec<IpAddr>,
    sock: PathBuf,
    transcript: String,
    bound: Arc<super::ops::Bound>,
    /// Op sequence numbers already taken: a replayed write never runs twice.
    seen: Mutex<HashSet<u64>>,
}

impl Ctx {
    fn reply(&self, req: u32, answer: Res<Value>) {
        let (ok, body) = match answer {
            Ok(v) => (true, v.to_string()),
            Err(e) => (false, e.0),
        };
        self.tx.send(HostFrame::Reply { req, ok, body }).ok();
    }

    fn daemon(&self, method: &str, params: Value) -> Res<Value> {
        crate::protocol::blocking_call(&self.sock, method, params, DAEMON_BOUND).ok_or_else(|| {
            Refusal(format!(
                "the host daemon refused or did not answer {method}"
            ))
        })
    }

    /// One guest frame. Every param the daemon reads is built here from the
    /// spawn this relay owns; nothing from the guest but the typed field.
    fn handle(self: &Arc<Self>, frame: GuestFrame) {
        match frame {
            GuestFrame::Status { req } => {
                self.reply(req, Ok(json!({"status": "ok", "sandboxed": true})))
            }
            GuestFrame::Report { req, state } => {
                let answer = match &self.session {
                    Some(sid) => self.daemon(
                        "agent.report",
                        json!({
                            "session_id": sid,
                            "seq": self.seq.fetch_add(1, Ordering::Relaxed),
                            "state": state.as_str(),
                        }),
                    ),
                    None => Err("this sandbox has no bound session to report on".into()),
                };
                self.reply(req, answer);
            }
            GuestFrame::SessionReport { req, source } => {
                let answer = match &self.session {
                    Some(sid) => self.daemon(
                        &format!("agent.{}", crate::session_report::SESSION_REPORT_COMMAND),
                        json!({
                            "harness": self.harness,
                            "session_id": sid,
                            "source": source,
                            "agent_self": self.name,
                            "cwd": self.cwd,
                            "sandbox_transcript": self.transcript,
                        }),
                    ),
                    None => Err("this sandbox has no bound session to report".into()),
                };
                self.reply(req, answer);
            }
            GuestFrame::Connect { stream, host, port } => {
                let me = self.clone();
                std::thread::spawn(move || me.connect(stream, &host, port));
            }
            GuestFrame::Data { stream, bytes } => {
                let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = streams.get_mut(&stream) {
                    if s.write_all(&bytes).is_err() {
                        streams.remove(&stream);
                    }
                }
            }
            GuestFrame::Close { stream } => {
                if let Some(s) = self
                    .streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stream)
                {
                    s.shutdown(std::net::Shutdown::Write).ok();
                }
            }
            GuestFrame::Op { req, seq, op } => {
                let fresh = self
                    .seen
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(seq);
                if !fresh {
                    let answer = Err(Refusal(format!(
                        "op {} seq {seq} already ran; a replay never runs twice",
                        op.name()
                    )));
                    self.reply(req, answer);
                    return;
                }
                // Every op runs off the reader thread, so transcript and
                // proxy frames never wait behind a push or a gh call.
                let me = self.clone();
                std::thread::spawn(move || {
                    let answer = super::ops::handle(&me.bound, &op);
                    me.reply(req, answer);
                });
            }
            GuestFrame::Transcript { .. } | GuestFrame::Flushed => {}
        }
    }

    fn connect(&self, stream: u32, host: &str, port: u16) {
        let conn = match super::egress::dial(host, port, &self.owned) {
            Ok(c) => c,
            Err(reason) => {
                self.tx
                    .send(HostFrame::Connected {
                        stream,
                        ok: false,
                        reason: reason.0,
                    })
                    .ok();
                return;
            }
        };
        // Data frames write on the guest-stream thread, so a peer that stops
        // reading costs this stream, never every frame behind it.
        conn.set_write_timeout(Some(PEER_WRITE_BOUND)).ok();
        let Ok(mut reader) = conn.try_clone() else {
            return;
        };
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(stream, conn);
        self.tx
            .send(HostFrame::Connected {
                stream,
                ok: true,
                reason: String::new(),
            })
            .ok();
        let mut buf = vec![0u8; CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let frame = HostFrame::Data {
                        stream,
                        bytes: buf.get(..n).unwrap_or_default().to_vec(),
                    };
                    if self.tx.send(frame).is_err() {
                        break;
                    }
                }
            }
        }
        self.tx.send(HostFrame::Close { stream }).ok();
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lane's argv parses back unchanged; a worktree path moves inside,
    /// a host settings file points at the guest copy, a host grant drops, and
    /// any other host path refuses by name.
    #[test]
    fn args_round_trip_and_host_paths_rewrite_or_refuse() {
        let wt = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let settings = other.path().join("route.json");
        std::fs::write(&settings, "{}").unwrap();
        let a = Args {
            id: "w-0000abcd".into(),
            name: "w".into(),
            harness: "codex".into(),
            provider: "docker".into(),
            cwd: wt.path().to_path_buf(),
            node: Some("x-aaaa".into()),
            from: Some("lead".into()),
            engine_host: Some("ssh://runner@localhost".into()),
            set: vec!["ROUTE_KEY".into()],
            argv: vec![
                "/opt/bin/codex".into(),
                "-C".into(),
                wt.path().display().to_string(),
            ],
            ..Args::default()
        };
        let argv = a.to_argv("fno-agent");
        assert_eq!(Args::parse(argv.get(3..).unwrap()).unwrap(), a);
        let sub = wt.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let (out, file) = rewrite_argv(
            &[
                "/opt/bin/claude".into(),
                "--settings".into(),
                settings.display().to_string(),
                "--add-dir".into(),
                other.path().display().to_string(),
                "--add-dir".into(),
                sub.display().to_string(),
                "-p".into(),
                "hi".into(),
            ],
            wt.path(),
        )
        .unwrap();
        assert_eq!(
            out,
            [
                "claude",
                "--settings",
                CONTAINER_SETTINGS,
                "--add-dir",
                "/work/src",
                "-p",
                "hi"
            ]
        );
        assert_eq!(file.as_deref(), Some(settings.as_path()));
        let err =
            rewrite_argv(&["codex".into(), settings.display().to_string()], wt.path()).unwrap_err();
        assert!(err.0.contains("host path"), "{err}");
        let codex = ["/x/codex", "exec", "--sandbox", "workspace-write", "-C"]
            .map(String::from)
            .into_iter()
            .chain([wt.path().display().to_string()])
            .collect::<Vec<_>>();
        assert_eq!(
            rewrite_argv(&codex, wt.path()).unwrap().0,
            [
                "codex",
                "exec",
                "--sandbox",
                "danger-full-access",
                "-C",
                "/work"
            ]
        );
        assert!(Args::parse(&["--id".into(), "../x".into(), "--".into(), "c".into()]).is_err());
        let bad_node = ["--id", "w-1", "--node", "x-1 --force", "--", "c"].map(String::from);
        assert!(Args::parse(&bad_node).is_err());
    }
}
