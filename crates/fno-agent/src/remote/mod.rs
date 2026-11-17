//! The remote door: a paired phone on the same tailnet reads the fleet
//! (`agent.list`, `agent.watch`, `agent.status`, `agent.truth`) from the
//! daemon on this Mac. Rulings D2503 (Noise IK, QR pairing, allow list),
//! D2513 (tailnet only, no relay) and D2524 (the same allow list). The wire
//! spec a phone client implements is docs/architecture/remote-door.md.
//!
//! People reach these commands as `fno agent workspace remote <command>`;
//! the binary answers `fno-agent remote <command>`.

pub mod allow;
pub mod channel;
pub mod door;
pub mod keeper;
pub mod pairing;

use std::net::SocketAddr;
use std::process::Command;

use clap::{Parser, Subcommand};

use crate::paths::AgentsHome;

#[derive(Parser)]
#[command(
    name = "fno agent workspace remote",
    about = "The tailnet door a paired phone reads the fleet through."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the door on 127.0.0.1; `tailscale serve` puts it on the tailnet.
    Serve {
        #[arg(long, default_value_t = door::DEFAULT_PORT)]
        port: u16,
        /// Install a launchd agent that keeps the door up, then exit.
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        /// Remove that launchd agent, then exit.
        #[arg(long)]
        uninstall: bool,
    },
    /// Print a one-time pairing offer (URL and QR) a phone scans.
    Pair {
        /// The door's port; defaults to the running door's, else 8725.
        #[arg(long)]
        port: Option<u16>,
        /// The host in the offer; defaults to this machine's tailnet name.
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value_t = 10)]
        ttl_minutes: u64,
        #[arg(long)]
        json: bool,
    },
    /// List paired devices.
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Revoke a paired device by id; its key is never accepted again.
    Revoke { id: String },
    /// The running door's status file.
    Status {
        #[arg(long)]
        json: bool,
    },
}

/// This machine's tailnet DNS name from `tailscale status --json`
/// (`Self.DNSName`, trailing dot dropped). None when Tailscale is absent,
/// down or slow. The same read as crates/fno's phone-sink doctor; this
/// crate cannot link that one.
pub fn tailnet_dns_name() -> Option<String> {
    let mut cmd = Command::new("tailscale");
    cmd.args(["status", "--json"]);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, 5).ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let name = v.get("Self")?.get("DNSName")?.as_str()?;
    let name = name.trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_string())
}

fn read_status(home: &AgentsHome) -> Option<door::DoorStatus> {
    let bytes = std::fs::read(door::status_path(home)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `fno-agent remote <command>`.
pub fn run(args: &[String]) -> i32 {
    let argv = std::iter::once("remote".to_string()).chain(args.iter().cloned());
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(e) => {
            if let Err(err) = e.print() {
                eprintln!("fno-agent remote: {err}");
            }
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let home = AgentsHome::from_env();
    match cli.command {
        Cmd::Serve {
            port,
            install,
            uninstall,
        } => serve(&home, port, install, uninstall),
        Cmd::Pair {
            port,
            host,
            ttl_minutes,
            json,
        } => pair(&home, port, host, ttl_minutes, json),
        Cmd::Devices { json } => devices(&home, json),
        Cmd::Revoke { id } => match pairing::revoke(&home, &id, pairing::now_unix()) {
            Ok(true) => {
                println!("revoked {id}");
                0
            }
            Ok(false) => {
                eprintln!("no live device with id {id}; `remote devices` lists them");
                1
            }
            Err(e) => fail(e),
        },
        Cmd::Status { json } => status(&home, json),
    }
}

fn fail(e: impl std::fmt::Display) -> i32 {
    eprintln!("fno-agent remote: {e}");
    1
}

fn serve(home: &AgentsHome, port: u16, install: bool, uninstall: bool) -> i32 {
    if uninstall {
        return match keeper::uninstall() {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(e) => fail(e),
        };
    }
    if install {
        return match keeper::install(home, port) {
            Ok(line) => {
                println!("{line}");
                println!("Put the door on the tailnet once (it survives reboots):");
                println!("  {}", keeper::tailscale_serve_line(port));
                0
            }
            Err(e) => fail(e),
        };
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(e),
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    rt.block_on(async {
        let listener = match door::bind(addr).await {
            Ok(l) => l,
            Err(e) => return fail(e),
        };
        match door::serve(listener, home.clone(), tailnet_dns_name()).await {
            Ok(()) => 0,
            Err(e) => fail(e),
        }
    })
}

fn pair(home: &AgentsHome, port: Option<u16>, host: Option<String>, ttl: u64, json: bool) -> i32 {
    let port = port
        .or_else(|| read_status(home).map(|s| s.port))
        .unwrap_or(door::DEFAULT_PORT);
    let Some(host) = host.or_else(tailnet_dns_name) else {
        return fail("no tailnet name: is Tailscale up? Pass --host to name the door's host");
    };
    let ticket = match pairing::new_offer(home, &host, port, ttl * 60, pairing::now_unix()) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    if json {
        let v = serde_json::json!({"url": ticket.url, "expires_at": ticket.expires_at});
        println!("{v}");
        return 0;
    }
    if let Some(qr) = pairing::render_qr(&ticket.url) {
        println!("{qr}");
    }
    println!("{}", ticket.url);
    println!("One use, valid {ttl} minutes. The phone must be on this tailnet.");
    0
}

fn devices(home: &AgentsHome, json: bool) -> i32 {
    let list = match pairing::with_store(home, |s| s.devices.clone()) {
        Ok(list) => list,
        Err(e) => return fail(e),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&list).expect("devices serialize")
        );
        return 0;
    }
    if list.is_empty() {
        println!("no paired devices; `remote pair` prints an offer");
    }
    for d in list {
        let state = match d.revoked_at {
            Some(t) => format!("revoked at {t}"),
            None => "live".into(),
        };
        println!("{}  {}  paired at {}  {state}", d.id, d.name, d.paired_at);
    }
    0
}

fn status(home: &AgentsHome, json: bool) -> i32 {
    let Some(s) = read_status(home) else {
        return fail(format!(
            "no door status at {}; start it with `remote serve`",
            door::status_path(home).display()
        ));
    };
    if json {
        println!("{}", serde_json::to_string(&s).expect("status serializes"));
        return 0;
    }
    let age = pairing::now_unix().saturating_sub(s.updated_at);
    println!(
        "{} on {} (pid {}, {} device(s), beat {age}s ago)",
        s.state, s.bind, s.pid, s.devices
    );
    match s.tailnet_dns {
        Some(dns) => println!("tailnet: {dns}:{}", s.port),
        None => println!("tailnet: unknown (is Tailscale up?)"),
    }
    0
}
