//! Read one live session's transcript as chat events and count them.
//!
//! `cargo run --example chat_events -- <claude|codex|opencode> <session-id|newest>`
//!
//! Prints `chat_events=<n> unknown=<m>` and one count line per body kind. A
//! nonzero `unknown` names a source kind the mapping table must gain.

use fno::chat_events::{ChatRoots, SessionReader};
use fno::proto::chat_event::{ChatBody, SessionKey};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [harness, id] = args.as_slice() else {
        eprintln!("usage: chat_events <claude|codex|opencode> <session-id|newest>");
        std::process::exit(2);
    };
    let roots = ChatRoots::host();
    let session_id = if id == "newest" {
        match newest(harness, &roots) {
            Some(id) => id,
            None => {
                eprintln!("chat_events: no {harness} session found");
                std::process::exit(1);
            }
        }
    } else {
        id.clone()
    };
    let key = SessionKey {
        harness: harness.clone(),
        session_id: session_id.clone(),
        subagent: None,
    };
    let events = match SessionReader::open(&key, &roots, 0).and_then(|mut r| r.poll()) {
        Ok(events) => events,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown = 0;
    for event in &events {
        let kind = match &event.body {
            ChatBody::Unknown { name, .. } => {
                unknown += 1;
                format!("unknown:{name}")
            }
            body => serde_json::to_value(body)
                .ok()
                .and_then(|v| {
                    v.as_object()
                        .and_then(|o| o.get("kind"))
                        .and_then(|k| k.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default(),
        };
        *kinds.entry(kind).or_default() += 1;
    }
    println!("session={harness}/{session_id}");
    println!("chat_events={} unknown={unknown}", events.len());
    for (kind, n) in kinds {
        println!("  {kind}={n}");
    }
}

/// The most recent session in a harness's store.
fn newest(harness: &str, roots: &ChatRoots) -> Option<String> {
    match harness {
        "claude" | "codex" => {
            let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
            for root in &roots.transcripts {
                let is_codex = root.ends_with("sessions");
                if is_codex != (harness == "codex") {
                    continue;
                }
                walk(root, 0, &mut best);
            }
            let stem = best?.1.file_stem()?.to_str()?.to_string();
            let start = stem.len().checked_sub(36)?;
            Some(stem[start..].to_string())
        }
        "opencode" => roots.opencode_stores.iter().find_map(|db| {
            let conn = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            conn.query_row(
                "SELECT id FROM session WHERE parent_id IS NULL ORDER BY time_updated DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok()
        }),
        _ => None,
    }
}

fn walk(dir: &Path, depth: usize, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) != Some("subagents") {
                walk(&path, depth + 1, best);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                *best = Some((modified, path));
            }
        }
    }
}
