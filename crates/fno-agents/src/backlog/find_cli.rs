//! `fno backlog find`'s search ladder, ported from the Python command the
//! grouped dispatcher routes here (the Python leg is deleted). Owns: the
//! exact tiers, the ab- prefix tier (resolve_id's id branch), the
//! describe-it token-subset search over title+slug+details, the four
//! filters, the archive read-through on a miss, and the two render shapes
//! (pretty JSON, TSV handle lines). `--fts` rides the documented substring
//! degrade, and an external backend selection refuses with the guarded
//! metadata reader's exact message; everything else is byte-faithful.

use serde_json::Value;

use super::workflows::text_at;

/// The parsed find invocation. `None` is the usage-error shape (an unknown
/// flag, a second positional, or no query).
struct FindArgs<'a> {
    query: &'a str,
    domain: Option<&'a str>,
    project: Option<&'a str>,
    status: Option<&'a str>,
    source_kind: Option<&'a str>,
    /// `--tag` values; a row must carry every one.
    tags: Vec<&'a str>,
    /// `--related <id>`: rank the graph against this node.
    related: Option<&'a str>,
    /// `--notes <prefix>`: list progress notes that start with the prefix.
    notes: Option<&'a str>,
    limit: usize,
    fts: bool,
    json_out: bool,
}

impl FindArgs<'_> {
    fn parse<'a>(tail: &'a [String]) -> Option<FindArgs<'a>> {
        let mut query: Option<&'a str> = None;
        let mut domain: Option<&'a str> = None;
        let mut project: Option<&'a str> = None;
        let mut status: Option<&'a str> = None;
        let mut source_kind: Option<&'a str> = None;
        let mut tags: Vec<&'a str> = Vec::new();
        let mut related: Option<&'a str> = None;
        let mut notes: Option<&'a str> = None;
        let mut limit = 20;
        let mut fts = false;
        let mut json_out = false;
        let mut i = 0;
        while i < tail.len() {
            match tail[i].as_str() {
                "--domain" | "-d" => {
                    i += 1;
                    domain = Some(tail.get(i)?.as_str());
                }
                "--project" | "-p" => {
                    i += 1;
                    project = Some(tail.get(i)?.as_str());
                }
                "--status" | "-s" => {
                    i += 1;
                    status = Some(tail.get(i)?.as_str());
                }
                "--source-kind" => {
                    i += 1;
                    source_kind = Some(tail.get(i)?.as_str());
                }
                "--tag" | "-t" => {
                    i += 1;
                    tags.push(tail.get(i)?.as_str());
                }
                "--related" => {
                    i += 1;
                    related = Some(tail.get(i)?.as_str());
                }
                "--notes" => {
                    i += 1;
                    notes = Some(tail.get(i)?.as_str());
                }
                // Accepted for compatibility; the native find has no FTS
                // cache, so the flag rides the documented substring degrade.
                "--fts" => fts = true,
                "--limit" | "-L" => {
                    i += 1;
                    limit = tail.get(i)?.parse().ok()?;
                }
                "--json" | "-J" => json_out = true,
                other => {
                    if other.starts_with('-') {
                        return None;
                    }
                    if query.is_some() {
                        return None;
                    }
                    query = Some(other);
                }
            }
            i += 1;
        }
        // A `--tag` or `--related` read needs no query; `--related` takes
        // none beside its id.
        if ((related.is_some() || notes.is_some()) && query.is_some())
            || (query.is_none() && tags.is_empty() && related.is_none() && notes.is_none())
        {
            return None;
        }
        Some(FindArgs {
            query: query.unwrap_or(""),
            domain,
            project,
            status,
            source_kind,
            tags,
            related,
            notes,
            limit,
            fts,
            json_out,
        })
    }
}

