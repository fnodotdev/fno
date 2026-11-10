//! The rule book: standing law grouped by section, one current ruling per
//! topic. Pure functions over the rows `decisions_overlay` already reads, so
//! the CLI listing keeps its byte-identical contract. Selection is by subject
//! shape and lifecycle, never by lane: the coordination lane holds standing
//! rules too (`look-rolls-forward`), and the law lane holds one-off approvals.

use crate::decisions_overlay::DecisionRow;

/// Door subjects name a live constraint on one PR or lane, not a rule a
/// person lives by; they never enter the book.
const DOOR_PREFIXES: [&str; 6] = [
    "merge-grant:",
    "role-grant:",
    "merge-freeze:",
    "pr-hold:",
    "authorized-merge",
    "merge-guard-pin",
];

/// A row whose body opens with a verdict word records something that
/// happened once (an approval at a head), however its subject reads.
const ONEOFF_OPENINGS: [&str; 5] = ["approved", "resolved", "merged", "rejected", "denied"];

/// The lifecycle values that make a row the current ruling of its topic.
/// `unscoped` is the coordination lane's free-floating row: no node expiry
/// to derive, so nothing closes it.
fn is_current(lifecycle: &str) -> bool {
    lifecycle == "live" || lifecycle == "unscoped"
}

/// The seven book sections in paint order; Other always paints last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Release,
    MergingReview,
    LooksUi,
    QuestionsMail,
    Spend,
    MachinesSpawning,
    Other,
}

pub const BOOK_SECTIONS: [Section; 7] = [
    Section::Release,
    Section::MergingReview,
    Section::LooksUi,
    Section::QuestionsMail,
    Section::Spend,
    Section::MachinesSpawning,
    Section::Other,
];

