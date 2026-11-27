//! The container engine: docker or podman, each call under a timeout. A
//! devcontainer is only an image source; it runs on docker. apple/container
//! has no passing receipt for no network, exec stdio and dropped privileges
//! together, so it refuses by name.

use super::{Refusal, Res};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::{LABEL, LABEL_PID};

const CALL_BOUND_S: u64 = 120;
const BUILD_BOUND_S: u64 = 1800;
/// The embedded recipe. CI and a dev build pass a local binary directory.
pub const RECIPE: &str = include_str!("../../assets/sandbox/Dockerfile");

/// The configured provider. `None` runs the worker on the host, as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    None,
    Docker,
    Podman,
    Apple,
    Devcontainer,
}

impl Provider {
    pub const VALUES: [&'static str; 5] = ["none", "docker", "podman", "apple", "devcontainer"];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "none" => Some(Self::None),
            "docker" => Some(Self::Docker),
            "podman" => Some(Self::Podman),
            "apple" => Some(Self::Apple),
            "devcontainer" => Some(Self::Devcontainer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::Apple => "apple",
            Self::Devcontainer => "devcontainer",
        }
    }

    /// The engine that runs this provider, or the refusal that names why
    /// none can.
    pub fn engine(self) -> Res<Engine> {
        match self {
            Self::None => Err("[sandbox] provider = \"none\" runs no sandbox".into()),
            Self::Docker | Self::Devcontainer => Ok(Engine {
                bin: "docker",
                host: None,
            }),
            Self::Podman => Ok(Engine {
                bin: "podman",
                host: None,
            }),
            Self::Apple => Err(
                "apple/container has no passing sandbox journey for no network, exec stdio \
                 and dropped privileges; use provider docker (OrbStack or Docker Desktop) or podman"
                    .into(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    pub bin: &'static str,
    /// A remote daemon on the user's own VM, `ssh://[user@]host[:port]`.
    /// Every call carries it, so the engine CLI moves the argv over its own
    /// API and no remote shell quotes anything.
    pub host: Option<&'static str>,
}

/// A checked `sandbox.host` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl SshHost {
    /// `ssh://[user@]host[:port]`, with only `[A-Za-z0-9._-]` in the user
    /// and host, no leading dash, and a port in 1..=65535. Anything else
    /// refuses and names the value.
    pub fn parse(raw: &str) -> Res<Self> {
        let bad = || {
            Refusal(format!(
                "sandbox.host = {:?} is not ssh://[user@]host[:port] with plain characters",
                super::escape(raw, 120)
            ))
        };
        let rest = raw.strip_prefix("ssh://").ok_or_else(bad)?;
        let (user, hostport) = match rest.split_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, rest),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h,
                Some(p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(bad)?),
            ),
            None => (hostport, None),
        };
        let plain = |s: &str| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.len() <= 253
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        };
        if !plain(host) || user.is_some_and(|u| !plain(u)) {
            return Err(bad());
        }
        Ok(Self {
            user: user.map(str::to_string),
            host: host.to_string(),
            port,
        })
    }

    /// The destination ssh reads: `[user@]host`.
    pub fn dest(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// Refuse a host the user has not trusted, or an ssh config that would
    /// forward the agent or a port to the VM. Run once before first use.
    pub fn preflight(&self) -> Res<()> {
        let known = match self.port {
            Some(p) if p != 22 => format!("[{}]:{p}", self.host),
            _ => self.host.clone(),
        };
        let mut c = Command::new("ssh-keygen");
        c.args(["-F", &known]);
        let trusted = crate::bounded_cmd::output_with_timeout_result(c, 30)
            .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
        if !trusted {
            return Err(Refusal(format!(
                "sandbox.host {} is not in known_hosts; connect once with ssh and check its key",
                self.host
            )));
        }
        let mut c = Command::new("ssh");
        c.arg("-G");
        if let Some(p) = self.port {
            c.args(["-p", &p.to_string()]);
        }
        c.arg("--").arg(self.dest());
        let out = crate::bounded_cmd::output_with_timeout_result(c, 30)
            .map_err(|e| format!("ssh -G {}: {e}", self.host))?;
        let config = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        if let Some(line) = forwarding(&config) {
            return Err(Refusal(format!(
                "the ssh config for {} sets `{line}`; a sandbox host gets no agent or port forward",
                self.host
            )));
        }
        Ok(())
    }
}

