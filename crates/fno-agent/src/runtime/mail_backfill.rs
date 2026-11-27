//! `fno-agent mail-backfill` - the historical-mail row engine.
//!
//! Mail an outage bypassed (sent over the harness's native cross-session
//! transport while `fno mail send` was down) goes back into the bus log as
//! durable rows that never re-deliver. The whole engine lives here once:
//! the transcript scan, the sender/receiver join, the deterministic
//! archive id (the idempotency key), the envelope assembly, and the write
//! through the bus-append door. `fno mail admin backfill` is the front
//! door; this module owns every decision the command makes.
//!
//! Contract: provenance is the point. A send without a row uuid has no
//! stable id and is skipped loud, never half-attributed; an id already in
//! the log is skipped (idempotent by msg_id); a joined pair whose receiver
//! block names a different body is never paired.

use serde_json::json;

/// The audit-only delivery value: the bytes already reached the recipient
/// over the harness's native cross-session transport. Mirrors
/// `bus.log.CROSS_SESSION_DELIVERY` and the drain gates
/// (`bus_read::AUDIT_ONLY_DELIVERIES`).
pub const CROSS_SESSION_DELIVERY: &str = "cross-session";

/// The transport label the row's meta carries: the provenance string the
/// node orders.
pub const TRANSPORT: &str = "claude-cross-session";

/// The deterministic archive id: `fmail-bf-` + the first 12 hex of the
/// sha256 over `sender-session|row-uuid`. Stable across re-runs so a
/// re-scan skips what already landed (idempotent by msg_id).
fn archive_msg_id(sender_session: &str, row_uuid: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(sender_session.as_bytes());
    h.update(b"|");
    h.update(row_uuid.as_bytes());
    let digest = h.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("fmail-bf-{}", &hex[..12])
}

