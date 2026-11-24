//! The org view's pages: a seat, the FEED of questions to the user, and
//! SYSTEM's machine table. A field with no reading names its basis.

use super::org_seats::{Fleet, Lead, Machine, Seat};
use super::org_tabs::{
    ago, blank, dur, epoch, fit, glyph, kv, meta, not_sampled, rule, sampled, status_role, Lb, Line,
};
use crate::text_style::TextStyle;

const NOT_PUBLISHED: &str = "not published";

fn or_np(v: Option<&str>) -> String {
    v.map(str::to_string)
        .unwrap_or_else(|| NOT_PUBLISHED.into())
}

/// The command that attaches a seat from its own machine.
pub(crate) fn attach_command(s: &Seat) -> String {
    format!("fno agent session attach {}", s.short)
}

pub(crate) fn seat(m: &Machine, p: &Lead, s: &Seat, width: usize, now: u64) -> Vec<Line> {
    let inner = width.saturating_sub(17);
    let mut out = vec![
        Lb::default()
            .s(" seat ", TextStyle::Meta)
            .s(s.name.clone(), TextStyle::Item)
            .s(" \u{b7} ", TextStyle::Meta)
            .s(
                format!("{} {}", glyph(s.status), s.status),
                status_role(s.status),
            )
            .done(),
        blank(),
        rule("Context", width),
        kv(
            "context",
            s.context_pct
                .map_or_else(|| NOT_PUBLISHED.into(), |p| format!("{p}% used")),
            TextStyle::Item,
        ),
        kv("runtime", or_np(s.harness.as_deref()), TextStyle::Item),
    ];
    out.push(if s.local.is_some() {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), TextStyle::Meta)
            .s("enter attaches here \u{b7} ", TextStyle::Title)
            .s(attach_command(s), TextStyle::Item)
            .done()
    } else {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), TextStyle::Meta)
            .s(attach_command(s), TextStyle::Item)
            .s(format!(" on {}", m.host), TextStyle::Meta)
            .done()
    });
    out.push(blank());
    out.push(rule("Current activity", width));
    out.push(kv("activity", s.status, status_role(s.status)));
    out.push(kv(
        "for",
        s.status_age_s.map_or_else(|| "unknown".into(), dur),
        TextStyle::Item,
    ));
    out.push(kv(
        "last turn",
        ago(s.last_message_at.as_deref(), now).unwrap_or_else(|| NOT_PUBLISHED.into()),
        TextStyle::Item,
    ));
    out.push(blank());
    out.push(rule("Token use", width));
    out.push(kv(
        "compactions",
        s.compactions
            .map_or_else(|| NOT_PUBLISHED.into(), |n| n.to_string()),
        TextStyle::Item,
    ));
    for (key, value) in [
        ("input", s.input_tokens),
        ("output", s.output_tokens),
        ("cache read", s.cache_read_tokens),
        ("cache write", s.cache_write_tokens),
    ] {
        out.push(kv(
            key,
            value.map_or_else(|| NOT_PUBLISHED.into(), |n| super::row_meter::token_cell(n)),
            TextStyle::Item,
        ));
    }
    out.push(kv(
        "cost",
        s.cost_cents
            .map_or_else(|| NOT_PUBLISHED.into(), |c| super::row_meter::cost_cell(c)),
        TextStyle::Item,
    ));
    out.push(blank());
    match &s.work {
        Some(w) => {
            out.push(rule(&format!("Current work \u{b7} {}", w.id), width));
            let facts = [
                Some(w.id.clone()),
                w.status.clone(),
                w.priority.clone(),
                w.difficulty.clone(),
            ];
            out.push(kv(
                "node",
                facts
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" \u{b7} "),
                TextStyle::Item,
            ));
            out.push(kv(
                "title",
                fit(w.title.as_deref().unwrap_or("not in this graph"), inner)
                    .trim_end()
                    .to_string(),
                TextStyle::Item,
            ));
            out.push(match &w.plan {
                Some(plan) => kv(
                    "plan",
                    fit(plan, inner).trim_end().to_string(),
                    TextStyle::Item,
                ),
                None => kv("plan", "no plan_path", TextStyle::Meta),
            });
            if let Some(pr) = w.pr {
                out.push(kv("pr", format!("#{pr}"), TextStyle::Item));
            }
            if let Some((id, title)) = &w.epic {
                let text = match title {
                    Some(t) => format!("{id} \u{b7} {t}"),
                    None => id.clone(),
                };
                out.push(kv(
                    "epic",
                    fit(&text, inner).trim_end().to_string(),
                    TextStyle::Item,
                ));
            }
        }
        None => {
            out.push(rule("Current work", width));
            out.push(kv("node", "none", TextStyle::Meta));
        }
    }
    out.push(blank());
    out.push(rule("Last message", width));
    match s.last_message.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(text) => {
            for line in text.lines().take(6) {
                out.push(
                    Lb::default()
                        .s("   \u{2502} ", TextStyle::Meta)
                        .s(fit(line, inner).trim_end().to_string(), TextStyle::Item)
                        .done(),
                );
            }
            if let Some(a) = ago(s.last_message_at.as_deref(), now) {
                out.push(meta(format!("   \u{b7} last turn {a}")));
            }
        }
        None => {
            let stamp = match ago(s.last_message_at.as_deref(), now) {
                Some(a) => format!("last turn {a}"),
                None => NOT_PUBLISHED.into(),
            };
            out.push(
                Lb::default()
                    .s("   \u{2502} ", TextStyle::Meta)
                    .s(stamp, TextStyle::Item)
                    .done(),
            );
        }
    }
    out.push(blank());
    out.push(rule("Seat", width));
    out.push(kv(
        "machine",
        format!("{} ({})", m.host, m.id),
        TextStyle::Item,
    ));
    out.push(kv("lead", p.name.clone(), TextStyle::Item));
    out.push(kv("project", or_np(s.project.as_deref()), TextStyle::Item));
    out.push(kv(
        "session",
        or_np(s.session_id.as_deref()),
        TextStyle::Item,
    ));
    out.push(kv(
        "model",
        match (&s.model, &s.model_basis) {
            (Some(model), Some(basis)) => format!("{model} ({basis})"),
            (Some(model), None) => model.clone(),
            _ => NOT_PUBLISHED.into(),
        },
        TextStyle::Item,
    ));
    out.push(kv(
        "started",
        or_np(s.started_at.as_deref()),
        TextStyle::Item,
    ));
    out.push(kv(
        "alive",
        epoch(s.started_at.as_deref())
            .map_or_else(|| NOT_PUBLISHED.into(), |t| dur(now as i64 - t)),
        TextStyle::Item,
    ));
    out.push(kv(
        "cwd",
        fit(&or_np(s.cwd.as_deref()), inner).trim_end().to_string(),
        TextStyle::Item,
    ));
    out.push(blank());
    out.push(meta(match (&s.local, m.local) {
        (Some(_), _) => {
            "   enter attaches \u{b7} space peeks \u{b7} x stops \u{b7} P portals \u{b7} u goes up to the lead"
        }
        (None, true) => {
            "   no pane here carries this session: run the terminal command \u{b7} u goes up to the lead"
        }
        (None, false) => {
            "   space peeks \u{b7} P opens a viewer pane \u{b7} u goes up to the lead"
        }
    }));
    out
}

