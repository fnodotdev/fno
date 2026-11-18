//! The ruling-4 gate: every SQL write to an owned backlog table lives in
//! its owning module. A positive control proves the scanner sees the real
//! in-owner writes - a zero there means the scanner or the walk is broken, not that the tree is clean.
//! AC12 adds one stricter rule: backlog/api.rs composes through module APIs
//! and may not even NAME an owned table.

use std::fs;
use std::path::{Path, PathBuf};

const WRITE_COMMANDS: &[&str] = &[
    // "insert or replace into" must be tried before "insert into".
    "insert or replace into",
    "replace into",
    "insert into",
    "delete from",
    "update",
];

/// Identifier characters, for whole-word matching.
fn word_boundary_chars(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True when the lowercased line writes to `table`: some write command is
/// followed by whitespace and the table as a whole word.
fn contains_write(table: &str, line_lower: &str) -> bool {
    for command in WRITE_COMMANDS {
        let mut search_from = 0;
        while let Some(hit) = line_lower[search_from..].find(command) {
            let command_start = search_from + hit;
            let command_end = command_start + command.len();
            search_from = command_end;
            // A command glued to earlier word characters is someone else's word
            // ("reupdate", "updated_at").
            if command_start > 0
                && line_lower[..command_start]
                    .chars()
                    .next_back()
                    .map_or(false, word_boundary_chars)
            {
                continue;
            }
            let token: String = line_lower[command_end..]
                .chars()
                .skip_while(|c: &char| c.is_whitespace())
                .take_while(|c| word_boundary_chars(*c))
                .collect();
            if token == table {
                return true;
            }
        }
    }
    false
}

/// The contains_name walk with a string-literal gate: when `strings_only` is
/// set, an occurrence counts only inside a double-quoted string literal on
/// the line. Code identifiers (fn names, module paths, struct-field
/// accesses) are the API surface composing through the typed model and the
/// owning modules; only a string can carry a table name into SQL or a wire
/// selector. A backslash escapes the closing quote, minimally.
fn contains_name_in_string(table: &str, line_lower: &str, strings_only: bool) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line_lower.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            _ => {}
        }
        if ch == table.chars().next().unwrap() {
            // Occurrence candidate: match the rest of the table word.
            let candidate = &line_lower[index..];
            if let Some(hit_end) = candidate.strip_prefix(table) {
                let before_ok = index == 0
                    || !line_lower[..index]
                        .chars()
                        .next_back()
                        .map_or(false, word_boundary_chars);
                let after_ok = hit_end.is_empty()
                    || !hit_end.chars().next().map_or(false, word_boundary_chars);
                if before_ok && after_ok && (in_string || !strings_only) {
                    return true;
                }
            }
        }
    }
    false
}

fn owner_of<'a>(table: &str, owners: &'a [(&'a str, &'a str)]) -> Option<&'a str> {
    owners
        .iter()
        .find(|(owned, _)| *owned == table)
        .map(|(_, owner)| *owner)
}