const FIND_HELP: &str = "Search graph entries: exact id/slug/bare-hex, else high-recall over title+slug+details+wave notes\n\nUsage: fno backlog find [OPTIONS] [QUERY]\n       fno backlog find --tag <FACET:VALUE>... [OPTIONS]\n       fno backlog find --related <ID> [OPTIONS]\n       fno backlog find --notes <PREFIX>\n\nArguments:\n  [QUERY]  ab-id / id-prefix / slug / bare-hex / free-text description\n\nOptions:\n  -t, --tag <TAG>           Keep nodes that carry TAG, e.g. area:mail (repeatable; every tag must match)\n      --related <ID>        Rank nodes related to ID across epics: shared terms, area/harness/topic tags, same epic\n      --notes <PREFIX>      List progress notes whose text starts with PREFIX, newest first, verbatim (no query)\n  -d, --domain <DOMAIN>     Filter by domain\n  -p, --project <PROJECT>   Filter by project\n  -s, --status <STATUS>     Filter by status\n      --source-kind <KIND>  Filter by origin: organic|from_inbox|from_observation|from_supervisor|operator_request\n      --fts                 Full-text search; the native find degrades to substring search\n  -L, --limit <LIMIT>       Max results for the --fts and --related lanes [default: 20]\n  -J, --json                Emit JSON array\n  -h, --help                Print help";

/// The describe-it search: every query token must appear in the
/// concatenated title+slug+details+wave-note text, case-insensitive,
/// non-done first.
fn search_entries(entries: &[Value], query: &str) -> Vec<Value> {
    let tokens: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let hit = |e: &Value| {
        let mut text = String::new();
        for field in ["title", "slug", "details"] {
            if let Some(s) = e.get(field).and_then(Value::as_str) {
                text.push_str(&s.to_lowercase());
                text.push(' ');
            }
        }
        for note in super::wave_notes::wave_rows(e) {
            for field in ["title", "details", "text"] {
                if let Some(s) = note.get(field).and_then(Value::as_str) {
                    text.push_str(&s.to_lowercase());
                    text.push(' ');
                }
            }
        }
        tokens.iter().all(|t| text.contains(t.as_str()))
    };
    let (non_done, done): (Vec<&Value>, Vec<&Value>) = entries
        .iter()
        .partition(|e| e.get("status").and_then(Value::as_str) != Some("done"));
    non_done
        .into_iter()
        .chain(done)
        .filter(|e| hit(e))
        .cloned()
        .collect()
}

/// The id-prefix tier for an `ab-` query: 4-7 hex suffix matches ids by
/// prefix; a full 8-hex or malformed suffix returns nothing (resolve_id's
/// id branch, byte-faithful).
fn ab_prefix_hits(entries: &[Value], query: &str) -> Vec<Value> {
    let suffix = &query["ab-".len()..];
    let full = query.len() == 3 + 8 && suffix.bytes().all(|b| b.is_ascii_hexdigit());
    let partial = (4..=7).contains(&suffix.len()) && suffix.bytes().all(|b| b.is_ascii_hexdigit());
    if full {
        return Vec::new();
    }
    if partial {
        return entries
            .iter()
            .filter(|e| {
                e.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v.starts_with(query))
            })
            .cloned()
            .collect();
    }
    Vec::new()
}

/// The exact tiers (the same three get uses), returning the candidates.
fn exact_tier(entries: &[Value], query: &str) -> Option<Vec<Value>> {
    for e in entries {
        if e.get("id").and_then(Value::as_str) == Some(query) {
            return Some(vec![e.clone()]);
        }
    }
    let q_lc = query.to_lowercase();
    for e in entries {
        let slug = e.get("slug").and_then(Value::as_str);
        if slug.is_some_and(|s| !s.is_empty()) && slug == Some(q_lc.as_str()) {
            return Some(vec![e.clone()]);
        }
    }
    let bare_hex = query.len() >= 4
        && query.len() <= 8
        && query
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if bare_hex {
        let mut prefixes = vec![super::settings::node_id_prefix()];
        prefixes.push("ab-".to_string());
        prefixes.dedup();
        for p in &prefixes {
            let cand = format!("{p}{query}");
            if let Some(hit) = entries
                .iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(cand.as_str()))
            {
                return Some(vec![hit.clone()]);
            }
        }
    }
    None
}

/// The filters, reading the resolved source_kind default. `tags` holds the
/// normalized `--tag` values.
fn passes_filters(e: &Value, args: &FindArgs<'_>, tags: &[String]) -> bool {
    if !super::tags::has_all(e, tags) {
        return false;
    }
    if let Some(d) = args.domain {
        if e.get("domain").and_then(Value::as_str) != Some(d) {
            return false;
        }
    }
    if let Some(p) = args.project {
        if e.get("project").and_then(Value::as_str) != Some(p) {
            return false;
        }
    }
    if let Some(s) = args.status {
        if e.get("status").and_then(Value::as_str) != Some(s) {
            return false;
        }
    }
    if let Some(sk) = args.source_kind {
        let resolved = e
            .get("source_kind")
            .and_then(Value::as_str)
            .unwrap_or("organic");
        if resolved != sk {
            return false;
        }
    }
    true
}

