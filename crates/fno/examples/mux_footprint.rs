//! Memory footprint of a private mux server at N panes and M seeded lines.
//!
//! The method follows a published one: a release build, 1, 16 or 48 shell
//! panes, each seeded with the same number of lines, a 20 s settle, then 30 s
//! of samples. It prints the median footprint of each owner group apart: the
//! server, the pane keepers, and the shells the keepers host.
//!
//! `cargo run --release -p fno --example mux_footprint -- --panes 16 --lines 1000`
//! Flags: `--fno <bin>` (default `fno`), `--panes N` (16), `--lines N` (1000),
//! `--size COLSxROWS` (80x24), `--scrollback N` (sets `FNO_MUX_SCROLLBACK_LINES`
//! for the server).
//!
//! macOS reads physical footprint (`ri_phys_footprint`, what Activity Monitor
//! shows); other hosts read RSS. Exit codes: 0 measured, 1 a step failed (the
//! message names it), 2 a bad flag.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fno::process_admission::std_output;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_secs(20);
const SAMPLE: Duration = Duration::from_secs(30);

struct Args {
    bin: String,
    panes: usize,
    lines: usize,
    size: String,
    scrollback: Option<usize>,
}

fn number(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<usize, String> {
    it.next()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .ok_or_else(|| format!("{flag} needs a positive number"))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        bin: "fno".into(),
        panes: 16,
        lines: 1000,
        size: "80x24".into(),
        scrollback: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--fno" => args.bin = it.next().ok_or("--fno needs a path")?,
            "--panes" => args.panes = number(&mut it, "--panes")?,
            "--lines" => args.lines = number(&mut it, "--lines")?,
            "--scrollback" => args.scrollback = Some(number(&mut it, "--scrollback")?),
            "--size" => {
                let size = it.next().ok_or("--size needs COLSxROWS")?;
                match size.split_once('x') {
                    Some((c, r)) if c.parse::<u16>().is_ok() && r.parse::<u16>().is_ok() => {
                        args.size = size
                    }
                    _ => return Err(format!("--size needs COLSxROWS, got {size:?}")),
                }
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

/// A scratch mux: its own dir, socket and home, never the live fleet. Every
/// process it signals was found under its own dir.
struct IsolatedMux {
    dir: PathBuf,
    bin: String,
    scrollback: Option<usize>,
    server: Option<Child>,
}

impl IsolatedMux {
    fn new(bin: &str, scrollback: Option<usize>) -> Result<IsolatedMux, String> {
        // A short path: the session socket lives here and sun_path is small.
        let dir = PathBuf::from(format!("/tmp/fmf-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
        // zsh stops to ask for setup when a home has no rc file.
        std::fs::write(dir.join("home/.zshrc"), "PS1='$ '\n").map_err(|e| e.to_string())?;
        Ok(IsolatedMux {
            dir,
            bin: bin.to_string(),
            scrollback,
            server: None,
        })
    }

    fn env(&self) -> Vec<(String, String)> {
        let d = |leaf: &str| self.dir.join(leaf).to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> = vec![
            ("HOME".into(), d("home")),
            ("FNO_MUX_DIR".into(), d("")),
            ("FNO_AGENTS_HOME".into(), d("iso-agents")),
            ("FNO_CLAUDE_DAEMON_DIR".into(), d("iso-daemon")),
            ("FNO_GRAPH_JSON".into(), d("iso-graph.json")),
            ("FNO_CLAIMS_ROOT".into(), d("home")),
            (
                "FNO_GLOBAL_SETTINGS_PATH".into(),
                d("iso-cfg/settings.json"),
            ),
            (
                "FNO_MUX_ADMISSION_NAMESPACE".into(),
                format!("fmf{}", std::process::id()),
            ),
            ("FNO_PROCESS_ADMISSION_MAX".into(), "512".into()),
            ("FNO_E2E".into(), "1".into()),
            ("FNO_BIN".into(), self.bin.clone()),
            ("SHELL".into(), "/bin/zsh".into()),
            ("TERM".into(), "xterm-256color".into()),
        ];
        if let Some(n) = self.scrollback {
            env.push(("FNO_MUX_SCROLLBACK_LINES".into(), n.to_string()));
        }
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        for (key, _) in std::env::vars() {
            if key.starts_with("FNO_") {
                cmd.env_remove(key);
            }
        }
        cmd.args(args)
            .envs(self.env())
            .current_dir(&self.dir)
            .stdin(Stdio::null());
        cmd
    }

    fn output(&self, args: &[&str]) -> Result<String, String> {
        let out = std_output(&mut self.command(args)).map_err(|e| format!("fno {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "fno {args:?}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn start_server(&mut self) -> Result<u32, String> {
        let sock = self.dir.join("main.sock");
        let log = std::fs::File::create(self.dir.join("server.log")).map_err(|e| e.to_string())?;
        let mut cmd = self.command(&["--server"]);
        cmd.arg(&sock).stdout(Stdio::null()).stderr(log);
        let child = cmd
            .spawn()
            .map_err(|e| format!("cannot spawn {}: {e}", self.bin))?;
        let pid = child.id();
        self.server = Some(child);
        let deadline = Instant::now() + READY_TIMEOUT;
        while UnixStream::connect(&sock).is_err() {
            if let Some(Ok(Some(status))) = self.server.as_mut().map(Child::try_wait) {
                return Err(format!(
                    "the server exited {status} before it bound; see {}",
                    self.dir.join("server.log").display()
                ));
            }
            if Instant::now() > deadline {
                return Err(format!("the server never bound {}", sock.display()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(pid)
    }

    /// Every process under this mux's dir: `(pid, ppid, command)`.
    fn processes(&self) -> Vec<(u32, u32, String)> {
        let Ok(out) = std_output(Command::new("ps").args(["-axo", "pid=,ppid=,command="])) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let pid = parts.next()?.parse().ok()?;
                let ppid = parts.next()?.parse().ok()?;
                Some((pid, ppid, parts.collect::<Vec<_>>().join(" ")))
            })
            .collect()
    }

    /// The keeper pids (one per pane) and the shells they host.
    fn pane_processes(&self) -> (Vec<u32>, Vec<u32>) {
        let marker = format!("--pane --sock {}", self.dir.join("panes").display());
        let all = self.processes();
        let keepers: Vec<u32> = all
            .iter()
            .filter(|(_, _, cmd)| cmd.contains(&marker))
            .map(|(pid, _, _)| *pid)
            .collect();
        let shells = all
            .iter()
            .filter(|(_, ppid, _)| keepers.contains(ppid))
            .map(|(pid, _, _)| *pid)
            .collect();
        (keepers, shells)
    }
}

impl Drop for IsolatedMux {
    fn drop(&mut self) {
        // Keepers outlive the server by design, so end them by pid.
        let (keepers, shells) = self.pane_processes();
        let _ = std_output(&mut self.command(&["mux", "kill-server", "main", "--end-unkept"]));
        let doomed: Vec<u32> = keepers.into_iter().chain(shells).collect();
        for pid in &doomed {
            unsafe { libc::kill(*pid as i32, libc::SIGTERM) };
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && doomed
                .iter()
                .any(|pid| unsafe { libc::kill(*pid as i32, 0) } == 0)
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        for pid in &doomed {
            unsafe { libc::kill(*pid as i32, libc::SIGKILL) };
        }
        if let Some(mut server) = self.server.take() {
            let _ = server.kill();
            let _ = server.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Bytes the kernel charges this pid: physical footprint on macOS.
#[cfg(target_os = "macos")]
fn footprint(pid: u32) -> Option<u64> {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as i32,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        )
    };
    (rc == 0).then_some(info.ri_phys_footprint)
}

/// Bytes the kernel charges this pid: resident set size off macOS.
#[cfg(not(target_os = "macos"))]
fn footprint(pid: u32) -> Option<u64> {
    let out = std_output(Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()])).ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb * 1024)
}

const METRIC: &str = if cfg!(target_os = "macos") {
    "phys_footprint"
} else {
    "rss"
};

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

/// Wait until a pane's last lines hold `needle` as a whole line.
fn wait_for_line(mux: &IsolatedMux, pane: &str, needle: &str) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let screen = mux.output(&["mux", "pane", "read", pane, "--lines", "4"])?;
        if screen.lines().any(|line| line.trim() == needle) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("pane {pane} never printed {needle:?}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn step<T>(name: &str, result: Result<T, String>) -> Result<T, String> {
    result.map_err(|e| format!("step {name}: {e}"))
}

fn run(args: &Args) -> Result<(), String> {
    let mut mux = step("prepare", IsolatedMux::new(&args.bin, args.scrollback))?;
    let server = step("start-server", mux.start_server())?;
    let mut panes = Vec::with_capacity(args.panes);
    for _ in 0..args.panes {
        let id = step(
            "add-pane",
            mux.output(&[
                "mux", "pane", "run", "--size", &args.size, "--", "/bin/zsh", "-l",
            ]),
        )?;
        panes.push(id);
    }
    // The printed word differs from the typed one, so the echo of the typed
    // line cannot satisfy the wait.
    let seed = format!("seq 1 {}; printf 'fmf-%s\\n' seeded\r", args.lines);
    for pane in &panes {
        step(
            "seed",
            mux.output(&[
                "mux", "pane", "send", pane, "--text", &seed, "--raw", "--source", "fixture",
            ]),
        )?;
    }
    for pane in &panes {
        step("seed", wait_for_line(&mux, pane, "fmf-seeded"))?;
    }
    std::thread::sleep(SETTLE);
    let (keepers, shells) = mux.pane_processes();
    if keepers.len() != args.panes {
        return Err(format!(
            "step find-keepers: found {} keepers for {} panes",
            keepers.len(),
            args.panes
        ));
    }
    let groups: [(&str, Vec<u32>); 3] = [
        ("server", vec![server]),
        ("keepers", keepers),
        ("shells", shells),
    ];
    let mut samples: [Vec<u64>; 3] = Default::default();
    let started = Instant::now();
    while started.elapsed() < SAMPLE {
        for ((name, pids), taken) in groups.iter().zip(samples.iter_mut()) {
            let mut sum = 0;
            for pid in pids {
                sum += footprint(*pid)
                    .ok_or_else(|| format!("step sample: no {METRIC} for {name} pid {pid}"))?;
            }
            taken.push(sum);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(
        "panes={} lines={} size={} scrollback={} metric={METRIC} samples={}",
        args.panes,
        args.lines,
        args.size,
        args.scrollback
            .map_or_else(|| "default".to_string(), |n| n.to_string()),
        samples[0].len()
    );
    for ((name, pids), taken) in groups.iter().zip(samples.iter_mut()) {
        let mb = median(taken) as f64 / (1024.0 * 1024.0);
        println!("group={name} pids={} median_mb={mb:.1}", pids.len());
    }
    drop(mux);
    Ok(())
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("mux_footprint: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(&args) {
        eprintln!("mux_footprint: {error}");
        std::process::exit(1);
    }
}