/// Every line of `text` that writes an owned table, as (line_no, table).
fn scan_text<'a>(text: &str, owners: &'a [(&'a str, &'a str)]) -> Vec<(usize, &'a str)> {
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        if let Some(found) = owners
            .iter()
            .find(|(table, _)| contains_write(table, &lower))
        {
            hits.push((index + 1, found.0));
        }
    }
    hits
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Scan every .rs file under `root`, judging by paths relative to `root`,
/// the way the real-tree scan does. Returns the violations and the count of
/// in-owner writes (the positive control).
fn scan_root(root: &Path, owners: &[(&str, &str)]) -> (Vec<String>, usize) {
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    let mut violations = Vec::new();
    let mut in_owner_writes = 0;
    for path in files {
        let rel = path
            .strip_prefix(root)
            .expect("every walked file sits under the scan root")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        let text = fs::read_to_string(&path).expect("read source file");
        // approvals.rs writes the tables of approvals.db, a separate database
        // whose `decisions` table only shares a name with the graph store's.
        if rel == "gate/approvals.rs" {
            continue;
        }
        // A test fixture in a `#[cfg(test)] mod tests` inserts into owned
        // tables to build a throwaway DB in tempdir; that is not the store's
        // production write path. The graph_db scan later in this file makes
        // the same exemption. A planted production write still fails: the
        // self-test files carry no test module.
        if rel.ends_with("tests.rs") || rel.split('/').any(|part| part == "tests") {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let test_module = (1..lines.len())
            .find(|&index| {
                lines[index].trim_start().starts_with("mod tests")
                    && lines[index - 1].trim() == "#[cfg(test)]"
            })
            .unwrap_or(lines.len());
        let (production_lines, _) = lines.split_at(test_module);
        let production = production_lines.join("\n");
        for (line_no, table) in scan_text(&production, owners) {
            let owner = owner_of(table, owners).expect("an owned table has an owner");
            if rel == owner {
                in_owner_writes += 1;
            } else {
                violations.push(format!(
                    "{rel}:{line_no}: SQL write to {table} outside {owner}"
                ));
            }
        }
        // AC12: the api surface composes through module APIs; it may not
        // name an owned table in a STRING: strings are how a table name
        // reaches SQL, a wire op selector, or a store key. Code identifiers
        // (fn names, module paths, struct-field accesses) are composition,
        // not naming - `pub fn nodes` and `crate::backlog::nodes::load` are
        // the rule working as designed, so the naming check gates strings
        // only.
        if rel == "backlog/api.rs" {
            for (index, line) in text.lines().enumerate() {
                let lower = line.to_lowercase();
                if let Some(found) = owners
                    .iter()
                    .find(|(table, _)| contains_name_in_string(table, &lower, true))
                {
                    violations.push(format!(
                        "backlog/api.rs:{}: names owned table {} (compose through module APIs)",
                        index + 1,
                        found.0
                    ));
                }
            }
        }
    }
    (violations, in_owner_writes)
}

/// Every non-test site under `root` that opens the graph.db path: a
/// `Connection::open(` or `open_with_flags(` whose next lines name
/// `database_path(` or `graph.db`. Only `backlog::open` sets
/// `foreign_keys=ON`, so any other writer skips the schema's keys. Allowed:
/// the store module itself and the migration's read-only snapshot reader.
/// Returns the violations as `file:line` and the count of allowed sites.
fn graph_db_opens(root: &Path) -> (Vec<String>, usize) {
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    let mut violations = Vec::new();
    let mut allowed = 0;
    for path in files {
        let rel = path
            .strip_prefix(root)
            .expect("every walked file sits under the scan root")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        if rel.ends_with("tests.rs") || rel.split('/').any(|part| part == "tests") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read source file");
        let lines: Vec<&str> = text.lines().collect();
        let test_module = (1..lines.len())
            .find(|&index| {
                lines[index].trim_start().starts_with("mod tests")
                    && lines[index - 1].trim() == "#[cfg(test)]"
            })
            .unwrap_or(lines.len());
        for index in 0..test_module {
            let line = lines[index];
            if !line.contains("Connection::open(") && !line.contains("open_with_flags(") {
                continue;
            }
            let window = lines[index..(index + 3).min(test_module)].join("\n");
            if !window.contains("database_path(") && !window.contains("graph.db") {
                continue;
            }
            if rel == "backlog/mod.rs"
                || (rel == "backlog/schema_v4.rs" && window.contains("READ_ONLY"))
            {
                allowed += 1;
            } else {
                violations.push(format!("{rel}:{}", index + 1));
            }
        }
    }
    (violations, allowed)
}

#[test]
fn only_the_store_opens_graph_db() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    let mut allowed = 0;
    for root in [manifest.join("src"), manifest.join("../fno/src")] {
        let (found, ok) = graph_db_opens(&root);
        violations.extend(found);
        allowed += ok;
    }
    assert!(
        allowed > 0,
        "the audit found zero store opens; the scanner or the walk is broken"
    );
    assert!(
        violations.is_empty(),
        "graph.db opened outside backlog::open:\n{}",
        violations.join("\n")
    );
}

#[test]
fn src_tree_writes_only_in_owning_modules() {
    let owners = fno_agent::backlog::TABLE_OWNERS;
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(src.is_dir(), "{} is missing", src.display());
    let (violations, in_owner_writes) = scan_root(&src, owners);
    assert!(
        in_owner_writes > 0,
        "the scan found zero in-owner SQL writes; the scanner or the walk is broken"
    );
    assert!(
        violations.is_empty(),
        "SQL writes outside their owning modules:\n{}",
        violations.join("\n")
    );
}
