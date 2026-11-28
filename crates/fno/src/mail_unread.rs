//! `fno agent mail unread`, answered in process: the cursor-bounded unread
//! listing over the bus log. The listing rules are `bus_read`, the copy of the
//! module fno-agent's own leaf and drain share, so the two cannot disagree.
//! `--mine` reads the session transcript and the You-thread store, which only
//! fno-agent owns, so that one flag returns `None` and the caller execs the
//! sibling binary.

use std::io::IsTerminal;

use crate::bus_read::{json_array, read_cursor_file, str_field, unread_listing};
use crate::config_doctor::{bus_dir, Settings};
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

/// What the argv asks for, or the exit code a bad argv ends with.
enum Parsed {
    Help,
    Mine,
    Read { name: String, json: bool },
    Refused(i32),
}

fn parse(args: &[String]) -> Parsed {
    let mut name = String::from("fno");
    let mut json = false;
    let mut mine = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(value) = arg.strip_prefix("--name=") {
            name = value.to_string();
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return Parsed::Help,
            "--name" | "-n" => match it.next() {
                Some(value) => name = value.clone(),
                None => {
                    eprintln!("unread: --name wants a value");
                    return Parsed::Refused(2);
                }
            },
            "--mine" => mine = true,
            "--json" | "-J" => json = true,
            other => {
                eprintln!("unread: unknown argument {other:?}");
                return Parsed::Refused(2);
            }
        }
    }
    if mine {
        return Parsed::Mine;
    }
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
        eprintln!("unread: invalid cursor name: {name:?}");
        return Parsed::Refused(2);
    }
    Parsed::Read { name, json }
}

/// Run the leaf. `None` means the argv needs fno-agent (`--mine`).
pub fn run(args: &[String]) -> Option<i32> {
    match parse(args) {
        Parsed::Mine => None,
        Parsed::Refused(code) => Some(code),
        Parsed::Help => {
            print!("{HELP}");
            Some(0)
        }
        Parsed::Read { name, json } => Some(list(&name, json)),
    }
}

fn list(name: &str, json_out: bool) -> i32 {
    let bus = bus_dir(&Settings::load());
    let msgs = read_bus_segments(&bus.join("messages.jsonl"));
    let (cursor, warnings) = read_cursor_file(&bus.join("cursors").join(format!("{name}.json")));
    for warning in &warnings {
        eprintln!("{warning}");
    }
    let unread = unread_listing(&msgs, name, cursor.as_deref());

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
