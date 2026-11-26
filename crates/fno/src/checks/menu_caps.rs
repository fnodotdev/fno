//! `fno doctor lint menu-caps`: the first three levels of every fno binary
//! hold at most twelve commands each (thirteen at a root), and a hidden command counts. Hiding is not
//! folding, so the guard never reads a visibility cell.
//!
//! It is a pure read over committed files, so it needs no Python and no
//! running binary:
//! - `scripts/ci/command-collapse-map.tsv`: every leaf of a collapsed Python
//!   group, hidden included (the command ratchet refuses drift from the code);
//! - `scripts/ci/command-baseline.txt`: every other Python leaf (`inbox` and
//!   `whoami` are in no collapsed group, so the map alone misses them);
//! - `scripts/ci/native-command-tree.txt`: the typed clap tree;
//! - `crates/fno/command-names.tsv`, compiled in: the root is its live rows
//!   alone, so a Python or clap path under a retired root is not counted,
//!   and its rows below the root add the names a Rust classifier claims;
//! - `accounts_route::NATIVE_ACCOUNTS_COMMANDS`, the `config accounts` door;
//! - `crates/fno-agent/src/bin/client.rs`, its doors module
//!   `bin/doors/client_doors.rs`, and the module tables in
//!   `AGENTS_DOOR_TABLES`: every argv[0] word the fno-agent binary answers,
//!   in `run` or before it, which count the same as fno's;
//! - and, in-process, the `agents` table in `agents_menu`. The group is
//!   counted from the table alone; the map's `agents` rows are the spellings
//!   the table's own test places, and no Python registers them.
//!
//! `scripts/ci/menu-caps-debt.tsv` names the groups that are over today. It
//! is shrink-only: a group over the cap needs a row, may not pass its row's
//! ceiling, may not sit below it (lower the row in the same change), and a
//! row for a group back at its cap or under must go.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

/// The cap at a binary's root: `fno` and `fno-agent`.
pub const ROOT_CAP: usize = 13;
/// The cap at the second and third levels.
pub const GROUP_CAP: usize = 12;

/// The cap for a counted group: a root holds thirteen, every other group twelve.
pub fn cap_for(path: &str) -> usize {
    if path == "(root)" || path == "fno-agent" {
        ROOT_CAP
    } else {
        GROUP_CAP
    }
}

const MAP_REL: &str = "scripts/ci/command-collapse-map.tsv";
const BASELINE_REL: &str = "scripts/ci/command-baseline.txt";
const NATIVE_REL: &str = "scripts/ci/native-command-tree.txt";
const AGENTS_SOURCE_REL: &str = "crates/fno-agent/src/bin/client.rs";
/// The binary-direct doors `run` dispatches through, split out of client.rs.
pub(crate) const AGENTS_DOORS_REL: &str = "crates/fno-agent/src/bin/doors/client_doors.rs";
/// The module tables client.rs hands argv[0] to, in `run` or before it: each
/// file and the function whose `match` is the table.
pub(crate) const AGENTS_DOOR_TABLES: &[(&str, &str)] = &[
    (
        "crates/fno-agent/src/family_doors.rs",
        "pub fn client_dispatch",
    ),
    ("crates/fno-agent/src/gate/pr_door.rs", "pub fn run_command"),
    (
        "crates/fno-agent/src/gate/review_doors.rs",
        "pub fn dispatch",
    ),
    (
        "crates/fno-agent/src/client_transport.rs",
        "pub fn transport_early_arms",
    ),
    (
        "crates/fno-agent/src/gate/pr_status/adapter.rs",
        "pub fn gh_transport_door",
    ),
    (
        "crates/fno-agent/src/runtime/worktree/mod.rs",
        "pub fn client_dispatch",
    ),
    (
        "crates/fno-agent/src/spawn/launch_workdir.rs",
        "pub fn client_dispatch",
    ),
    (
        "crates/fno-agent/src/runtime/org_board/pr_closure.rs",
        "pub fn run_command",
    ),
];
pub const DEBT_REL: &str = "scripts/ci/menu-caps-debt.tsv";

