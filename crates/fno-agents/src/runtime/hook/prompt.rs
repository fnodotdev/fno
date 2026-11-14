//! The UserPromptSubmit entry: the open list where the user types.
//!
//! Reads `prompt` from the hook payload. A turn that carries the mail
//! envelope, or that the provenance classifier reads as machine-shaped,
//! renders nothing. Otherwise it arms the conversation hold (a real
//! message whose Enter the mux witnessed) and prints one
//! `hookSpecificOutput` whose `additionalContext` lists the cached
//! projection (top five ready items) and a count. It never folds a
//! journal: the daemon's `attention` arm writes `~/.fno/state/attention/items.json`,
//! this reads it.
//!
//! A question is news once. `seen.json` beside the cache records, for the
//! user and not per session, which ids the block already showed. A turn
//! lists only ids not yet shown, or ids whose deadline window has newly
//! passed; a snoozed question never lists. Everything else is one count.

use crate::attention::{quiet_from, Quiet};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

/// A cache older than this names its age instead of the items.
const CACHE_MAX_AGE_SECS: u64 = 120;
const MAX_ITEMS: usize = 5;

/// `fno-agents hook prompt`: payload on stdin, the envelope (or nothing) on
/// stdout, exit 0 always.
pub fn run(_args: &[String]) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let prompt = payload.get("prompt").and_then(Value::as_str).unwrap_or("");
    if crate::mail_inject::contains_fno_mail_tag_anywhere(prompt)
        || crate::provenance::classify(prompt).is_err()
    {
        return 0;
    }
    // A real user message whose Enter the mux witnessed arms the
    // conversation hold (C2). Past the machine-prompt skip, silent, and
    // before the attention render.
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !session_id.is_empty() {
        crate::mail_hold::conversation_prompt(session_id, prompt);
    }
    // Role audience: a worker session never sees the fleet projection. The
    // hold armed above; a lead the user types to still reads the block.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let role =
        crate::owner_ladder::role_now(session_id, &crate::paths::AgentsHome::from_env(), &cwd);
    if role == crate::owner_ladder::Role::Worker {
        return 0;
    }
    let event_name = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("UserPromptSubmit");
    let text = match read_cache() {
        None => {
            "No attention cache yet: the daemon's attention arm writes it every beat.".to_string()
        }
        Some((as_of, items)) => {
            let now = now_secs();
            if now.saturating_sub(as_of) > CACHE_MAX_AGE_SECS {
                format!(
                    "Attention cache is {}s old (max {CACHE_MAX_AGE_SECS}s); showing nothing.",
                    now.saturating_sub(as_of)
                )
            } else {
                let seen_file = seen_path();
                let seen = seen_file
                    .as_deref()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .map(|raw| parse_seen(&raw))
                    .unwrap_or_default();
                let (fresh, next) = select_new(&items, &seen, now as i64);
                if let Some(path) = seen_file.as_deref().filter(|_| next != seen) {
                    write_seen(path, &next);
                }
                if fresh.is_empty() {
                    standing_line(items.len())
                } else {
                    let mut block = render_block(&fresh);
                    if items.len() > fresh.len() {
                        block.push_str(&format!(
                            "\n{} more sit on your list (fno inbox outstanding).",
                            items.len() - fresh.len()
                        ));
                    }
                    block
                }
            }
        }
    };
    println!(
        "{}",
        json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": text,
            }
        })
    );
    0
}

/// The recorded epoch for an id shown before its deadline. A due epoch is
/// never negative, so the first due window always beats it.
const SHOWN_EARLY: i64 = -1;

fn seen_path() -> Option<PathBuf> {
    Some(
        crate::attention_arm::attention_dir()
            .ok()?
            .join("seen.json"),
    )
}