/// Parse every cross-session-message block out of one user-turn text.
/// Attribute vocabulary observed in the wild: `from` (a socket or a name)
/// and `from-name` (the sender's legible label); the body is the text
/// between the open tag's `>` and the closing tag, byte-verbatim.
fn parse_blocks(text: &str) -> Vec<serde_json::Value> {
    let re =
        regex::Regex::new(r#"(?s)<cross-session-message\b([^>]*)>(.*?)</cross-session-message>"#)
            .expect("valid pattern");
    let attr_re = regex::Regex::new(r#"([a-zA-Z-]+)="([^"]*)""#).expect("valid pattern");
    re.captures_iter(text)
        .map(|c| {
            let attrs: serde_json::Map<String, serde_json::Value> = attr_re
                .captures_iter(c.get(1).map(|m| m.as_str()).unwrap_or(""))
                .map(|a| {
                    (
                        a.get(1).unwrap().as_str().to_string(),
                        json!(a.get(2).unwrap().as_str()),
                    )
                })
                .collect();
            let mut out = serde_json::Map::new();
            out.insert(
                "from".into(),
                attrs.get("from").cloned().unwrap_or(json!(null)),
            );
            out.insert(
                "from_name".into(),
                attrs.get("from-name").cloned().unwrap_or(json!(null)),
            );
            out.insert(
                "body".into(),
                json!(c.get(2).map(|m| m.as_str()).unwrap_or("")),
            );
            serde_json::Value::Object(out)
        })
        .collect()
}

/// Entry: `run` only. The one public shape is the JSON summary line; an
/// unknown subcommand refuses loud on stderr with exit 2.
pub fn run_mail_backfill(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "run" => match backfill_summary(args.get(1..).unwrap_or(&[])) {
            Ok(summary) => {
                println!("{summary}");
                0
            }
            Err(code) => code,
        },
        other => {
            eprintln!("mail-backfill: unknown subcommand {other:?} (expected run)");
            2
        }
    }
}

// ---------------------------------------------------------------------------
// The scan-and-write engine (`run`): the transcript scan, the sender-side
// join, and the durable write through the bus-append door.
// ---------------------------------------------------------------------------
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One native `SendMessage` tool call in a sender transcript.
#[derive(Clone)]
struct SendRow {
    uuid: String,
    ts: String,
    sender_session: String,
    summary: Option<String>,
    body: String,
    sender_transcript: String,
}

/// One `<cross-session-message>` block in a receiver transcript.
#[derive(Clone)]
struct BlockRow {
    from_name: Option<String>,
    body: String,
    receiver_session: String,
    receiver_transcript: String,
}

/// Whitespace-normalized first `n` chars: the join key on the body head.
fn norm_head(text: &str, n: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(n).collect()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// One stamp string to epoch seconds: the provenance fold's own reader, so
/// a format fix there reaches the window filter too.
fn ts_epoch(ts: &str) -> Option<f64> {
    crate::provenance::turn_ts_epoch(&json!({"timestamp": ts}))
}

/// Window bounds as epoch seconds; an unparseable bound refuses (a silent
/// wrong window would archive the wrong traffic).
fn bound_epoch(flag: &str, value: &str) -> Result<f64, String> {
    ts_epoch(value).ok_or_else(|| format!("unparseable --{flag} value {value:?}"))
}

/// The mail-send outage window, the engine's default scan scope: the hours
/// `fno mail send` refused every send while the role-rename deploy healed.
/// Overrides: --since / --until.
const OUTAGE_SINCE: &str = "2026-10-07T08:42:47Z";
const OUTAGE_UNTIL: &str = "2026-10-07T12:52:39Z";

/// The dry-run summary's `rows` sample head: the count stays in `joined`.
const DRY_ROWS_SAMPLE: usize = 200;

/// Window membership for one row timestamp: rows without a parseable stamp
/// stay in (the mtime prefilter already bounded them loosely).
fn ts_in_window(ts: &str, since: f64, until: f64) -> bool {
    match ts_epoch(ts) {
        Some(t) => t >= since && t <= until,
        None => true,
    }
}

/// The user-visible text of one transcript row (string content, or the
/// text blocks joined): the same shapes the provenance fold reads.
fn row_text(obj: &Value) -> String {
    let content = obj
        .get("message")
        .and_then(|m| m.get("content"))
        .unwrap_or(&Value::Null);
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Every native `SendMessage` tool call in one transcript row.
fn sender_sends_in(obj: &Value, path: &str) -> Vec<SendRow> {
    if obj.get("type").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    let content = match obj.get("message").and_then(|m| m.get("content")) {
        Some(Value::Array(blocks)) => blocks,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for block in content {
        let is_send = block.get("type").and_then(Value::as_str) == Some("tool_use")
            && block.get("name").and_then(Value::as_str) == Some("SendMessage");
        if !is_send {
            continue;
        }
        let input = block.get("input").cloned().unwrap_or(json!({}));
        let body = input
            .get("message")
            .or_else(|| input.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if body.is_empty() {
            continue;
        }
        out.push(SendRow {
            uuid: obj
                .get("uuid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            ts: obj
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            sender_session: obj
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            summary: input
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string),
            body: body.to_string(),
            sender_transcript: path.to_string(),
        });
    }
    out
}

/// Every cross-session block in one transcript row.
fn receiver_blocks_in(obj: &Value, path: &str) -> Vec<BlockRow> {
    if obj.get("type").and_then(Value::as_str) != Some("user") {
        return Vec::new();
    }
    let text = row_text(obj);
    if !text.contains("<cross-session-message") {
        return Vec::new();
    }
    let session = obj
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    parse_blocks(&text)
        .into_iter()
        .map(|b| BlockRow {
            from_name: b
                .get("from_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            body: b
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            receiver_session: session.clone(),
            receiver_transcript: path.to_string(),
        })
        .collect()
}

/// Recursively collect transcript files, deepest-last. Depth-bounded: a
/// project dir, its session dirs and their `subagents/` dirs is the real
/// shape; anything deeper is not a transcript we join.
fn collect_jsonl(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            collect_jsonl(&path, depth + 1, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// The live log the run writes, argv-first like the bus-append door so the
/// Python transport's resolver stays the single path authority.
fn backfill_live_path(explicit: Option<&str>) -> PathBuf {
    if let Some(raw) = explicit {
        return PathBuf::from(raw);
    }
    let home = crate::paths::AgentsHome::from_env();
    let dot_fno = home.root().parent().unwrap_or_else(|| home.root());
    crate::mail_paths::bus_log_path(dot_fno)
}

/// Pair each send with its receiver-side block by body attribution. The
/// sender addresses a peer by NAME while the receiver block carries the
/// sender's SOCKET, so the address forms never match; the body is the
/// evidence both halves share. A send joins when its normalized 120-char
/// head maps to exactly ONE candidate landing (a repeated head is
/// ambiguous and stays out; a repeated head inside ONE receiver transcript
/// is a resend and keeps the first copy). Heads under 40 chars are too
/// short to attribute by body alone and stay out. Deterministic, so a
/// re-scan pairs the same halves.
fn join<'a>(
    sends: &'a [SendRow],
    blocks: &'a [BlockRow],
) -> (Vec<(&'a SendRow, &'a BlockRow)>, usize) {
    let mut by_head: std::collections::HashMap<String, Vec<&'a BlockRow>> =
        std::collections::HashMap::new();
    let mut seen: std::collections::HashSet<(String, &str)> = std::collections::HashSet::new();
    for b in blocks {
        let head = norm_head(&b.body, 120);
        if seen.insert((head.clone(), b.receiver_transcript.as_str())) {
            by_head.entry(head).or_default().push(b);
        }
    }
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for s in sends {
        let head = norm_head(&s.body, 120);
        let attributable = head.chars().count() >= 40;
        match by_head.get(&head).map(Vec::as_slice) {
            Some([only]) if attributable => out.push((s, *only)),
            _ => skipped += 1,
        }
    }
    (out, skipped)
}

/// One parsed `run` invocation: the window bounds as epoch seconds, the
/// bus target, the scan roots, and whether the run writes.
struct BackfillRun {
    since_e: f64,
    until_e: f64,
    live: String,
    roots: Vec<String>,
    apply: bool,
}

/// The value after a value-taking flag: a following flag or end-of-line
/// refuses, the same rule `--root` holds. A silent default would scan or
/// write a window the operator never named.
fn next_value(args: &[String], i: usize, flag: &str) -> Result<String, i32> {
    match args.get(i + 1) {
        Some(v) if !v.starts_with("--") => Ok(v.clone()),
        _ => {
            eprintln!("mail-backfill run: {flag} needs a value");
            Err(2)
        }
    }
}

/// Parse the `run` argv: the window flags and their refusal, the wide
/// --all bounds, the default scan root, and the write posture. Err
/// carries the exit code, with the reason on stderr.
fn parse_backfill_run(args: &[String]) -> Result<BackfillRun, i32> {
    let mut since = String::new();
    let mut until = String::new();
    let mut live = String::new();
    let mut roots: Vec<String> = Vec::new();
    let mut apply = false;
    let mut all = false;
    let mut i = 0;
    while i < args.len() {
        match args.get(i).map(String::as_str) {
            Some("--since") => {
                since = next_value(args, i, "--since")?;
                i += 2;
            }
            Some("--until") => {
                until = next_value(args, i, "--until")?;
                i += 2;
            }
            Some("--all") => {
                all = true;
                i += 1;
            }
            Some("--live") => {
                live = next_value(args, i, "--live")?;
                i += 2;
            }
            Some("--root") => {
                // A following flag is never a path: consuming it would scan
                // a directory named "--apply" and report a quiet zero.
                match args.get(i + 1) {
                    Some(v) if !v.starts_with("--") => roots.push(v.clone()),
                    _ => {
                        eprintln!("mail-backfill run: --root needs a path");
                        return Err(2);
                    }
                }
                i += 2;
            }
            Some("--apply") => {
                apply = true;
                i += 1;
            }
            Some(other) => {
                eprintln!("mail-backfill run: unknown argument {other}");
                return Err(2);
            }
            None => break,
        }
    }
    if all && (!since.is_empty() || !until.is_empty()) {
        eprintln!("mail-backfill run: --all scans every transcript; drop --since/--until");
        return Err(2);
    }
    let (since_e, until_e) = if all {
        // No window: the bounds open wide so the mtime prefilter and the
        // per-row stamp filter both pass every row unchanged.
        (f64::NEG_INFINITY, f64::INFINITY)
    } else {
        let since = if since.is_empty() {
            OUTAGE_SINCE.to_string()
        } else {
            since
        };
        let until = if until.is_empty() {
            OUTAGE_UNTIL.to_string()
        } else {
            until
        };
        let since_e = bound_epoch("since", &since).map_err(|e| {
            eprintln!("mail-backfill run: {e}");
            2
        })?;
        let until_e = bound_epoch("until", &until).map_err(|e| {
            eprintln!("mail-backfill run: {e}");
            2
        })?;
        (since_e, until_e)
    };
    if roots.is_empty() {
        // No --root: scan the Claude projects home, the transcript base
        // every Claude Code session writes under, so the bare front-door
        // form finds the user's history.
        roots.push(
            crate::claude_drive::claude_projects_dir()
                .to_string_lossy()
                .to_string(),
        );
    }
    Ok(BackfillRun {
        since_e,
        until_e,
        live,
        roots,
        apply,
    })
}

/// One scan's harvest: the joined send/block pairs the attribution fold
/// found, and the count it refused. The raw halves die in the scan; only
/// attributed pairs survive.
struct Scan {
    pairs: Vec<(SendRow, BlockRow)>,
    skipped: usize,
}

/// Collect the transcript files under the run's roots, read the rows in
/// window, and join each send to its receiver landing by body head.
fn scan_join(run: &BackfillRun) -> Scan {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &run.roots {
        collect_jsonl(Path::new(root), 0, &mut files);
    }
    let mut sends: Vec<SendRow> = Vec::new();
    let mut blocks: Vec<BlockRow> = Vec::new();
    for path in &files {
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else { continue };
        let Ok(age) = mtime.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        if age.as_secs_f64() < run.since_e {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines().take(200_000) {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let ts = obj.get("timestamp").and_then(Value::as_str).unwrap_or("");
            if !ts_in_window(ts, run.since_e, run.until_e) {
                continue;
            }
            let p = path.to_string_lossy().to_string();
            sends.extend(sender_sends_in(&obj, &p));
            blocks.extend(receiver_blocks_in(&obj, &p));
        }
    }
    let (joined, skipped) = join(&sends, &blocks);
    let pairs = joined
        .into_iter()
        .map(|(s, b)| (s.clone(), b.clone()))
        .collect();
    Scan { pairs, skipped }
}

/// The whole engine: parse, scan and join, then (with --apply) write each
/// joined pair as an audit-only envelope through the bus-append door.
/// Returns the JSON summary line; Err carries the exit code of a refusal
/// (2) or a failed write (1), with the reason on stderr.
pub fn backfill_summary(args: &[String]) -> Result<Value, i32> {
    let run = parse_backfill_run(args)?;
    let scan = scan_join(&run);
    let mut written = 0usize;
    let mut rows_out: Vec<Value> = Vec::new();
    if run.apply {
        let live_path = backfill_live_path(if run.live.is_empty() {
            None
        } else {
            Some(run.live.as_str())
        });
        let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
        for seg in fno_mail::log::read_bus_segments(&live_path) {
            if let Some(id) = seg.get("id").and_then(Value::as_str) {
                existing.insert(id.to_string());
            }
        }
        for (send, block) in &scan.pairs {
            match write_pair(send, block, &existing, &live_path) {
                Ok(true) => written += 1,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("mail-backfill run: {e}");
                    return Err(1);
                }
            }
        }
    } else {
        for (send, block) in &scan.pairs {
            rows_out.push(json!({
                "ts": send.ts,
                "from_session": send.sender_session,
                "to_session": block.receiver_session,
                "body_head": norm_head(&send.body, 60),
            }));
        }
    }
    // An all-history dry run can join thousands of pairs; the summary keeps
    // the count and `rows` only a sample head, with the cut named.
    let truncated = rows_out.len().saturating_sub(DRY_ROWS_SAMPLE);
    rows_out.truncate(DRY_ROWS_SAMPLE);
    Ok(json!({
        "written": written,
        "joined": scan.pairs.len(),
        "skipped": scan.skipped,
        "rows_truncated": truncated,
        "rows": rows_out,
    }))
}

/// Write one joined pair: derive the archive id, skip an id already in the
/// log, and append the audit-only envelope through the bus-append door.
/// Ok(false) = already archived or no stable id; Err = the door failed.
fn write_pair(
    s: &SendRow,
    b: &BlockRow,
    existing: &std::collections::HashSet<String>,
    live_path: &Path,
) -> Result<bool, String> {
    if s.uuid.is_empty() {
        eprintln!("mail-backfill run: send without row uuid, no stable id; skipped");
        return Ok(false);
    }
    let id = archive_msg_id(&s.sender_session, &s.uuid);
    if existing.contains(&id) {
        return Ok(false);
    }
    let mut meta = serde_json::Map::new();
    meta.insert("transport".into(), json!("claude-cross-session"));
    meta.insert("sender_transcript".into(), json!(s.sender_transcript));
    meta.insert("receiver_transcript".into(), json!(b.receiver_transcript));
    meta.insert("to_session".into(), json!(b.receiver_session));
    meta.insert("sender_row".into(), json!(s.uuid));
    meta.insert("backfilled_at".into(), json!(now_iso()));
    // Key order mirrors the Python serializer (bus.log.to_json_line):
    // always-present keys first, optional tags, body last.
    let mut env = serde_json::Map::new();
    env.insert("v".into(), json!(1));
    env.insert("id".into(), json!(id));
    env.insert("ts".into(), json!(s.ts));
    env.insert("thread".into(), json!(id));
    let from_label = b
        .from_name
        .clone()
        .unwrap_or_else(|| s.sender_session.clone());
    env.insert("from".into(), json!(from_label));
    env.insert("to".into(), json!(b.receiver_session));
    env.insert("kind".into(), json!("send"));
    env.insert("delivery".into(), json!("cross-session"));
    env.insert("from_session".into(), json!(s.sender_session));
    env.insert("to_kind".into(), json!("session"));
    env.insert("origin".into(), json!("peer"));
    if let Some(sub) = &s.summary {
        env.insert("subject".into(), json!(sub));
    }
    env.insert(
        "word_count".into(),
        json!(s.body.split_whitespace().count()),
    );
    env.insert("meta".into(), Value::Object(meta));
    env.insert("body".into(), json!(s.body));
    crate::announce::append_line_open(live_path, &Value::Object(env), &crate::chats::chats_dir())
        .map(|()| true)
        .map_err(|e| format!("append failed: {e}"))
}