pub(crate) fn feed(
    fold: Option<&crate::needs_overlay::QuestionsFold>,
    requests: Option<&[crate::requests_model::RequestRow]>,
    error: Option<&str>,
    grouped: bool,
    width: usize,
) -> Vec<Line> {
    let mut out = vec![
        Lb::default().s(" fleet / the user", TextStyle::Meta).done(),
        blank(),
        Lb::default().s(" Your thoughts", TextStyle::Title).done(),
    ];
    out.extend(requests_section(requests, error, grouped, width));
    out.push(blank());
    out.push(Lb::default().s(" Waiting on you", TextStyle::Title).done());
    match fold {
        None => out.push(meta("   questions not read yet")),
        Some(f) if f.items.is_empty() => out.push(meta("   no open question")),
        Some(f) => {
            for q in &f.items {
                let title = if q.title.is_empty() {
                    "(no title)"
                } else {
                    q.title.as_str()
                };
                let kind = if q.kind.is_empty() {
                    "question"
                } else {
                    q.kind.as_str()
                };
                out.push(
                    Lb::default()
                        .s("  \u{25a0} ", TextStyle::Meta)
                        .s(format!("[{kind}] "), TextStyle::Meta)
                        .s(
                            fit(title, width.saturating_sub(kind.chars().count() + 8))
                                .trim_end()
                                .to_string(),
                            TextStyle::Item,
                        )
                        .done(),
                );
                let mut to = "      To: user".to_string();
                if let Some(a) = &q.asker {
                    to.push_str(&format!(" \u{b7} From: {}", a.handle));
                }
                if let Some(node) = &q.node {
                    to.push_str(&format!(" \u{b7} node {node}"));
                }
                out.push(meta(to));
                out.push(blank());
            }
        }
    }
    out.push(meta(
        " Viewing is not approval. Rule on the question board.",
    ));
    out
}