/// `{id: epoch}`; a corrupt or unreadable record reads as empty.
fn parse_seen(raw: &str) -> HashMap<String, i64> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Atomic write; the temp name carries the pid so two prompts at once do
/// not share one. A lost write only shows a question once more.
fn write_seen(path: &std::path::Path, seen: &HashMap<String, i64>) {
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if serde_json::to_string(seen)
        .map(|s| std::fs::write(&tmp, s))
        .unwrap_or(Err(std::io::Error::other("serialize")))
        .is_ok()
    {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The items this turn should list and the record to write back. An item
/// lists when it is unseen and not snoozed, or when its deadline window is
/// newer than the one the record holds. Ids no longer in the cache leave
/// the record.
fn select_new(
    items: &[Value],
    seen: &HashMap<String, i64>,
    now: i64,
) -> (Vec<Value>, HashMap<String, i64>) {
    let mut fresh = Vec::new();
    let mut next = HashMap::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            fresh.push(item.clone());
            continue;
        };
        let field = |k: &str| item.get(k).and_then(Value::as_str);
        let recorded = seen.get(id).copied();
        let quiet = quiet_from(
            field("created_at").unwrap_or(""),
            field("deadline"),
            field("snoozed_until"),
            now,
        );
        let (show, mark) = match quiet {
            Quiet::Snoozed(_) => (false, recorded),
            Quiet::BeforeDeadline(_) => (recorded.is_none(), recorded.or(Some(SHOWN_EARLY))),
            Quiet::Due { epoch } => {
                let epoch = epoch as i64;
                let show = recorded.is_none_or(|r| epoch > r);
                (show, if show { Some(epoch) } else { recorded })
            }
        };
        if show {
            fresh.push(item.clone());
        }
        if let Some(mark) = mark {
            next.insert(id.to_string(), mark);
        }
    }
    (fresh, next)
}