/// The first `ssh -G` line that forwards the agent or a port, if any.
pub fn forwarding(config: &str) -> Option<String> {
    config
        .lines()
        .map(str::trim)
        .find(|l| {
            *l == "forwardagent yes"
                || l.starts_with("localforward ")
                || l.starts_with("remoteforward ")
                || l.starts_with("dynamicforward ")
        })
        .map(str::to_string)
}

impl Engine {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.bin);
        if let Some(h) = self.host {
            let flag = if self.bin == "podman" { "--url" } else { "-H" };
            c.args([flag, h]);
        }
        c.args(args);
        c
    }

    /// This engine against a remote daemon, or the local one for `None`.
    pub fn on(self, host: Option<&'static str>) -> Self {
        Self { host, ..self }
    }

    /// One command inside with its exit status kept, under `secs`.
    pub fn exec_raw(&self, name: &str, argv: &[&str], secs: u64) -> std::io::Result<Output> {
        let mut args = vec!["exec", "-w", super::CONTAINER_WORK, name];
        args.extend_from_slice(argv);
        crate::bounded_cmd::output_with_timeout_result(self.cmd(&args), secs)
    }

    /// One bounded call. A non-zero exit is an error naming the call and the
    /// engine's own stderr, escaped.
    pub fn call(&self, args: &[&str]) -> Res<Output> {
        self.call_bounded(args, CALL_BOUND_S)
    }

    fn call_bounded(&self, args: &[&str], secs: u64) -> Res<Output> {
        let what = format!("{} {}", self.bin, args.first().copied().unwrap_or(""));
        let out = crate::bounded_cmd::output_with_timeout_result(self.cmd(args), secs)
            .map_err(|e| format!("{what}: {e}"))?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(Refusal(format!(
                "{what} exited {}: {}",
                out.status
                    .code()
                    .map_or("by signal".into(), |c| c.to_string()),
                super::escape(String::from_utf8_lossy(&out.stderr).trim(), 400)
            )))
        }
    }

    /// The engine answers at all. Run before anything else so a missing
    /// engine refuses by name, never falls back to the host.
    pub fn probe(&self) -> Res<()> {
        self.call(&["version", "--format", "{{.Server.Version}}"])
            .map(|_| ())
    }

    pub fn image_present(&self, image: &str) -> bool {
        self.call(&["image", "inspect", image]).is_ok()
    }

    /// Build `tag` from `dockerfile` with `context`, under the build bound.
    pub fn build(
        &self,
        tag: &str,
        dockerfile: &Path,
        context: &Path,
        args: &[(&str, String)],
    ) -> Res<()> {
        let mut argv: Vec<String> = vec!["build".into(), "-t".into(), tag.into(), "-f".into()];
        argv.push(dockerfile.display().to_string());
        for (k, v) in args {
            argv.push("--build-arg".into());
            argv.push(format!("{k}={v}"));
        }
        argv.push(context.display().to_string());
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        self.call_bounded(&refs, BUILD_BOUND_S).map(|_| ())
    }

    /// Start the container: non-root, no network, no capabilities, no new
    /// privileges, no bind, no host home and no engine socket. It idles until
    /// the relay execs into it.
    pub fn start(&self, name: &str, image: &str) -> Res<()> {
        let label = format!("{LABEL}={name}");
        let pid = format!("{LABEL_PID}={}", std::process::id());
        let args = run_argv(name, image, &label, &pid);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.call(&refs).map(|_| ())
    }

    /// `exec -i` with piped stdio, for the guest relay stream.
    pub fn exec_piped(&self, name: &str, argv: &[&str]) -> Command {
        let mut c = self.cmd(&["exec", "-i", "-w", super::CONTAINER_WORK, name]);
        c.args(argv);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        c
    }

    /// `exec -i` with the caller's stdio, for the harness.
    pub fn exec_inherit(&self, name: &str, argv: &[String]) -> Command {
        let mut c = self.cmd(&["exec", "-i", "-w", super::CONTAINER_WORK, name]);
        c.args(argv);
        c
    }

    /// One command inside, its output kept.
    pub fn exec_out(&self, name: &str, argv: &[&str]) -> Res<Output> {
        let mut args = vec!["exec", "-w", super::CONTAINER_WORK, name];
        args.extend_from_slice(argv);
        self.call(&args)
    }

    pub fn copy_in(&self, name: &str, host: &Path, inside: &str) -> Res<()> {
        let dest = format!("{name}:{inside}");
        self.call(&["cp", &host.display().to_string(), &dest])
            .map(|_| ())
    }

    pub fn copy_out(&self, name: &str, inside: &str, host: &Path) -> Res<()> {
        let src = format!("{name}:{inside}");
        self.call(&["cp", &src, &host.display().to_string()])
            .map(|_| ())
    }

    pub fn remove(&self, name: &str) -> Res<()> {
        self.call(&["rm", "-f", name]).map(|_| ())
    }

    /// Labelled containers and the wrapper pid each records.
    pub fn labelled(&self) -> Vec<(String, Option<u32>)> {
        let filter = format!("label={LABEL}");
        let format = format!("{{{{.Names}}}}\t{{{{.Label \"{LABEL_PID}\"}}}}");
        let Ok(out) = self.call(&["ps", "-a", "--filter", &filter, "--format", &format]) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (name, pid) = l.split_once('\t').unwrap_or((l, ""));
                let name = name.trim();
                super::valid_id(name).then(|| (name.to_string(), pid.trim().parse().ok()))
            })
            .collect()
    }
}