/// The fno-agent client's dispatch sources, as text.
pub struct AgentSources {
    pub client: String,
    pub doors: String,
    /// Each door table: its file, its marker, and the file's text.
    pub tables: Vec<(&'static str, &'static str, String)>,
}

impl AgentSources {
    pub fn read(root: &Path) -> Result<Self, String> {
        let mut tables = Vec::new();
        for &(rel, marker) in AGENTS_DOOR_TABLES {
            tables.push((rel, marker, read_rel(root, rel)?));
        }
        Ok(Self {
            client: read_rel(root, AGENTS_SOURCE_REL)?,
            doors: read_rel(root, AGENTS_DOORS_REL)?,
            tables,
        })
    }
}

/// The committed files the guard reads, as text.
pub struct Inputs {
    pub collapse_map: String,
    pub baseline: String,
    pub native_tree: String,
    pub agents: AgentSources,
    pub debt: String,
}

fn read_rel(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel))
        .map_err(|e| format!("menu-caps: cannot read {rel} under {}: {e}", root.display()))
}

/// Read every input under `root`; the error names the first missing file.
pub fn read_inputs(root: &Path) -> Result<Inputs, String> {
    Ok(Inputs {
        collapse_map: read_rel(root, MAP_REL)?,
        baseline: read_rel(root, BASELINE_REL)?,
        native_tree: read_rel(root, NATIVE_REL)?,
        agents: AgentSources::read(root)?,
        debt: read_rel(root, DEBT_REL)?,
    })
}

fn words(path: &str) -> Vec<String> {
    path.split_whitespace().map(str::to_string).collect()
}

/// Every Python leaf path, hidden included, under a live root.
fn python_paths(inputs: &Inputs, live: &BTreeSet<String>) -> Vec<Vec<String>> {
    let from_map = inputs
        .collapse_map
        .lines()
        .skip(1)
        .filter_map(|line| line.split('\t').next())
        .map(words);
    let from_baseline = inputs
        .baseline
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| {
            line.split_whitespace()
                .filter(|w| !w.starts_with('!'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    from_map
        .chain(from_baseline)
        .filter(|p| !p.is_empty())
        // The baseline names the sibling binary as one leaf; it is its own tree.
        .filter(|p| p.as_slice() != ["fno-agent"])
        .filter(|p| live.contains(&p[0]))
        .collect()
}

fn native_paths(native_tree: &str) -> Vec<Vec<String>> {
    native_tree
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split('\t').next())
        .filter(|path| *path != "(root)")
        .map(words)
        .collect()
}

/// The `agents` group as the menu table declares it.
fn menu_paths() -> Vec<Vec<String>> {
    crate::agents_menu::walk(&crate::agents_menu::ROOT)
        .into_iter()
        .map(|(path, _)| {
            std::iter::once("agent".to_string())
                .chain(path.iter().map(|w| w.to_string()))
                .collect()
        })
        .collect()
}

/// The source lines with `//` comments removed: doc comments narrate commands
/// that no arm dispatches.
pub(crate) fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The brace-balanced body after `marker` in `text`, the file at `rel`.
pub(crate) fn braced_region<'a>(text: &'a str, rel: &str, marker: &str) -> Result<&'a str, String> {
    let missing = || format!("menu-caps: {rel} has no dispatch marker {marker:?}");
    let start = text.find(marker).ok_or_else(missing)?;
    let opening = start + text[start..].find('{').ok_or_else(missing)?;
    let mut depth = 0usize;
    for (offset, ch) in text[opening..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&text[opening..=opening + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!(
        "menu-caps: unbalanced dispatch marker {marker:?} in {rel}"
    ))
}

/// The command literals the match opening at the first line dispatches
/// directly. Depth-scoped: a flag parser nested inside an arm spells words
/// that are not commands.
fn arms_at_top_level(lines: &[&str], arm: &Regex) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    let mut depth: i64 = 0;
    for (i, line) in lines.iter().enumerate() {
        if depth == 0 {
            commands.extend(arm.captures_iter(line).map(|c| c[1].to_string()));
        }
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if i == 0 {
            depth = (depth - 1).max(0);
        }
    }
    commands
}