/// The display handle: `slug (id)`, or `(id)` when unslugged.
fn format_handle(e: &Value) -> String {
    let nid = e.get("id").and_then(Value::as_str).unwrap_or("?");
    match e.get("slug").and_then(Value::as_str) {
        Some(slug) if !slug.is_empty() => format!("{slug} ({nid})"),
        _ => format!("({nid})"),
    }
}

/// `fno backlog idea`'s pre-mint prior-work read: distinctive title tokens
/// against the whole graph (every project, done ranked last) and the
/// project's plans dir, printed before the mint. A read, never a gate: a
/// zero is a result, and the filing proceeds either way.
const PRIOR_WORK_TOKEN_CAP: usize = 6;
const PRIOR_WORK_MIN_TOKEN: usize = 5;
const PRIOR_WORK_GRAPH_CAP: usize = 6;
const PRIOR_WORK_PLAN_CAP: usize = 5;
const PRIOR_WORK_PLAN_READ_CAP: u64 = 256 * 1024;

/// Distinctive tokens from a title: lowercase alphanumeric words past the
/// length floor, first-seen order, capped. Short words and repeats carry no
/// recall, and an uncapped token set floods the plans scan on long titles.
fn prior_work_tokens(title: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
    {
        if raw.len() >= PRIOR_WORK_MIN_TOKEN && !out.iter().any(|t| t == raw) {
            out.push(raw.to_string());
        }
        if out.len() == PRIOR_WORK_TOKEN_CAP {
            break;
        }
    }
    out
}

/// OR-widen one pool pass per token, dedup by id keeping first sight. The
/// AND search over a full sentence title rarely matches, so the OR lane is
/// what makes the read worth printing.
fn or_merge(hits: &mut Vec<Value>, pool: &[Value], tokens: &[String]) {
    let mut seen: std::collections::HashSet<String> = hits
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    for token in tokens {
        for e in search_entries(pool, token) {
            if seen.insert(
                e.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ) {
                hits.push(e);
            }
        }
    }
}

/// The graph lane: the same whole-store read `find` serves (every project;
/// archived rows excluded), the AND search first, the OR widen on a miss,
/// capped. An unreadable graph reads as no matches - the read never fails
/// the filing.
fn prior_work_graph_rows(tokens: &[String]) -> Vec<Value> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let graph_path = super::settings::graph_path();
    let Ok(mut entries) = crate::backlog::read_entries(&graph_path) else {
        return Vec::new();
    };
    crate::graph_store::apply_defaults(&mut entries, false);
    crate::graph_store::apply_readiness_overlay(&mut entries);
    let live: Vec<Value> = entries
        .iter()
        .filter(|r| r.get("archived_at").is_none())
        .cloned()
        .collect();
    let mut hits = search_entries(&live, &tokens.join(" "));
    if hits.is_empty() {
        or_merge(&mut hits, &live, tokens);
    }
    hits.truncate(PRIOR_WORK_GRAPH_CAP);
    hits
}