/// The run argv. Measured on docker 28.0.4 and podman 4.9.3 (both accept
/// every flag here); `--userns=keep-id` is podman-only, so it stays out.
pub fn run_argv(name: &str, image: &str, label: &str, pid: &str) -> Vec<String> {
    [
        "run",
        "-d",
        "--name",
        name,
        "--label",
        label,
        "--label",
        pid,
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--user",
        "1000:1000",
        "--pids-limit",
        "1024",
        "--memory",
        "4g",
        "--init",
        "--entrypoint",
        "sleep",
        image,
        "infinity",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Keys a devcontainer.json may not carry: compose runs services on the host
/// engine, and `initializeCommand` runs on the host at every build and up.
pub const REFUSED_KEYS: [&str; 2] = ["dockerComposeFile", "initializeCommand"];

/// What a devcontainer image source resolves to.
#[derive(Debug, PartialEq, Eq)]
pub enum DevcontainerImage {
    Pull(String),
    Build {
        dockerfile: PathBuf,
        context: PathBuf,
    },
}

/// Read a devcontainer.json exported from the default branch. JSONC line and
/// block comments are stripped; a refused key refuses by name.
/// The devcontainer.json fields the sandbox reads. The refused keys are kept
/// as raw values only so their presence refuses.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevcontainerFile {
    image: Option<String>,
    build: Option<DevcontainerBuild>,
    docker_file: Option<String>,
    docker_compose_file: Option<serde_json::Value>,
    initialize_command: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct DevcontainerBuild {
    dockerfile: Option<String>,
    context: Option<String>,
}

pub fn devcontainer_image(file: &Path, root: &Path) -> Res<DevcontainerImage> {
    let raw = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let doc: DevcontainerFile =
        serde_json::from_str(&strip_jsonc(&raw)).map_err(|e| format!("{}: {e}", file.display()))?;
    let present = [
        doc.docker_compose_file.is_some(),
        doc.initialize_command.is_some(),
    ];
    if let Some((key, _)) = REFUSED_KEYS.iter().zip(present).find(|(_, set)| *set) {
        return Err(Refusal(format!(
            "{} sets `{key}`, which the sandbox refuses: it runs on the host",
            file.display()
        )));
    }
    let dir = file.parent().unwrap_or(Path::new("."));
    if let Some(image) = doc.image {
        return Ok(DevcontainerImage::Pull(image));
    }
    let (dockerfile, context) = match doc.build {
        Some(b) => (b.dockerfile.or(doc.docker_file), b.context),
        None => (doc.docker_file, None),
    };
    let dockerfile = dockerfile.ok_or_else(|| {
        format!(
            "{} names neither `image` nor `build.dockerfile`",
            file.display()
        )
    })?;
    let context = context.unwrap_or_else(|| ".".to_string());
    let (dockerfile, context) = (dir.join(dockerfile), dir.join(context));
    // The export folder is the boundary: a path that climbs out refuses.
    let inside = |p: &Path| lexical(p).starts_with(lexical(root));
    if !inside(&dockerfile) || !inside(&context) {
        return Err(Refusal(format!(
            "{} points its build outside the repository",
            file.display()
        )));
    }
    Ok(DevcontainerImage::Build {
        dockerfile,
        context,
    })
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn strip_jsonc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run argv carries every isolation flag and no bind or socket; a
    /// devcontainer with a host key refuses by key name; JSONC comments and
    /// an image source read through.
    #[test]
    fn run_argv_isolates_and_devcontainer_refuses_host_keys() {
        let argv = run_argv("w-1", "img", "l", "p").join(" ");
        for flag in [
            "--network none",
            "--cap-drop ALL",
            "no-new-privileges",
            "--user 1000:1000",
        ] {
            assert!(argv.contains(flag), "{flag}: {argv}");
        }
        for bad in ["-v ", "--volume", "--mount", "docker.sock", "--privileged"] {
            assert!(!argv.contains(bad), "{bad}: {argv}");
        }
        assert!(Provider::Apple
            .engine()
            .unwrap_err()
            .0
            .contains("apple/container"));
        assert_eq!(
            Provider::parse("podman").map(Provider::as_str),
            Some("podman")
        );
        assert_eq!(Provider::parse("nope"), None);
        // AC9-EDGE: an unsafe host value refuses; a forwarding config names
        // the line; a remote engine carries the host on every call.
        for bad in [
            "host",
            "ssh://-oProxyCommand=x",
            "ssh://a b",
            "ssh://u@h:0",
            "ssh://u@h:99999",
            "ssh://h;rm",
            "ssh://$(x)",
        ] {
            assert!(SshHost::parse(bad).is_err(), "{bad}");
        }
        let h = SshHost::parse("ssh://runner@localhost:2222").unwrap();
        assert_eq!((h.dest(), h.port), ("runner@localhost".into(), Some(2222)));
        assert_eq!(
            forwarding("user x\nforwardagent yes\n").as_deref(),
            Some("forwardagent yes")
        );
        assert_eq!(forwarding("forwardagent no\nport 22\n"), None);
        let remote = Engine {
            bin: "podman",
            host: Some("ssh://u@h"),
        };
        let argv: Vec<String> = remote
            .cmd(&["ps"])
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, ["--url", "ssh://u@h", "ps"]);
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("devcontainer.json");
        std::fs::write(&f, "{ // c\n \"image\": \"debian:12\" /* x */ }").unwrap();
        assert_eq!(
            devcontainer_image(&f, dir.path()).unwrap(),
            DevcontainerImage::Pull("debian:12".into())
        );
        for key in REFUSED_KEYS {
            std::fs::write(&f, format!("{{\"image\": \"a\", \"{key}\": \"touch /x\"}}")).unwrap();
            assert!(devcontainer_image(&f, dir.path())
                .unwrap_err()
                .0
                .contains(key));
        }
    }
}
