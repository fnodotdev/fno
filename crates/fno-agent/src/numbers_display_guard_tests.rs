//! The display guard: a question or decision reaches a reader by its
//! sequential number (`D3476`), never by its hash. The hash stays a storage
//! key and an input form. One scan walks the production half of every crate
//! and refuses a print statement that interpolates a raw id token or embeds
//! a literal hash; one protocol pin holds the push and relay builders to
//! `numbers::display`, because hooks/ask-push.sh reads the id field of the
//! claude-push line straight into the text a phone shows.
//!
//! Scope boundary: the scan pins print macros (`println!`, `eprintln!`,
//! `print!`, `write!`, `writeln!`) over the id vocabulary the doors use.
//! A `format!` that only builds an event id, a stored subject or a file
//! path is internal and stays legal; whatever it builds still has to pass
//! a print macro (or a pinned builder) to reach a reader, and those are
//! what the scan reads.

use std::fs;
use std::path::{Path, PathBuf};

const ID_TOKENS: [&str; 6] = [
    "{qid}",
    "{q_id}",
    "{decision_id}",
    "{did}",
    "{question_id}",
    "{question_hash}",
];

const PRINT_MACROS: [&str; 5] = ["println!", "eprintln!", "print!", "write!", "writeln!"];

fn src_roots() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        manifest.join("src"),
        manifest.join("../fno-app/src"),
        manifest.join("../fno-mail/src"),
        manifest.join("../fno/src"),
    ]
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_test_file = name.ends_with("_tests.rs");
            let in_tests_dir = path
                .components()
                .any(|c| c.as_os_str().to_str().is_some_and(|s| s == "tests"));
            if !is_test_file && !in_tests_dir {
                out.push(path);
            }
        }
    }
}

/// Every line of one file, numbered. Test files drop out at the walk (a
/// `_tests.rs` name or a `tests/` dir); the lines inside an inline
/// `#[cfg(test)]` mod stay in scope. A brace-tracking skip was measured and
/// dropped: fixture strings in test code hide braces from a per-line
/// counter, the skip swallowed whole production sections, and a no-skip
/// scan of every crate read clean. A future test that prints a raw id
/// token trips the guard loudly instead of silently scanning less.
fn all_lines(path: &Path) -> Vec<(usize, String)> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .enumerate()
        .map(|(index, raw)| (index + 1, raw.to_string()))
        .collect()
}

/// An 8-hex run right after `q-` or `d-`: a bare hash in printed text.
/// Number forms (`D142`) are 1-7 digits, so they never match. A hash handed
/// to `numbers::display("...")` inside the same span is the resolver at
/// work, not a leak, so those literals drop out first.
fn mask_display_literals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("display(\"") {
        let head = &rest[..pos + "display(\"".len()];
        let tail = &rest[pos + "display(\"".len()..];
        match tail.find('"') {
            Some(end) => {
                out.push_str(head);
                out.push('#');
                out.push(')');
                rest = &tail[end + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

fn has_literal_hash(text: &str) -> bool {
    let masked = mask_display_literals(text);
    let bytes = masked.as_bytes();
    (0..bytes.len().saturating_sub(10)).any(|start| {
        matches!(bytes.get(start), Some(b'q' | b'd'))
            && bytes.get(start + 1) == Some(&b'-')
            && (start + 2..start + 10)
                .all(|offset| bytes.get(offset).is_some_and(u8::is_ascii_hexdigit))
            && bytes
                .get(start + 10)
                .is_some_and(|byte| !byte.is_ascii_hexdigit())
    })
}

/// The pin arm: the builders whose output rides hooks/ask-push.sh (and the
/// chat relay line) must resolve ids through `numbers::display`, so the id
/// a phone or a chat transcript shows is the sequential number. Each entry
/// is (builder, source file); the fn body runs to the next item boundary.
fn pinned_builders() -> [(&'static str, &'static str); 5] {
    [
        ("push_payload", "backlog/question_ladder.rs"),
        ("push_line", "backlog/question_ladder.rs"),
        ("chat_answer_line", "backlog/question_clear.rs"),
        ("wake_holder", "backlog/question_ladder.rs"),
        ("push_title", "runtime/attention_http.rs"),
    ]
}

fn builder_missing_display(builder: &str, rel: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    let body = fs::read_to_string(&path).unwrap_or_default();
    let needle = format!("fn {builder}");
    let start = body.find(&needle)?;
    let tail = &body[start + needle.len()..];
    let boundary = ["\npub fn ", "\npub(crate) fn ", "\nfn ", "\n/// "]
        .iter()
        .filter_map(|marker| tail.find(marker))
        .min()
        .unwrap_or(tail.len());
    if tail[..boundary].contains("numbers::display") {
        None
    } else {
        Some(format!(
            "{builder} in {} lost numbers::display",
            path.display()
        ))
    }
}

#[test]
fn no_user_facing_render_emits_a_bare_question_or_decision_hash() {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in src_roots() {
        rs_files(&root, &mut files);
    }
    assert!(
        files.len() > 100,
        "the scan found no sources: {}",
        files.len()
    );

    let mut offences: Vec<String> = Vec::new();
    for file in &files {
        let lines = all_lines(file);
        let mut index = 0;
        while index < lines.len() {
            let (no, raw) = &lines[index];
            if !PRINT_MACROS
                .iter()
                .any(|macro_name| raw.contains(macro_name))
            {
                index += 1;
                continue;
            }
            // One print statement: the macro line plus its continuation
            // lines, until the parentheses balance out. Parens inside
            // format strings are balanced by construction, so the count
            // stays honest.
            let mut span = String::new();
            let mut depth: i32 = 0;
            let mut cursor = index;
            loop {
                let (_, line) = &lines[cursor];
                span.push_str(line);
                span.push('\n');
                depth += line.matches('(').count() as i32;
                depth -= line.matches(')').count() as i32;
                cursor += 1;
                if depth <= 0 || cursor >= lines.len() {
                    break;
                }
            }
            for token in ID_TOKENS {
                if span.contains(token) {
                    offences.push(format!(
                        "{}:{no} interpolates raw id {token}",
                        file.display()
                    ));
                }
            }
            if has_literal_hash(&span) {
                offences.push(format!("{}:{no} prints a literal hash", file.display()));
            }
            index = cursor;
        }
    }
    assert!(
        offences.is_empty(),
        "user-facing prints carry bare question/decision hashes; resolve through \
         crate::numbers::display and print the number:\n{}",
        offences.join("\n")
    );

    let missing: Vec<String> = pinned_builders()
        .iter()
        .filter_map(|(builder, rel)| builder_missing_display(builder, rel))
        .collect();
    assert!(
        missing.is_empty(),
        "push payload builders must resolve ids through numbers::display:\n{}",
        missing.join("\n")
    );
}