/// The block when nothing is new: one line, never the standing list.
fn standing_line(total: usize) -> String {
    match total {
        0 => String::from("Nothing is waiting on you."),
        1 => String::from(
            "Nothing new is waiting on you. 1 question sits on your list (fno inbox outstanding).",
        ),
        n => format!(
            "Nothing new is waiting on you. {n} questions sit on your list (fno inbox outstanding)."
        ),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `{as_of, items}` as the arm writes it; a malformed cache reads as absent.
fn read_cache() -> Option<(u64, Vec<Value>)> {
    let path = crate::attention_arm::attention_dir()
        .ok()?
        .join("items.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("as_of").and_then(Value::as_u64)?,
        v.get("items")
            .and_then(Value::as_array)?
            .iter()
            .filter(|i| i.get("ready").and_then(Value::as_bool) == Some(true))
            .cloned()
            .collect(),
    ))
}

/// The one field render_block reads beyond the title line: the cached probe
/// reading the attention arm joined in. Unknown fields deserialize away.
#[derive(serde::Deserialize, Default)]
struct ItemProbe {
    #[serde(default)]
    probe: Option<crate::question_probe::ProbeReading>,
}

fn render_block(items: &[Value]) -> String {
    let total = items.len();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let mut lines: Vec<String> = items
        .iter()
        .take(MAX_ITEMS)
        .map(|i| {
            let number = i
                .get("number")
                .and_then(Value::as_str)
                .map(|n| format!("{n} "))
                .unwrap_or_default();
            let kind = i.get("kind").and_then(Value::as_str).unwrap_or("?");
            let title = i.get("title").and_then(Value::as_str).unwrap_or("");
            let asker = i
                .get("asker")
                .and_then(|a| a.get("handle"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let host = i
                .get("host")
                .and_then(Value::as_str)
                .map(|h| format!(" on {h}"))
                .unwrap_or_default();
            let probe = serde_json::from_value::<ItemProbe>(i.clone())
                .ok()
                .and_then(|v| v.probe);
            let suffix = crate::question_probe::check_suffix(probe.as_ref(), now);
            if asker.is_empty() {
                format!("- {number}{kind}: {title}{host}{suffix}")
            } else {
                format!("- {number}{kind}: {title} (from {asker}{host}){suffix}")
            }
        })
        .collect();
    if total > MAX_ITEMS {
        lines.push(format!(
            "... and {} more; the full list is on your questions file.",
            total - MAX_ITEMS
        ));
    }
    if lines.is_empty() {
        return String::from("Nothing is waiting on you.");
    }
    let mut out = format!("Waiting on you ({}):\n", total);
    out.push_str(&lines.join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, kind: &str, asker: &str) -> Value {
        json!({"kind": kind, "title": title, "asker": {"handle": asker}, "ready": true})
    }

    #[test]
    fn ac12_hp_block_lists_items_with_a_count() {
        let items = vec![
            item("Which reading?", "question", "lead-fno-g6"),
            item("Publish the crate", "pin", "worker-2"),
            item("ship tonight", "mine", ""),
        ];
        let text = render_block(&items);
        assert!(text.contains("Waiting on you (3)"));
        assert!(text.contains("- question: Which reading? (from lead-fno-g6)"));
        assert!(text.contains("- pin: Publish the crate (from worker-2)"));
        assert!(text.contains("- mine: ship tonight"));
        let numbered = render_block(&[serde_json::json!(
            {"kind": "question", "title": "Pick", "number": "Q87"}
        )]);
        assert!(numbered.contains("- Q87 question: Pick"), "{numbered}");
    }

    #[test]
    fn waiting_on_you_lines_end_with_the_probe_last_check() {
        let checked_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
            - 180;
        let items = vec![
            json!({
                "kind": "question", "title": "Approve release run 42700851689?",
                "ready": true, "asker": {"handle": "lead"},
                "probe": {"kind": "run", "target": "42700851689",
                    "baseline": "waiting", "state": "waiting",
                    "evidence": "", "checked_at": checked_at},
            }),
            item("Publish the crate", "pin", "worker-2"),
        ];
        let block = render_block(&items);
        assert!(
            block
                .lines()
                .any(|l| l.ends_with("(checked 3m ago: run 42700851689 waiting)")),
            "{block}"
        );
        assert!(
            block
                .lines()
                .any(|l| l.ends_with("(not machine-checkable)")),
            "{block}"
        );
    }

    /// A mail-tagged payload returns before the arm call (the skip lives in
    /// `run`); the same payload must not arm through `conversation_prompt`
    /// either. Drives the env-pinned harness the mail-hold tests use.
    #[test]
    fn a_mail_tagged_prompt_arms_nothing() {
        const SID: &str = "cccccccc-1111-2222-3333-444455556666";
        crate::mail_hold::tests::with_hold_env(|dir| {
            crate::mail_hold::tests::write_registry(
                dir,
                serde_json::json!([crate::mail_hold::tests::registry_row("worker", SID)]),
            );
            crate::mail_hold::tests::witness_row(
                SID,
                chrono::Utc::now().timestamp_millis() - 1_000,
            );
            let mail = "<fno_mail from=\"x\">hello</fno_mail>";
            crate::mail_hold::conversation_prompt(SID, mail);
            assert!(
                !dir.join("db/mail/mail-hold")
                    .join(format!("{SID}.json"))
                    .exists(),
                "a mail-tagged prompt never arms the conversation hold"
            );
            // The header-framed shape reads injected the same way.
            let header_mail = "`@folio · msg-1 · hello`\nhello body";
            crate::mail_hold::conversation_prompt(SID, header_mail);
            assert!(
                !dir.join("db/mail/mail-hold")
                    .join(format!("{SID}.json"))
                    .exists(),
                "a header-framed prompt never arms the conversation hold"
            );
        });
    }
    fn q(id: &str, created: &str) -> Value {
        json!({"id": id, "kind": "question", "title": id, "ready": true,
            "created_at": created, "deadline": "2026-11-13T00:00:00Z"})
    }

    fn at(ts: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn a_question_shown_once_is_not_listed_again_in_any_session() {
        let items = vec![
            q("q-1", "2026-11-12T00:00:00Z"),
            q("q-2", "2026-11-12T00:00:00Z"),
            q("q-3", "2026-11-12T00:00:00Z"),
        ];
        let now = at("2026-11-13T06:00:00Z");
        let (first, record) = select_new(&items, &HashMap::new(), now);
        assert_eq!(first.len(), 3);
        // The next turn, in any lead session, reads the same one record.
        let (second, _) = select_new(&items, &record, now + 60);
        assert!(second.is_empty(), "{second:?}");
        assert!(standing_line(3).contains("3 questions sit on your list"));
        // A later 24h window is news again, once.
        let (window, record) = select_new(&items, &record, now + 86_400);
        assert_eq!(window.len(), 3);
        assert!(select_new(&items, &record, now + 86_460).0.is_empty());
    }

    #[test]
    fn a_snoozed_question_never_lists_and_a_corrupt_record_reads_empty() {
        let mut snoozed = q("q-s", "2026-11-12T00:00:00Z");
        snoozed["snoozed_until"] = json!("2026-11-20T09:00:00Z");
        let (fresh, record) = select_new(&[snoozed], &HashMap::new(), at("2026-11-14T00:00:00Z"));
        assert!(fresh.is_empty() && record.is_empty());
        assert!(parse_seen("{not json").is_empty());
    }
}