impl Section {
    pub fn title(&self) -> &'static str {
        match self {
            Section::Release => "Release",
            Section::MergingReview => "Merging and review",
            Section::LooksUi => "Looks and UI",
            Section::QuestionsMail => "Questions and mail",
            Section::Spend => "Spend",
            Section::MachinesSpawning => "Machines and spawning",
            Section::Other => "Other",
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        match self {
            Section::Release => &[
                "release",
                "version",
                "pypi",
                "publish",
                "launch",
                "changelog",
                "proof",
                "bump",
                "tag",
            ],
            Section::MergingReview => &[
                "merge", "review", "guard", "rebase", "branch", "conflict", "ship", "pr", "pull",
            ],
            Section::LooksUi => &[
                "look", "ui", "paint", "theme", "view", "board", "layout", "design", "phone",
                "style", "render", "visual",
            ],
            Section::QuestionsMail => &[
                "mail",
                "question",
                "inbox",
                "ask",
                "notify",
                "answer",
                "outstanding",
                "message",
            ],
            Section::Spend => &[
                "spend", "cost", "budget", "quota", "money", "usd", "price", "billing",
            ],
            Section::MachinesSpawning => &[
                "spawn",
                "machine",
                "worker",
                "agent",
                "fleet",
                "pane",
                "thread",
                "mux",
                "zai",
                "lane",
                "harness",
                "model",
                "provider",
                "session",
                "claim",
                "watchdog",
                "footprint",
                "background",
                "ci",
                "test",
                "deploy",
                "repo",
                "doc",
                "skill",
                "hook",
                "plan",
                "backlog",
                "codex",
                "claude",
                "opencode",
                "glm",
                "config",
                "command",
                "cli",
                "crate",
                "rust",
                "python",
            ],
            Section::Other => &[],
        }
    }
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn mentions(section: Section, toks: &[String]) -> bool {
    section
        .keywords()
        .iter()
        .any(|k| toks.iter().any(|t| t == k))
}

/// The section a ruling paints under: the subject slug decides first, the
/// ruling text breaks ties. Spend is checked before machines so a `quota`
/// slug stays about money even when the text names workers.
pub fn section_for(subject: &str, text: &str) -> Section {
    let subject_toks = tokens(subject);
    let text_toks = tokens(text);
    for &s in BOOK_SECTIONS.iter().take(5) {
        if mentions(s, &subject_toks) {
            return s;
        }
    }
    // A token that starts with a digit and carries a dot is a version
    // (`0.4.7`), which belongs to Release.
    if subject_toks
        .iter()
        .any(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
    {
        return Section::Release;
    }
    // A machines-and-spawning slug stays there even when its face text
    // mentions another section in passing (`pane-limit-per-worker` whose text
    // says "ask"), so the subject outranks every text pass.
    if mentions(Section::MachinesSpawning, &subject_toks) {
        return Section::MachinesSpawning;
    }
    for &s in BOOK_SECTIONS.iter().take(5) {
        if mentions(s, &text_toks) {
            return s;
        }
    }
    if mentions(Section::MachinesSpawning, &text_toks) {
        return Section::MachinesSpawning;
    }
    Section::Other
}

/// The plain-words title of a topic: its slug with separators as spaces.
pub fn topic_title(slug: &str) -> String {
    slug.split(|c: char| c == '-' || c == '_' || c == '.')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The rows the book reads, in listing order. A row qualifies when its
/// subject is free text (a topic, not a node or PR attachment), its lane is
/// law or coordination, nothing retracted or retired it, no door subject
/// names a live constraint, and its body is not a one-off verdict.
pub fn standing(rows: &[DecisionRow]) -> Vec<&DecisionRow> {
    rows.iter()
        .filter(|r| {
            if r.lane != "law" && r.lane != "coord" {
                return false;
            }
            if !is_current_lifecycle_candidate(r) {
                return false;
            }
            let st = r.subject_text.trim();
            if st.is_empty() || !r.subject_node.trim().is_empty() || !r.subject_pr.trim().is_empty()
            {
                return false;
            }
            let subject = r.subject.trim();
            if DOOR_PREFIXES
                .iter()
                .any(|p| subject.starts_with(p) || st.starts_with(p))
            {
                return false;
            }
            let body = r.decision.trim();
            let lower = body.to_ascii_lowercase();
            if ONEOFF_OPENINGS.iter().any(|w| lower.starts_with(w)) {
                return false;
            }
            true
        })
        .collect()
}

/// Retired and retracted rows are dead for selection; every other lifecycle
/// still groups (a superseded row folds under its topic's face), so the
/// lifecycle cut here is only the hard-dead pair.
fn is_current_lifecycle_candidate(r: &DecisionRow) -> bool {
    r.lifecycle != "retracted" && r.lifecycle != "retired"
}

/// One topic of the book: the current ruling as its face, older rows of the
/// same slug folded under it as a count.
#[derive(Debug, Clone)]
pub struct Topic<'a> {
    pub slug: String,
    pub title: String,
    pub face: &'a DecisionRow,
    /// Superseded, expired and otherwise non-current rows of this slug.
    pub folded: Vec<&'a DecisionRow>,
    pub section: Section,
}

/// The book: every topic with a current ruling, grouped by section in paint
/// order, topics inside a section newest-first. A slug whose rows are all
/// dead is omitted; the book never shows a dead face.
pub fn book(rows: &[DecisionRow]) -> Vec<(Section, Vec<Topic<'_>>)> {
    let standing_rows = standing(rows);
    let mut by_slug: std::collections::HashMap<String, Vec<&DecisionRow>> =
        std::collections::HashMap::new();
    for r in standing_rows {
        let slug = if r.subject_text.trim().is_empty() {
            r.subject.trim()
        } else {
            r.subject_text.trim()
        };
        by_slug
            .entry(slug.to_ascii_lowercase())
            .or_default()
            .push(r);
    }
    let mut topics: Vec<Topic> = by_slug
        .into_iter()
        .filter_map(|(slug, mut rows)| {
            rows.sort_by(|a, b| {
                b.ts.cmp(&a.ts)
                    .then_with(|| b.decision_id.cmp(&a.decision_id))
            });
            let face = rows.iter().find(|r| is_current(&r.lifecycle))?;
            let folded: Vec<&DecisionRow> = rows
                .iter()
                .filter(|r| !std::ptr::eq(**r, *face))
                .copied()
                .collect();
            let section = section_for(&slug, &face.decision);
            Some(Topic {
                title: topic_title(&slug),
                slug,
                face,
                folded,
                section,
            })
        })
        .collect();
    topics.sort_by(|a, b| b.face.ts.cmp(&a.face.ts).then_with(|| a.slug.cmp(&b.slug)));
    let mut out: Vec<(Section, Vec<Topic>)> =
        BOOK_SECTIONS.iter().map(|&s| (s, Vec::new())).collect();
    for t in topics {
        if let Some((_, bucket)) = out.iter_mut().find(|(s, _)| *s == t.section) {
            bucket.push(t);
        }
    }
    out.retain(|(_, ts)| !ts.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions_overlay::DecisionRow;

    fn row(
        id: &str,
        subject: &str,
        subject_text: &str,
        node: &str,
        pr: &str,
        lane: &str,
        lifecycle: &str,
        ts: &str,
        decision: &str,
    ) -> DecisionRow {
        DecisionRow {
            decision_id: id.into(),
            decision: decision.into(),
            subject: subject.into(),
            ts: ts.into(),
            lifecycle: lifecycle.into(),
            lane: lane.into(),
            subject_node: node.into(),
            subject_pr: pr.into(),
            subject_text: subject_text.into(),
            ..Default::default()
        }
    }

    fn law(id: &str, slug: &str, ts: &str, decision: &str) -> DecisionRow {
        row(id, slug, slug, "", "", "law", "live", ts, decision)
    }

    #[test]
    fn standing_keeps_free_text_and_drops_attachments() {
        let rows = vec![
            law(
                "d-1",
                "merge-guard-purpose",
                "2026-11-01T00:00:00Z",
                "A merge guard stops a PR only when it does not solve the plan's problems.",
            ),
            row(
                "d-2",
                "node-ref",
                "",
                "node-ref",
                "",
                "law",
                "live",
                "2026-11-01T01:00:00Z",
                "Approve the look at e7c4.",
            ),
            row(
                "d-3",
                "approval",
                "",
                "",
                "#4643",
                "law",
                "live",
                "2026-11-01T02:00:00Z",
                "Approved: PR 4643 at 1ea1.",
            ),
            row(
                "d-4",
                "push",
                "push",
                "",
                "",
                "unattributed",
                "live",
                "2026-11-01T03:00:00Z",
                "Retry after the rate limit.",
            ),
            row(
                "d-5",
                "visual approval PR 4643",
                "visual approval PR 4643",
                "",
                "",
                "law",
                "live",
                "2026-11-01T04:00:00Z",
                "Approved: PR 4643 at 1ea1; merge when green.",
            ),
            row(
                "d-6",
                "merge-grant:fno#4665",
                "merge-grant:fno#4665",
                "",
                "",
                "law",
                "live",
                "2026-11-01T05:00:00Z",
                "Grant the merge at head fbcf.",
            ),
            row(
                "d-7",
                "old-rule",
                "old-rule",
                "",
                "",
                "law",
                "retracted",
                "2026-11-01T06:00:00Z",
                "A rule someone retracted.",
            ),
            row(
                "d-8",
                "look-rolls-forward",
                "look-rolls-forward",
                "",
                "",
                "coord",
                "unscoped",
                "2026-11-01T07:00:00Z",
                "A look approval rolls forward to every later head until merge.",
            ),
        ];
        let kept = standing(&rows);
        let ids: Vec<&str> = kept.iter().map(|r| r.decision_id.as_str()).collect();
        assert_eq!(ids, ["d-1", "d-8"]);
    }

    #[test]
    fn book_folds_older_rows_under_the_face_and_omits_dead_topics() {
        let rows = vec![
            law(
                "d-a",
                "guard-diet",
                "2026-11-02T00:00:00Z",
                "The merge guard runs its architect pass on by default.",
            ),
            row(
                "d-b",
                "guard-diet",
                "guard-diet",
                "",
                "",
                "law",
                "superseded",
                "2026-11-01T00:00:00Z",
                "The older guard diet.",
            ),
            row(
                "d-c",
                "dead-topic",
                "dead-topic",
                "",
                "",
                "law",
                "superseded",
                "2026-11-01T12:00:00Z",
                "Nothing current on this slug.",
            ),
        ];
        let sections = book(&rows);
        let all: Vec<&Topic> = sections.iter().flat_map(|(_, ts)| ts.iter()).collect();
        assert_eq!(all.len(), 1, "dead topic omitted, one face");
        let only = all.first().expect("one topic");
        assert_eq!(only.slug, "guard-diet");
        assert_eq!(only.face.decision_id, "d-a");
        assert_eq!(only.folded.len(), 1);
    }

    #[test]
    fn classifier_places_the_measured_specimens() {
        assert_eq!(
            section_for(
                "merge-guard-purpose",
                "A merge guard stops a PR only when it does not solve the plan's problems."
            ),
            Section::MergingReview
        );
        assert_eq!(
            section_for(
                "look-rolls-forward",
                "A look approval rolls forward to every later head until merge."
            ),
            Section::LooksUi
        );
        assert_eq!(
            section_for("public-launch-version", "The public launch is v0.5.0."),
            Section::Release
        );
        assert_eq!(
            section_for(
                "codex-worker-start",
                "Codex workers start from the repo root, not a worktree."
            ),
            Section::MachinesSpawning
        );
        assert_eq!(
            section_for("ask-length", "Keep the ask short."),
            Section::QuestionsMail
        );
        assert_eq!(
            section_for("epic-size", "An epic carries its own size."),
            Section::Other
        );
        assert_eq!(
            section_for(
                "0.4.7-seeds",
                "The fresh public repo seeds at this version."
            ),
            Section::Release,
            "version token paints Release"
        );
        assert_eq!(
            section_for("agents-quota", "The lanes cap what the fleet may spend."),
            Section::Spend,
            "spend outranks machines on a quota slug"
        );
        assert_eq!(topic_title("look-rolls-forward"), "look rolls forward");
        assert_eq!(
            section_for("pane-limit-per-worker", "Keep the ask short."),
            Section::MachinesSpawning,
            "a machines slug outranks a passing text keyword"
        );
        assert_eq!(
            topic_title("agents.profiles.provider"),
            "agents profiles provider"
        );
    }

    #[test]
    fn sections_paint_in_named_order_other_last() {
        let rows = vec![
            law(
                "d-1",
                "zai-lanes-cap",
                "2026-11-05T00:00:00Z",
                "The zai lanes hold sixteen workers.",
            ),
            law(
                "d-2",
                "public-launch-version",
                "2026-11-05T01:00:00Z",
                "The public launch is v0.5.0.",
            ),
            law(
                "d-3",
                "epic-size",
                "2026-11-05T02:00:00Z",
                "An epic carries its own size.",
            ),
            law(
                "d-4",
                "ask-length",
                "2026-11-05T03:00:00Z",
                "Keep the ask short.",
            ),
            law(
                "d-5",
                "twin",
                "2026-11-05T04:00:00Z",
                "Second ruling, same instant.",
            ),
            law(
                "d-6",
                "twin",
                "2026-11-05T04:00:00Z",
                "First ruling, same instant.",
            ),
        ];
        let sections = book(&rows);
        let order: Vec<Section> = sections.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            order,
            [
                Section::Release,
                Section::QuestionsMail,
                Section::MachinesSpawning,
                Section::Other
            ]
        );
        let twins: Vec<&Topic> = sections
            .iter()
            .flat_map(|(_, ts)| ts.iter())
            .filter(|t| t.slug == "twin")
            .collect();
        let twin = twins.first().expect("the twin topic");
        assert_eq!(
            twin.face.decision_id, "d-6",
            "same-instant faces tiebreak on the id sort"
        );
        for (low, high) in sections.iter().zip(sections.iter().skip(1)) {
            let a = BOOK_SECTIONS
                .iter()
                .position(|&s| s == low.0)
                .expect("section in BOOK_SECTIONS");
            let b = BOOK_SECTIONS
                .iter()
                .position(|&s| s == high.0)
                .expect("section in BOOK_SECTIONS");
            assert!(a < b, "section order follows BOOK_SECTIONS");
        }
    }
}
