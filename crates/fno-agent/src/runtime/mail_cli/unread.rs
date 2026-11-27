//! The native `mail unread` leaf for the `--mine` leg: the cursor-bounded
//! unread listing itself lives in `bus_read`, which the fno front door
//! compiles as a copy and answers in process. `--mine` needs the transcript
//! and You-thread stores, so it stays here, and the front door execs this
//! leaf for it.

use std::io::IsTerminal;
use std::path::PathBuf;

use crate::bus_read::{json_array, str_field, unread_listing};
use crate::mail_control_drain::read_cursor_file;
use fno_mail::log::read_bus_segments;

const HELP: &str = "\
usage: fno-agent mail unread [--name NAME] [-n NAME] [--mine] [--json] [-J] [--help]

Show unread bus messages addressed to NAME (cursor-filtered): the bus log
scanned oldest-first, rows addressed to NAME after its ack cursor. With
--mine, print this session's You pairs instead (typed turn + reply).

options:
  -n, --name NAME  whose inbox to read (registry name or project; default fno)
  --mine           print this session's You pairs (typed + reply) instead
  -J, --json       emit JSON regardless of TTY
  -h, --help       print this help
";

pub fn run(args: &[String]) -> i32 {
    let mut name = String::from("fno");
    let mut json_out = false;
    let mut mine_only = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(value) = arg.strip_prefix("--name=") {
            name = value.to_string();
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
            "--name" | "-n" => match it.next() {
                Some(value) => name = value.clone(),
                None => {
                    eprintln!("unread: --name wants a value");
                    return 2;
                }
            },
            "--mine" => {
                mine_only = true;
            }
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("unread: unknown argument {other:?}");
                return 2;
            }
        }
    }
    if mine_only {
        return super::you_pairs::run(json_out);
    }
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
        eprintln!("unread: invalid cursor name: {name:?}");
        return 2;
    }

    let bus = crate::mail_paths::bus_dir_of(&state_root());
    let msgs = read_bus_segments(&bus.join("messages.jsonl"));
    let (cursor, warnings) = read_cursor_file(&bus.join("cursors").join(format!("{name}.json")));
    for warning in &warnings {
        eprintln!("{warning}");
    }
    let unread = unread_listing(&msgs, &name, cursor.as_deref());

    if json_out || !std::io::stdout().is_terminal() {
        println!("{}", json_array(&unread));
        return 0;
    }
    if unread.is_empty() {
        println!("inbox empty for {name:?} (no unread bus messages)");
        return 0;
    }
    for m in &unread {
        let excerpt: String = str_field(m, "body")
            .replace('\n', " ")
            .chars()
            .take(100)
            .collect();
        println!(
            "{}  {} -> {}  [{}]  {}",
            str_field(m, "id"),
            str_field(m, "from"),
            str_field(m, "to"),
            str_field(m, "kind"),
            excerpt
        );
    }
    println!();
    println!("to answer one: fno agent mail reply --to <id> --body \"...\"");
    0
}

/// The state root behind the Python `config.state_dir` default:
/// `$FNO_STATE_DIR`, else `$HOME/.fno`.
fn state_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".fno"))
        .unwrap_or_else(|| PathBuf::from(".fno"))
}