/// The plans lane: one distinct-token count per markdown file in the
/// project's plans dir, best first, name-tiebroken. A missing or unreadable
/// dir reads as no matches.
fn prior_work_plan_lines(anchor: &std::path::Path, tokens: &[String]) -> Vec<(usize, String)> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(dir) = crate::plans_path::plans_content_dir(anchor) else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut scored: Vec<(usize, String)> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > PRIOR_WORK_PLAN_READ_CAP {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lowered = text.to_lowercase();
        let score = tokens
            .iter()
            .filter(|t| lowered.contains(t.as_str()))
            .count();
        if score > 0 {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            scored.push((score, name));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(PRIOR_WORK_PLAN_CAP);
    scored
}

/// The whole read, formatted: a summary head, then graph rows, then plan
/// files. Empty tokens or empty pools still print the head, so the step is
/// visible even when it found nothing.
pub(crate) fn prior_work_lines(title: &str, anchor: &std::path::Path) -> Vec<String> {
    let tokens = prior_work_tokens(title);
    let rows = prior_work_graph_rows(&tokens);
    let plans = prior_work_plan_lines(anchor, &tokens);
    let head = if tokens.is_empty() {
        format!(
            "prior-work search \"{title}\": no distinctive title tokens; \
             run fno backlog find for the loose read"
        )
    } else if rows.is_empty() && plans.is_empty() {
        format!("prior-work search \"{title}\": no matches")
    } else {
        format!(
            "prior-work search \"{title}\": {} graph, {} plan matches",
            rows.len(),
            plans.len()
        )
    };
    let mut out = vec![head];
    for e in &rows {
        out.push(format!(
            "  graph {} {} {} {}",
            format_handle(e),
            e.get("status").and_then(Value::as_str).unwrap_or("?"),
            e.get("project").and_then(Value::as_str).unwrap_or("-"),
            e.get("title").and_then(Value::as_str).unwrap_or(""),
        ));
    }
    for (score, name) in &plans {
        out.push(format!("  plans {name} ({score} title tokens)"));
    }
    out
}

/// The whole ladder. `run` returns the process exit code.
pub fn run(tail: &[String]) -> i32 {
    if tail.iter().any(|t| t == "--help" || t == "-h") {
        println!("{FIND_HELP}");
        return 0;
    }
    let Some(args) = FindArgs::parse(tail) else {
        eprintln!("{FIND_USAGE}");
        return 2;
    };
    let Some(tags) = normalized_tags(&args.tags) else {
        return 2;
    };
    // The Python surface refused here with the guarded metadata reader's
    // message (an external tracker has its own UI; a stale local render is
    // the leak the seam closes). Same bytes, same exit.
    if crate::graph_get::external_backend_selected() {
        eprintln!(
            "fno backlog: find: footnote-minted node metadata (size/type/project/\
             dep/model pins/persistent slug) lives only in the default graph \
             store and is unavailable under an external tracker backend"
        );
        return 2;
    }
    let graph_path = super::settings::graph_path();
    // --fts rides the native FTS5 index (search.rs): the ids come back best
    // first and the pool filters still apply. An unavailable index degrades
    // to the substring search with the same warning the gap shipped with.
    let fts_ids: Option<Vec<String>> = if args.fts && !args.query.is_empty() {
        match super::search::search(&super::api::Store::new(&graph_path), &args.query, None) {
            Ok(ids) => Some(ids),
            Err(_) => {
                eprintln!(
                    "warning: fts unavailable (the native find has no FTS cache); \
                     using substring search"
                );
                None
            }
        }
    } else {
        None
    };
    // The store's own rows, verbatim the way the keeper serves them; the
    // `--json` render re-orders into the model dump shape, the TSV rows read
    // the live fields off the raw row.
    let mut entries = match crate::backlog::read_entries(&graph_path) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!(
                "Could not read the graph cleanly, so '{}' cannot be searched: {}",
                args.query, err
            );
            return super::get_cli::GRAPH_UNREADABLE_EXIT;
        }
    };
    // The search pool carries the defaults tail and the read-time derived
    // status and blocked reason, the same overlay the keeper's served rows
    // answer with. The live pool excludes archived residents; they surface
    // only through the read-through on a live miss (the `archived_at`
    // partition python's include-archived read answered with).
    crate::graph_store::apply_defaults(&mut entries, false);
    crate::graph_store::apply_readiness_overlay(&mut entries);
    let live: Vec<Value> = entries
        .iter()
        .filter(|r| r.get("archived_at").is_none())
        .cloned()
        .collect();
    if let Some(anchor) = args.related {
        return run_related(&live, anchor, &args, &tags);
    }
    if let Some(prefix) = args.notes {
        return run_notes(&live, prefix);
    }
    let mut matched = resolve_against(&live, &args, &fts_ids);
    matched.retain(|e| passes_filters(e, &args, &tags));
    if matched.is_empty() {
        let archived: Vec<Value> = entries
            .iter()
            .filter(|r| r.get("archived_at").is_some())
            .cloned()
            .collect();
        let mut hits = resolve_against(&archived, &args, &fts_ids);
        hits.retain(|e| passes_filters(e, &args, &tags));
        for h in hits {
            let mut row = h.clone();
            if let Some(obj) = row.as_object_mut() {
                obj.insert("_archived".to_string(), Value::Bool(true));
            }
            matched.push(row);
        }
    }
    if matched.is_empty() {
        let what = if args.query.is_empty() {
            tags.join(" + ")
        } else {
            format!("{:?}", args.query)
        };
        eprintln!("fno backlog find: no matches for {what}");
        return 1;
    }
    if args.json_out {
        let ordered: Vec<Value> = matched.iter().map(model_order).collect();
        println!("{}", super::render::py_json_pretty(&Value::Array(ordered)));
        return 0;
    }
    for e in &matched {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            format_handle(e),
            e.get("status").and_then(Value::as_str).unwrap_or("?"),
            e.get("domain").and_then(Value::as_str).unwrap_or("?"),
            e.get("project").and_then(Value::as_str).unwrap_or("-"),
            e.get("title").and_then(Value::as_str).unwrap_or(""),
        );
    }
    0
}