/// Every argv[0] word the fno-agent binary answers, hidden ones included:
/// `run`'s arms, the doors module (whose whole text is dispatch arms), the
/// `main()` and pre-run argv doors, and each `AGENTS_DOOR_TABLES` match. The
/// one reading of the client's top level: the command ratchet's surface rows
/// defend each of these words by name, and the debt row caps their count.
pub(crate) fn fno_agent_commands(sources: &AgentSources) -> Result<BTreeSet<String>, String> {
    let text = strip_comments(&sources.client);
    let arm = Regex::new(r#"(?:Some\(\s*)?"([a-z][a-z0-9-]*)"\s*(?:\)\s*)?(?:\||=>)"#)
        .expect("static regex");
    let literal = Regex::new(r#""([a-z][a-z0-9-]*)""#).expect("static regex");
    let mut commands = BTreeSet::new();
    let run = braced_region(&text, AGENTS_SOURCE_REL, "async fn run(")?;
    let doors = strip_comments(&sources.doors);
    let if_command = Regex::new(r#"\bif command == "([a-z][a-z0-9-]*)""#).expect("static regex");
    let matches = Regex::new(r"matches!\(\s*command\s*,([^)]*)\)").expect("static regex");
    for region in [run, doors.as_str()] {
        commands.extend(if_command.captures_iter(region).map(|c| c[1].to_string()));
        for alternatives in matches.captures_iter(region) {
            commands.extend(
                literal
                    .captures_iter(&alternatives[1])
                    .map(|c| c[1].to_string()),
            );
        }
    }
    let request = braced_region(&text, AGENTS_SOURCE_REL, "let method = match command")?;
    let lines: Vec<&str> = request.lines().collect();
    commands.extend(arms_at_top_level(&lines, &arm));

    // The argv doors outside `run`: main() and the pre-run door functions
    // test argv[0] before the runtime builds, and `|| command == "x"`
    // continues an `if command ==` arm.
    let dispatch = format!("{text}\n{doors}");
    let first_eq =
        Regex::new(r#"args\.first\(\)\.map\(String::as_str\)\s*==\s*Some\("([a-z][a-z0-9-]*)"\)"#)
            .expect("static regex");
    let first_owned =
        Regex::new(r#"args\.first\(\)\s*==\s*Some\(&"([a-z][a-z0-9-]*)"\.to_string\(\)\)"#)
            .expect("static regex");
    let continuation =
        Regex::new(r#"\|\|\s*command == "([a-z][a-z0-9-]*)""#).expect("static regex");
    for single in [&first_eq, &first_owned, &continuation] {
        commands.extend(single.captures_iter(&dispatch).map(|c| c[1].to_string()));
    }
    let first_matches =
        Regex::new(r"matches!\(\s*args\.first\(\)\.map\(String::as_str\)\s*,\s*Some\(([^)]*)\)")
            .expect("static regex");
    for alternatives in first_matches.captures_iter(&dispatch) {
        commands.extend(
            literal
                .captures_iter(&alternatives[1])
                .map(|c| c[1].to_string()),
        );
    }

    // The module tables: a `"x" =>`, `Some("x") =>` or `Some("x" | "y") =>`
    // arm. A nested purpose match (`Some(p @ ("a" | "b")) =>`) is a leaf's
    // grammar, and the closing `))` keeps it out.
    let table_arm =
        Regex::new(r#"(?:Some\()?((?:"[a-z][a-z0-9-]*"\s*\|\s*)*"[a-z][a-z0-9-]*")\)?\s*=>"#)
            .expect("static regex");
    for (rel, marker, source) in &sources.tables {
        let stripped = strip_comments(source);
        let region = braced_region(&stripped, rel, marker)?;
        for arms in table_arm.captures_iter(region) {
            commands.extend(literal.captures_iter(&arms[1]).map(|c| c[1].to_string()));
        }
        commands.extend(first_eq.captures_iter(region).map(|c| c[1].to_string()));
    }
    commands.insert("help".into());
    commands.insert("version".into());
    if !commands.contains("claim") || !commands.contains("detect") {
        return Err(format!(
            "menu-caps: the {AGENTS_SOURCE_REL} scan missed `claim` or `detect`; \
             a silent miss is the defect this scan exists to prevent"
        ));
    }
    Ok(commands)
}

/// How many distinct commands each group at the first three levels holds,
/// keyed by its printed path.
pub fn counts(inputs: &Inputs) -> Result<BTreeMap<String, usize>, String> {
    let live: BTreeSet<String> = crate::command_names::live_roots()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |path: &[String], from: usize, label: &dyn Fn(&[String]) -> String| {
        for i in from..path.len().min(3) {
            children
                .entry(label(&path[..i]))
                .or_default()
                .insert(path[i].clone());
        }
    };
    let plain = |parent: &[String]| {
        if parent.is_empty() {
            "(root)".to_string()
        } else {
            parent.join(" ")
        }
    };
    let menu = menu_paths();
    for path in &menu {
        add(path, 0, &plain);
    }
    let table = crate::command_names::live_paths()
        .into_iter()
        .map(|p| p.into_iter().map(str::to_string).collect::<Vec<_>>());
    let accounts = crate::accounts_route::NATIVE_ACCOUNTS_COMMANDS
        .iter()
        .map(|c| words(&format!("config accounts {c}")));
    for path in table.chain(accounts) {
        add(&path, 0, &plain);
    }
    for path in python_paths(inputs, &live) {
        if path.first().map(String::as_str) != Some("agent") {
            add(&path, 0, &plain);
        }
    }
    for path in native_paths(&inputs.native_tree) {
        if live.contains(&path[0]) {
            add(&path, 0, &plain);
        }
    }
    for command in fno_agent_commands(&inputs.agents)? {
        add(&[command], 0, &|_: &[String]| "fno-agent".to_string());
    }
    Ok(children.into_iter().map(|(k, v)| (k, v.len())).collect())
}

/// One debt row: the group's printed path and the count it may not pass.
fn debt_rows(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut rows = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let ceiling = cols.get(1).and_then(|c| c.trim().parse::<usize>().ok());
        match (cols.first(), ceiling) {
            (Some(path), Some(ceiling)) if !path.is_empty() => {
                rows.insert(path.to_string(), ceiling);
            }
            _ => {
                return Err(format!(
                    "menu-caps: {DEBT_REL}:{} is not `path<TAB>ceiling<TAB>note`",
                    n + 1
                ))
            }
        }
    }
    Ok(rows)
}

/// The verdict over the counts and the debt file: the ok line, or every
/// refusal, each naming the group and the fix.
pub fn check(counts: &BTreeMap<String, usize>, debt: &str) -> Result<String, Vec<String>> {
    let rows = debt_rows(debt).map_err(|e| vec![e])?;
    let mut failures = Vec::new();
    for (path, &count) in counts {
        let cap = cap_for(path);
        if count <= cap {
            continue;
        }
        match rows.get(path) {
            None => failures.push(format!(
                "`{path}` holds {count} commands (cap {cap}) and has no debt row. Fold its \
                 commands into nested groups; hiding a command does not lower the count. Only a \
                 group over the cap today may enter {DEBT_REL}, as `{path}\\t{count}\\t<note>`."
            )),
            Some(&ceiling) if count > ceiling => failures.push(format!(
                "`{path}` holds {count} commands, above its debt ceiling {ceiling} (cap \
                 {cap}). Nest the new command under a group, or remove it."
            )),
            Some(&ceiling) if count < ceiling => failures.push(format!(
                "`{path}` holds {count} commands, below its debt ceiling {ceiling}. Lower its \
                 row in {DEBT_REL} to {count} in this change, so the slack is not banked."
            )),
            Some(_) => {}
        }
    }
    for path in rows.keys() {
        let count = counts.get(path).copied().unwrap_or(0);
        let cap = cap_for(path);
        if count <= cap {
            failures.push(format!(
                "`{path}` holds {count} commands, inside the cap of {cap}. Delete its row \
                 from {DEBT_REL}."
            ));
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }
    let debt: Vec<String> = rows
        .keys()
        .map(|path| format!("{path} {}", counts[path]))
        .collect();
    Ok(format!(
        "menu-caps: ok (cap {ROOT_CAP} at a root, {GROUP_CAP} below; {} groups in debt: {})",
        debt.len(),
        if debt.is_empty() {
            "none".to_string()
        } else {
            debt.join(", ")
        }
    ))
}

/// The fno-agent words an fno root answers by its own name, with no menu
/// row: the client's front doors and the help and version every binary has.
const ROOT_SPELLED: &[&str] = &["backlog", "do", "help", "pr", "version"];

/// The client word each `fno agent` row forwards to.
pub(crate) fn menu_forwarded() -> BTreeSet<String> {
    crate::agents_menu::walk(&crate::agents_menu::ROOT)
        .into_iter()
        .filter_map(|(_, node)| node.legacy.first())
        .map(|word| crate::agents_client_route::client_word(word).to_string())
        .collect()
}

/// Every fno-agent word needs an fno spelling: an `fno agent` row that
/// forwards to it, or an fno root that answers it by name. The refusal names
/// each word with none.
pub(crate) fn check_homes(
    words: &BTreeSet<String>,
    forwarded: &BTreeSet<String>,
) -> Result<(), Vec<String>> {
    let homeless: Vec<String> = words
        .iter()
        .filter(|w| !forwarded.contains(*w) && !ROOT_SPELLED.contains(&w.as_str()))
        .map(|w| {
            format!(
                "fno-agent `{w}` has no fno spelling. Add an `fno agent` row whose legacy \
                 word is `{w}` (crates/fno/src/cli/agents_menu.rs), or delete its arm."
            )
        })
        .collect();
    if homeless.is_empty() {
        Ok(())
    } else {
        Err(homeless)
    }
}

const USAGE: &str = "usage: fno doctor lint menu-caps

Counts every command, hidden included, at the first three levels of fno and
fno-agent. Cap 13 at a root, 12 at levels two and three. Groups over the cap today are named in
scripts/ci/menu-caps-debt.tsv, which may only shrink.
Exit 0 ok, 1 a group over its cap or ceiling, 2 an input is missing.";

/// The command: exit 0 ok, 1 refused, 2 unreadable input.
pub fn run(args: &[std::ffi::OsString]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    if let Some(extra) = args.first() {
        eprintln!("menu-caps: unexpected argument {:?}\n{USAGE}", extra);
        return 2;
    }
    let root = crate::setup_run::repo_root();
    let inputs = match read_inputs(&root) {
        Ok(inputs) => inputs,
        Err(line) => {
            eprintln!("{line}");
            return 2;
        }
    };
    let counts = match counts(&inputs) {
        Ok(counts) => counts,
        Err(line) => {
            eprintln!("{line}");
            return 2;
        }
    };
    let homes = fno_agent_commands(&inputs.agents)
        .map_err(|e| vec![e])
        .and_then(|words| check_homes(&words, &menu_forwarded()));
    match check(&counts, &inputs.debt).and_then(|line| homes.map(|()| line)) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(failures) => {
            eprintln!("menu-caps: FAIL");
            for f in failures {
                eprintln!("  {f}");
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(groups: &[(&str, usize)]) -> BTreeMap<String, usize> {
        groups.iter().map(|(p, n)| (p.to_string(), *n)).collect()
    }

    #[test]
    fn menu_caps_refuses_every_debt_drift_and_counts_hidden_leaves() {
        // A group at the cap passes with no row; one more command, hidden or
        // not, refuses by name with the count and the cap (AC2).
        assert!(check(&tree(&[("do", 12)]), "").is_ok());
        let over = check(&tree(&[("do", 13)]), "").unwrap_err();
        assert!(
            over[0].contains("`do` holds 13") && over[0].contains("cap 12"),
            "{over:?}"
        );
        // A root holds one more than a group: 13 passes, 14 refuses (AC1).
        assert!(check(&tree(&[("(root)", 13), ("fno-agent", 13)]), "").is_ok());
        let root = check(&tree(&[("(root)", 14)]), "").unwrap_err();
        assert!(root[0].contains("cap 13"), "{root:?}");
        // Above, below, and inside a ceiling each refuse with the fix (AC3).
        let above = check(&tree(&[("mux", 22)]), "mux\t21\tfold\n").unwrap_err();
        assert!(above[0].contains("above its debt ceiling 21"), "{above:?}");
        let below = check(&tree(&[("mux", 20)]), "mux\t21\tfold\n").unwrap_err();
        assert!(
            below[0].contains("Lower its row") && below[0].contains("to 20"),
            "{below:?}"
        );
        let inside = check(&tree(&[("mux", 9)]), "# c\nmux\t21\tfold\n").unwrap_err();
        assert!(inside[0].contains("Delete its row"), "{inside:?}");
        assert!(check(&tree(&[]), "mux\tlots\n").is_err());
        // The ok line names the debt, so ok never reads as clean.
        let ok = check(&tree(&[("mux", 21), ("do", 3)]), "mux\t21\tfold\n").unwrap();
        assert!(ok.contains("1 groups in debt: mux 21"), "{ok}");

        // The guard never reads a visibility cell: a hidden native row
        // counts like a visible one, and a retired root's subtree is dropped.
        let mut native =
            String::from("# header\n(root)\troot\tvisible\t-\t-\nmux\tgroup\tvisible\t-\t-\n");
        for i in 0..13 {
            let vis = if i % 2 == 0 { "hidden" } else { "visible" };
            native.push_str(&format!("mux v{i}\tleaf\t{vis}\t-\t-\n"));
        }
        let mut inputs = Inputs {
            collapse_map: "current-leaf\ttier\nlead board\tT1\nwhoami x\tT1\n".into(),
            baseline: "# c\ninbox a !--hid\nfno-agent\n".into(),
            native_tree: native,
            agents: AgentSources {
                client: "async fn run(\n    args: X,\n    timing: Y,\n) { if command == \"claim\" {} if matches!(command, \"detect\" | \"top\") {} }\n\
                     let method = match command {\n    \"rows\" => {\n        match x { \"inner\" => 1 }\n    }\n}\n\
                     fn main() { if args.first().map(String::as_str) == Some(\"early\") {} }\n"
                    .into(),
                // A door module command counts like one dispatched in run.
                doors: "fn doors(command: &str) { if command == \"doored\" {} }\n".into(),
                // A pre-run table counts its arms, alternation included; a
                // nested purpose match is a leaf's grammar.
                tables: vec![(
                    "t.rs",
                    "pub fn client_dispatch",
                    "pub fn client_dispatch(a: &[String]) -> Option<i32> { match a {\n\
                     Some(\"tabled\" | \"twin\") => 1,\n\"plain\" => 2,\n\
                     Some(p @ (\"leaf\" | \"grammar\")) => 3,\n_ => None } }\n"
                        .into(),
                )],
            },
            debt: String::new(),
        };
        let got = counts(&inputs).unwrap();
        assert_eq!(got["mux"], 13);
        assert_eq!(got.get("lead"), None);
        assert_eq!(got.get("fno-agent"), Some(&11), "{got:?}");
        // A door table whose marker moved refuses by file and marker, never
        // a silent zero for its words.
        inputs.agents.tables[0].2 = "pub fn renamed() {}".into();
        let lost = counts(&inputs).unwrap_err();
        assert!(
            lost.contains("t.rs") && lost.contains("\"pub fn client_dispatch\""),
            "{lost}"
        );
        // The root is the table's thirteen; Rust-claimed rows count (AC1-HP).
        assert_eq!(got["(root)"], 13, "{got:?}");
        assert!(got["inbox"] >= 2, "{got:?}");
        assert_eq!(got["config accounts"], 15, "{got:?}");
        assert!(got.contains_key("agent fleet"));

        // A client word with no fno spelling refuses by name; a menu-forwarded
        // word and a root-spelled word pass.
        let words: BTreeSet<String> = ["zz-homeless", "census", "help"].map(String::from).into();
        let homes = check_homes(&words, &BTreeSet::from(["census".to_string()])).unwrap_err();
        assert!(
            homes.len() == 1 && homes[0].contains("`zz-homeless`"),
            "{homes:?}"
        );
    }
}
