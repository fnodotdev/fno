//! The container engine: docker or podman, each call under a timeout. A
//! devcontainer is only an image source; it runs on docker. apple/container
//! has no passing receipt for no network, exec stdio and dropped privileges
//! together, so it refuses by name.

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
        Self::VALUES[self as usize]
    }

    /// The engine that runs this provider, or the refusal that names why
    /// none can.
    pub fn engine(self) -> Result<Engine, String> {
        match self {
            Self::None => Err("[sandbox] provider = \"none\" runs no sandbox".into()),
            Self::Docker | Self::Devcontainer => Ok(Engine { bin: "docker" }),
            Self::Podman => Ok(Engine { bin: "podman" }),
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
}

impl Engine {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.bin);
        c.args(args);
        c
    }

    /// One bounded call. A non-zero exit is an error naming the call and the
    /// engine's own stderr, escaped.
    pub fn call(&self, args: &[&str]) -> Result<Output, String> {
        self.call_bounded(args, CALL_BOUND_S)
    }

    fn call_bounded(&self, args: &[&str], secs: u64) -> Result<Output, String> {
        let what = format!("{} {}", self.bin, args.first().copied().unwrap_or(""));
        let out = crate::bounded_cmd::output_with_timeout_result(self.cmd(args), secs)
            .map_err(|e| format!("{what}: {e}"))?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(format!(
                "{what} exited {}: {}",
                out.status
                    .code()
                    .map_or("by signal".into(), |c| c.to_string()),
                super::escape(String::from_utf8_lossy(&out.stderr).trim(), 400)
            ))
        }
    }

    /// The engine answers at all. Run before anything else so a missing
    /// engine refuses by name, never falls back to the host.
    pub fn probe(&self) -> Result<(), String> {
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
    ) -> Result<(), String> {
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
    pub fn start(&self, name: &str, image: &str) -> Result<(), String> {
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
    pub fn exec_out(&self, name: &str, argv: &[&str]) -> Result<Output, String> {
        let mut args = vec!["exec", "-w", super::CONTAINER_WORK, name];
        args.extend_from_slice(argv);
        self.call(&args)
    }

    pub fn copy_in(&self, name: &str, host: &Path, inside: &str) -> Result<(), String> {
        let dest = format!("{name}:{inside}");
        self.call(&["cp", &host.display().to_string(), &dest])
            .map(|_| ())
    }

    pub fn copy_out(&self, name: &str, inside: &str, host: &Path) -> Result<(), String> {
        let src = format!("{name}:{inside}");
        self.call(&["cp", &src, &host.display().to_string()])
            .map(|_| ())
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
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
pub fn devcontainer_image(file: &Path, root: &Path) -> Result<DevcontainerImage, String> {
    let raw = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&strip_jsonc(&raw)).map_err(|e| format!("{}: {e}", file.display()))?;
    for key in REFUSED_KEYS {
        if doc.get(key).is_some() {
            return Err(format!(
                "{} sets `{key}`, which the sandbox refuses: it runs on the host",
                file.display()
            ));
        }
    }
    let dir = file.parent().unwrap_or(Path::new("."));
    if let Some(image) = doc.get("image").and_then(|v| v.as_str()) {
        return Ok(DevcontainerImage::Pull(image.to_string()));
    }
    let build = doc.get("build");
    let dockerfile = build
        .and_then(|b| b.get("dockerfile"))
        .or_else(|| doc.get("dockerFile"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "{} names neither `image` nor `build.dockerfile`",
                file.display()
            )
        })?;
    let context = build
        .and_then(|b| b.get("context"))
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let (dockerfile, context) = (dir.join(dockerfile), dir.join(context));
    // The export folder is the boundary: a `..` path out of it refuses.
    // The export folder is the boundary: a path that climbs out refuses.
    let inside = |p: &Path| lexical(p).starts_with(lexical(root));
    if !inside(&dockerfile) || !inside(&context) {
        return Err(format!(
            "{} points its build outside the repository",
            file.display()
        ));
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
            .contains("apple/container"));
        assert_eq!(
            Provider::parse("podman").map(Provider::as_str),
            Some("podman")
        );
        assert_eq!(Provider::parse("nope"), None);
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
                .contains(key));
        }
    }
}