const FIND_USAGE: &str = "fno backlog find: usage: fno backlog find <query> [-d DOMAIN] \
    [-p PROJECT] [-s STATUS] [--source-kind KIND] [-t TAG]... [--fts] [-L LIMIT] [-J], or \
    fno backlog find --related <id>, or fno backlog find --notes <prefix> (--help for detail)";

/// The `--tag` values, normalized. A typo'd tag prints the refusal with the
/// allowed values and returns None, never a silent zero.
fn normalized_tags(raw: &[&str]) -> Option<Vec<String>> {
    let mut tags = Vec::new();
    for t in raw {
        match super::fields::normalize_tag(t) {
            Ok(tag) => tags.push(tag),
            Err(e) => {
                eprintln!("fno backlog find: {e}");
                return None;
            }
        }
    }
    Some(tags)
}

/// `--notes <prefix>`: every progress note whose text starts with the prefix,
/// newest first, verbatim. The Visual Log reader writes the user's taps as
/// `Visual Log: Change at PR ...` notes, so this is how a lead reads the
/// user's recorded complaints back before ruling a look.
fn run_notes(live: &[Value], prefix: &str) -> i32 {
    let mut hits: Vec<(&str, &str, &str)> = Vec::new();
    for entry in live {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("?");
        for note in entry
            .get("progress_notes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = note.get("text").and_then(Value::as_str).unwrap_or("");
            if text.starts_with(prefix) {
                let ts = note.get("ts").and_then(Value::as_str).unwrap_or("");
                hits.push((ts, id, text));
            }
        }
    }
    if hits.is_empty() {
        eprintln!("fno backlog find: no note starts with {prefix:?}");
        return 1;
    }
    hits.sort_by(|a, b| b.0.cmp(a.0));
    for (ts, id, text) in hits {
        println!("{id} {ts}\n{text}\n");
    }
    0
}

/// `--related <id>`: the anchor resolves through the exact tiers, then
/// every other live row ranks on the rollup relatedness score. The filters
/// narrow the ranked rows, never the anchor.
fn run_related(live: &[Value], anchor: &str, args: &FindArgs<'_>, tags: &[String]) -> i32 {
    let Some(entry) = exact_tier(live, anchor).and_then(|c| c.into_iter().next()) else {
        eprintln!("fno backlog find: --related {anchor:?} matches no live node");
        return 1;
    };
    let by_id: std::collections::BTreeMap<&str, &Value> = live
        .iter()
        .filter_map(|e| text_at(e, "id").map(|i| (i, e)))
        .collect();
    let ranked: Vec<(&Value, f64, String)> =
        super::relatedness::related_to(&entry, live, usize::MAX)
            .into_iter()
            .filter_map(|(id, score, reason)| {
                let e = *by_id.get(id.as_str())?;
                passes_filters(e, args, tags).then_some((e, score, reason))
            })
            .take(args.limit)
            .collect();
    if ranked.is_empty() {
        eprintln!("fno backlog find: nothing related to {anchor}");
        return 1;
    }
    if args.json_out {
        let rows: Vec<Value> = ranked
            .iter()
            .map(|(e, score, reason)| {
                serde_json::json!({
                    "id": text_at(e, "id"),
                    "title": text_at(e, "title"),
                    "status": text_at(e, "status"),
                    "tags": super::tags::row_tags(e),
                    "score": score,
                    "reason": reason,
                })
            })
            .collect();
        println!("{}", super::render::py_json_pretty(&Value::Array(rows)));
        return 0;
    }
    for (e, score, reason) in &ranked {
        println!(
            "{}\t{score:.2}\t{}\t{}\t{reason}",
            format_handle(e),
            text_at(e, "status").unwrap_or("?"),
            text_at(e, "title").unwrap_or(""),
        );
    }
    0
}