/// The HQ Thoughts rows above the questions: rank order, or the by-topic
/// groups, each row with its ask count, dates, node state and flag.
pub(crate) fn requests_section(
    requests: Option<&[crate::requests_model::RequestRow]>,
    error: Option<&str>,
    grouped: bool,
    width: usize,
) -> Vec<Line> {
    let Some(rows) = requests else {
        return vec![meta("   thoughts not read yet")];
    };
    let mut out = Vec::new();
    if rows.is_empty() {
        out.push(meta(match error {
            Some(e) => format!("   thoughts not read: {e}"),
            None => "   no asks recorded yet".to_string(),
        }));
        return out;
    }
    if grouped {
        for g in crate::client::messages_view::asks_topics::request_groups(rows) {
            let n = g.rows.len();
            let s = if n == 1 { "" } else { "s" };
            out.push(kv(&g.name, format!("{n} ask{s}"), TextStyle::Item));
            for i in &g.rows {
                if let Some(r) = rows.get(*i) {
                    out.extend(request_row_lines(r, width));
                }
            }
        }
    } else {
        for r in rows {
            out.extend(request_row_lines(r, width));
        }
    }
    if let Some(e) = error {
        out.push(meta(format!(
            "   last read failed: {e}; showing the last good rows"
        )));
    }
    out.push(meta(
        "   ranked by asks x days unaddressed \u{b7} t groups by topic",
    ));
    out
}