/// Resolve against one pool: exact tiers, then the ab- prefix tier, then
/// the describe-it search. With `--fts`, the FTS ids order the search lane
/// (best first); an unavailable index keeps the substring fallback. An empty
/// query (a bare `--tag` read) keeps the whole pool, non-done first.
fn resolve_against(
    pool: &[Value],
    args: &FindArgs<'_>,
    fts_ids: &Option<Vec<String>>,
) -> Vec<Value> {
    if args.query.is_empty() {
        let (open, done): (Vec<&Value>, Vec<&Value>) = pool
            .iter()
            .partition(|e| text_at(e, "status") != Some("done"));
        return open.into_iter().chain(done).cloned().collect();
    }
    if let Some(candidates) = exact_tier(pool, args.query) {
        return candidates;
    }
    if args.query.starts_with("ab-") {
        return ab_prefix_hits(pool, args.query);
    }
    if let Some(ids) = fts_ids {
        let mut out = Vec::new();
        for id in ids {
            if let Some(e) = pool
                .iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                out.push(e.clone());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    search_entries(pool, args.query)
}

/// The JSON row's display order: the typed model dump order the Python
/// surface served, captured from its output before deletion. Keys outside
/// the list keep their original order at the end.
const MODEL_KEY_ORDER: &[&str] = &[
    "id",
    "parent",
    "children",
    "slug",
    "title",
    "type",
    "project",
    "cwd",
    "priority",
    "rank",
    "domain",
    "blocked_by",
    "related",
    "locked_by",
    "locked_by_harness",
    "locked_by_harness_session",
    "session_id",
    "locked_at",
    "ownership_defect",
    "completed_at",
    "deferred_at",
    "deferred_reason",
    "deferred_kind",
    "touched_at",
    "has_brief",
    "roadmap_id",
    "vision_path",
    "details",
    "cost_usd",
    "cost_sessions",
    "contained_in",
    "size",
    "model",
    "batch",
    "dispatch_command",
    "dispatch_brief",
    "plan_path",
    "company_work",
    "pr_number",
    "pr_url",
    "additional_prs",
    "merge_status",
    "caused_by",
    "fixes_pr",
    "reverted",
    "artifact_url",
    "completion_note",
    "created_at",
    "superseded_by",
    "supersession",
    "source_harness",
    "source_cwd",
    "source_node_id",
    "source_plan_path",
    "spawned_by_session",
    "spawned_by_harness",
    "spawned_by_cwd",
    "request_origin",
    "origin_evidence",
    "sessions",
    "persisted_status",
    "archived_at",
    "progress_notes",
    "tags",
    "collisions_acknowledged",
    "supersedes",
    "source_kind",
    "source_project",
    "source_session_id",
    "source_inbox_msg",
    "decisions",
    "queued_at",
    "queued_reason",
    "blocked_reason",
    "status",
];

/// The model's Optional fields, null in the dump when the row lacks them.
/// Pinned from the captured output; a schema addition that gains a null
/// default extends this list in the same commit as MODEL_KEY_ORDER.
const MODEL_NULL_DEFAULTS: &[&str] = &[
    "parent",
    "children",
    "rank",
    "locked_by",
    "locked_by_harness",
    "locked_by_harness_session",
    "session_id",
    "locked_at",
    "ownership_defect",
    "completed_at",
    "deferred_at",
    "deferred_reason",
    "deferred_kind",
    "has_brief",
    "roadmap_id",
    "vision_path",
    "details",
    "cost_usd",
    "cost_sessions",
    "contained_in",
    "size",
    "model",
    "batch",
    "dispatch_command",
    "dispatch_brief",
    "plan_path",
    "company_work",
    "pr_number",
    "pr_url",
    "merge_status",
    "caused_by",
    "fixes_pr",
    "reverted",
    "artifact_url",
    "completion_note",
    "superseded_by",
    "supersession",
    "source_harness",
    "source_cwd",
    "source_node_id",
    "source_plan_path",
    "spawned_by_session",
    "spawned_by_harness",
    "spawned_by_cwd",
    "request_origin",
    "origin_evidence",
    "source_project",
    "source_session_id",
    "source_inbox_msg",
    "queued_at",
    "queued_reason",
    "persisted_status",
    "archived_at",
    "blocked_reason",
    "status",
];

/// The model's typed non-Option defaults: a bool that reads false, the list
/// fields that read empty, and the source_kind vocabulary default.
const MODEL_FALSE_DEFAULTS: &[&str] = &["reverted", "has_brief"];
const MODEL_ORGANIC_DEFAULTS: &[&str] = &["source_kind"];
const MODEL_EMPTY_ARRAY_DEFAULTS: &[&str] = &[
    "children",
    "blocked_by",
    "related",
    "collisions_acknowledged",
    "supersedes",
    "decisions",
    "sessions",
    "progress_notes",
    "cost_sessions",
    "tags",
    "additional_prs",
];

/// Reorder one row's keys into the model dump order, extras appended, the
/// model's typed defaults present: null for the Optionals, false for the
/// bool, empty lists for the list fields, and persisted_status mirrors the
/// row's stored status word.
fn model_order(row: &Value) -> Value {
    let Some(obj) = row.as_object() else {
        return row.clone();
    };
    let mut out = serde_json::Map::new();
    for key in MODEL_KEY_ORDER {
        if let Some(v) = obj.get(*key) {
            out.insert((*key).to_string(), v.clone());
        } else if *key == "persisted_status" {
            out.insert(
                (*key).to_string(),
                obj.get("status").cloned().unwrap_or(Value::Null),
            );
        } else if MODEL_FALSE_DEFAULTS.contains(key) {
            out.insert((*key).to_string(), Value::Bool(false));
        } else if MODEL_ORGANIC_DEFAULTS.contains(key) {
            out.insert((*key).to_string(), Value::String("organic".to_string()));
        } else if MODEL_EMPTY_ARRAY_DEFAULTS.contains(key) {
            out.insert((*key).to_string(), Value::Array(Vec::new()));
        } else if MODEL_NULL_DEFAULTS.contains(key) {
            out.insert((*key).to_string(), Value::Null);
        }
    }
    for (k, v) in obj {
        if !MODEL_KEY_ORDER.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_search_orders_non_done_first_and_matches_every_token() {
        let pool = vec![
            json!({"id": "x-1", "title": "beta done node", "status": "done"}),
            json!({"id": "x-2", "title": "alpha live node", "status": "ready"}),
        ];
        let hits = search_entries(&pool, "node");
        assert_eq!(hits[0]["id"], json!("x-2"));
        assert_eq!(hits.len(), 2);
        assert!(!search_entries(&pool, "alpha live").is_empty());
        let no = search_entries(&pool, "");
        assert!(no.is_empty());
    }

    #[test]
    fn the_ab_prefix_tier_matches_partial_hex_never_full_or_malformed() {
        let pool = vec![json!({"id": "ab-bbbbbbbb", "title": "Beta"})];
        assert_eq!(ab_prefix_hits(&pool, "ab-bbbb").len(), 1);
        assert!(ab_prefix_hits(&pool, "ab-bbbbbbbb").is_empty());
        assert!(ab_prefix_hits(&pool, "ab-nothex").is_empty());
    }

    #[test]
    fn prior_work_tokens_dedup_cap_and_the_or_merge_dedups() {
        let tokens = prior_work_tokens("Before building or filing, an agent searches prior work");
        assert_eq!(
            tokens,
            vec!["before", "building", "filing", "agent", "searches", "prior"]
        );
        let capped = prior_work_tokens("alpha bravo charlie delta foxtrot golf hotel india");
        assert_eq!(capped.len(), PRIOR_WORK_TOKEN_CAP);
        assert_eq!(capped[0], "alpha");
        assert_eq!(capped[5], "hotel");
        let pool = vec![
            json!({"id": "x-1", "title": "boarding the plane", "status": "ready"}),
            json!({"id": "x-2", "title": "the search plan", "status": "done"}),
            json!({"id": "x-3", "title": "plane search landing", "status": "done"}),
        ];
        let mut merged = search_entries(&pool, "boarding");
        or_merge(
            &mut merged,
            &pool,
            &["plane".to_string(), "search".to_string()],
        );
        let ids: Vec<&str> = merged.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["x-1", "x-3", "x-2"]);
    }
}