/// Render-safe text: markdown heading markers a pasted ask carries read as
/// prose here; the web page renders the same markers as a real heading.
fn strip_hashes(s: &str) -> String {
    s.lines()
        .map(|line| {
            let t = line.trim_start();
            let hashes = t.chars().take_while(|c| *c == '#').count();
            let rest = t.get(hashes..).unwrap_or(t);
            if hashes > 0 && hashes <= 6 && (rest.starts_with(' ') || rest.is_empty()) {
                rest.trim_start()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One row: the ask text with its flag, then dim lines with the count,
/// node state, dates, the judged outcome, the reply excerpt and the
/// response chain.
fn request_row_lines(r: &crate::requests_model::RequestRow, width: usize) -> Vec<Line> {
    let text = strip_hashes(if r.text.is_empty() {
        "(empty ask)"
    } else {
        r.text.as_str()
    });
    let ask_count = r.ask_count.max(1);
    let asked_word = if ask_count == 1 {
        "asked once".to_string()
    } else {
        format!("asked {ask_count} times")
    };
    let days = r.days_unaddressed.unwrap_or(0);
    let open_word = if days <= 0 {
        "new today".to_string()
    } else if days == 1 {
        "open 1 day".to_string()
    } else {
        format!("open {days} days")
    };
    let node = r.node.as_deref().unwrap_or("");
    let status = r.node_status.as_deref();
    let node_word = match (node.is_empty(), status) {
        (true, _) => "no linked node".to_string(),
        (false, Some(s)) => format!("{node} {s}"),
        (false, None) => node.to_string(),
    };
    // The state word joins the line only when the row's own fields do not
    // already say it: no_node reads through "no linked node".
    let state = r.state.as_str();
    let state_word = match state {
        "stalled" => "stalled".to_string(),
        "regression" => "asked again after done".to_string(),
        _ => String::new(),
    };
    let first = r.first_ts.as_str();
    let last = r.last_ts.as_str();
    let window = if first.is_empty() || first.get(0..10) == last.get(0..10) {
        String::new()
    } else {
        format!(
            "window {} to {}",
            first.get(0..10).unwrap_or(first),
            last.get(0..10).unwrap_or(last)
        )
    };
    let reply = strip_hashes(r.reply_excerpt.as_str());
    let meta_word = [asked_word, open_word, node_word, state_word, window]
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let head = Lb::default()
        .s("  ", TextStyle::Meta)
        .s(
            fit(&text, width.saturating_sub(4)).trim_end().to_string(),
            TextStyle::Item,
        )
        .done();
    let mut out2 = vec![head];
    out2.push(meta(format!("      {meta_word}")));
    let outcome_word = match r.outcome.as_deref() {
        Some("solved") => Some("solved"),
        Some("lost") => Some("lost"),
        Some("forgotten") => Some("acknowledged but forgotten"),
        Some("open") => Some("open"),
        _ => None,
    };
    if let Some(word) = outcome_word {
        let why = r.outcome_why.as_deref().unwrap_or("");
        let line = if why.is_empty() {
            word.to_string()
        } else {
            format!("{word} \u{b7} {why}")
        };
        out2.push(meta(format!("      outcome: {line}")));
    }
    if !reply.is_empty() {
        out2.push(meta(format!("      reply: {reply}")));
    }
    // The responses line names the count and the latest reply's handle, so
    // it stays on screen; a chain with no reply at all reads through the
    // outcome line's "no reply" and lists nothing.
    let replied: Vec<_> = r
        .responses
        .iter()
        .filter(|x| !strip_hashes(x.reply.as_str()).trim().is_empty())
        .collect();
    if let Some(latest) = replied.last() {
        let handle = if latest.name.is_empty() {
            "unnamed session".to_string()
        } else {
            latest.name.clone()
        };
        out2.push(meta(format!(
            "      responses: {} \u{b7} latest {} {}",
            replied.len(),
            latest.ts.get(0..10).unwrap_or(&latest.ts),
            handle
        )));
    }
    out2
}

pub(crate) fn system(fleet: &Fleet, width: usize) -> Vec<Line> {
    let mut out = vec![
        Lb::default().s(" machines", TextStyle::Meta).done(),
        blank(),
        Lb::default()
            .s(
                format!(
                    " {}{}{}{}{}{}CPU",
                    fit("Host", 12),
                    fit("State", 9),
                    fit("Beat", 8),
                    fit("Work", 6),
                    fit("Cap", 15),
                    fit("Memory", 13)
                ),
                TextStyle::Title,
            )
            .done(),
    ];
    for m in &fleet.machines {
        out.push(
            Lb::default()
                .s(format!(" {}", fit(&m.host, 12)), TextStyle::Item)
                .s(
                    fit(if m.fresh { "online" } else { "stale" }, 9),
                    if m.fresh {
                        TextStyle::Item
                    } else {
                        TextStyle::Title
                    },
                )
                .s(fit(&dur(m.age_s), 8), TextStyle::Meta)
                .s(
                    fit(if m.takes_work { "yes" } else { "no" }, 6),
                    TextStyle::Meta,
                )
                .s(
                    fit(
                        &m.cap
                            .map_or_else(|| NOT_PUBLISHED.into(), |c| c.to_string()),
                        15,
                    ),
                    TextStyle::Meta,
                )
                .s(
                    fit(
                        &m.mem_used
                            .map_or_else(|| "not sampled".into(), |u| format!("{:.0}%", u * 100.0)),
                        13,
                    ),
                    TextStyle::Meta,
                )
                .s(
                    m.cpu
                        .map_or_else(|| "not sampled".into(), |c| format!("{c:.0}%")),
                    TextStyle::Meta,
                )
                .whole(&super::org_board::host_key(m))
                .done(),
        );
    }
    out.push(blank());
    out.push(rule("Connections", width));
    out.push(meta(format!(
        "   roster: fleet_machine rows on the shared primary \u{b7} {} machine{}",
        fleet.machines.len(),
        if fleet.machines.len() == 1 { "" } else { "s" }
    )));
    out.push(meta("   read it: fno agent fleet top --machines"));
    if let Some(e) = &fleet.seat_error {
        out.push(meta(format!("   seat facts not read: {e}")));
    }
    out.push(blank());
    out.push(rule("Health", width));
    if fleet.machines.iter().all(|m| sampled(m).is_none()) {
        out.push(not_sampled());
    } else {
        for m in &fleet.machines {
            out.push(kv(
                &m.host,
                sampled(m).unwrap_or("not sampled").to_string(),
                TextStyle::Item,
            ));
        }
    }
    out
}

/// A peek's transcript lines, already stripped of control bytes.
pub(crate) fn peek_page(lines: &[String]) -> Vec<Line> {
    let mut out = vec![meta(" read-only \u{b7} esc closes"), blank()];
    out.extend(
        lines
            .iter()
            .map(|l| Lb::default().s(format!(" {l}"), TextStyle::Item).done()),
    );
    out
}

/// What HQ says while the fleet holds no seat: what it shows once work
/// runs, then the next step. One place, so the first-run copy swaps in one
/// edit.
const EMPTY_HEAD: &str = "Nothing is running yet.";
const EMPTY_BODY: &str = "HQ lists your leads, and the seats under each, once work runs.";

/// The mark's indent, matching the text under it.
const MARK_INDENT: usize = 3;

/// The F[no] mark above the copy: the splash's pixel art where the pane is
/// wide enough, the one-row wordmark where it is not. Ink is the text color
/// (`Item`), so every theme and both grounds read it.
fn empty_mark(width: usize) -> Vec<Line> {
    let pad = " ".repeat(MARK_INDENT);
    if width < MARK_INDENT + crate::splash::MARK_CELLS + 1 {
        return vec![
            Lb::default()
                .s(format!("{pad}\u{FF26}"), TextStyle::Title)
                .s("[no]", TextStyle::Tab)
                .done(),
            blank(),
        ];
    }
    let mut out: Vec<Line> = crate::splash::mark_rows(crate::splash::hq_mark_clock())
        .into_iter()
        .map(|row| {
            Lb::default()
                .s(format!("{pad}{row}"), TextStyle::Item)
                .done()
        })
        .collect();
    out.push(blank());
    out
}

pub(crate) fn empty_state(width: usize) -> Vec<Line> {
    let key = |k: &str, what: &str| {
        Lb::default()
            .s(format!("   {k}"), TextStyle::Item)
            .s(format!("   {what}"), TextStyle::Meta)
            .done()
    };
    let mut out = vec![blank()];
    out.extend(empty_mark(width));
    out.extend([
        Lb::default()
            .s(format!("   {EMPTY_HEAD}"), TextStyle::Item)
            .done(),
        meta(format!("   {EMPTY_BODY}")),
        blank(),
        key("n", "start a session: type a task"),
        key("L", "start a lead"),
    ]);
    out
}

pub(crate) fn keys_help() -> Vec<Line> {
    [
        " Org keys",
        "",
        "   j/k or arrows    move in the explorer",
        "   h/l or arrows    change tab \u{b7} 1-5 pick a tab",
        "   u or backspace   go up: seat, lead, machine, fleet",
        "   PageUp/PageDown  scroll the right pane",
        "   enter            open a lead \u{b7} attach a seat on this machine",
        "   space x P        peek, stop or portal a seat on this machine",
        "   space P          peek or view a seat on another machine, over ssh",
        "   n                start a session: the composer prefix+i opens",
        "   L                start a lead: the composer, seeded with /fno:lead",
        "   s                spawn a seat on the selected machine \u{b7} esc cancels the prompt",
        "   r                review the PR diff of a seat whose node has a PR",
        "   W opens the HQ web page \u{b7} F full screen \u{b7} V cycle views \u{b7} ? keys \u{b7} esc close",
    ]
    .into_iter()
    .map(meta)
    .collect()
}
