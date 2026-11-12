//! The backlog tag vocabulary: `facet:value` tags checked against the one
//! vocabulary file (`tag_vocabulary.toml`, compiled in), and the
//! deterministic classifier the backfill and the birth path share.

use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

const VOCABULARY: &str = include_str!("tag_vocabulary.toml");

/// The facets every open node must carry.
pub const REQUIRED_FACETS: [&str; 2] = ["audience", "area"];

#[derive(Deserialize)]
struct Vocabulary {
    facets: BTreeMap<String, Facet>,
    audience_keywords: BTreeMap<String, Vec<String>>,
    areas: Vec<Area>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum FacetKind {
    Closed,
    Version,
    Open,
}

#[derive(Deserialize)]
struct Facet {
    kind: FacetKind,
    #[serde(default)]
    values: Vec<String>,
}

#[derive(Deserialize)]
struct Area {
    name: String,
    audience: String,
    keywords: Vec<String>,
    #[serde(default)]
    projects: Vec<String>,
}

fn vocabulary() -> &'static Vocabulary {
    static V: OnceLock<Vocabulary> = OnceLock::new();
    V.get_or_init(|| toml::from_str(VOCABULARY).expect("tag_vocabulary.toml parses"))
}

/// The allowed values of a closed facet, `area` read from the area rows.
fn closed_values(facet: &str) -> Vec<String> {
    let v = vocabulary();
    if facet == "area" {
        return v.areas.iter().map(|a| a.name.clone()).collect();
    }
    v.facets
        .get(facet)
        .map(|f| f.values.clone())
        .unwrap_or_default()
}

fn is_kebab_word(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_version(s: &str) -> bool {
    let mut parts = s.split('.');
    let major_minor = parts
        .next()
        .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty())
        && parts
            .next()
            .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty());
    // A patch part may carry a pre-release suffix: 0.4.8, 0.5.0rc1.
    major_minor
        && parts.all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Check one normalized tag against the vocabulary. A bare tag (no `:`) is
/// an operational marker and passes. A refusal names the allowed values.
pub fn check(tag: &str) -> Result<(), String> {
    let Some((facet, value)) = tag.split_once(':') else {
        return Ok(());
    };
    let v = vocabulary();
    let Some(spec) = v.facets.get(facet) else {
        let known: Vec<&str> = v.facets.keys().map(String::as_str).collect();
        return Err(format!(
            "unknown tag facet '{facet}' in '{tag}'; facets: {} (a tag with no ':' is a free marker)",
            known.join(", ")
        ));
    };
    match spec.kind {
        FacetKind::Closed => {
            let allowed = closed_values(facet);
            if !allowed.iter().any(|a| a == value) {
                return Err(format!(
                    "'{value}' is not a {facet} value; allowed: {}. Add a value in crates/fno-agents/src/backlog/tag_vocabulary.toml",
                    allowed.join(", ")
                ));
            }
        }
        FacetKind::Version => {
            if !is_version(value) {
                return Err(format!("'{tag}' needs a version, e.g. {facet}:0.4.8"));
            }
        }
        FacetKind::Open => {
            if !is_kebab_word(value) {
                return Err(format!(
                    "'{tag}' needs one kebab-case word, e.g. {facet}:rust-idiom"
                ));
            }
        }
    }
    Ok(())
}

/// The tags stored on a row, empty when absent.
pub fn row_tags(e: &Value) -> Vec<&str> {
    e.get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// True when the row carries a tag of `facet`.
pub fn has_facet(e: &Value, facet: &str) -> bool {
    row_tags(e)
        .iter()
        .any(|t| t.split_once(':').is_some_and(|(f, _)| f == facet))
}

/// How sure the classifier is of one guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Low => "low",
        }
    }
}

/// One facet guess for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guess {
    pub tag: String,
    pub confidence: Confidence,
    pub why: String,
}

fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_matches('-').to_string())
        .collect()
}

fn text_of(e: &Value, field: &str) -> String {
    e.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Score each area: a title word counts 2, a details word 1, a project
/// mapping 3. High confidence needs a lead of 2 over the runner-up.
fn guess_area(
    e: &Value,
    title: &BTreeSet<String>,
    details: &BTreeSet<String>,
) -> Option<(Guess, String)> {
    let project = text_of(e, "project");
    let mut scored: Vec<(usize, &Area, Vec<&str>)> = Vec::new();
    for area in &vocabulary().areas {
        let mut score = 0;
        let mut hits = Vec::new();
        if area.projects.iter().any(|p| *p == project) {
            score += 3;
            hits.push("project");
        }
        for k in &area.keywords {
            if title.contains(k) {
                score += 2;
                hits.push(k.as_str());
            } else if details.contains(k) {
                score += 1;
            }
        }
        if score > 0 {
            scored.push((score, area, hits));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    let (top, area, hits) = scored.first()?;
    let runner_up = scored.get(1).map_or(0, |s| s.0);
    let confidence = if *top >= 2 && top - runner_up >= 2 {
        Confidence::High
    } else {
        Confidence::Low
    };
    let why = if hits.is_empty() {
        format!("details words, score {top} vs {runner_up}")
    } else {
        format!("{} (score {top} vs {runner_up})", hits.join(", "))
    };
    Some((
        Guess {
            tag: format!("area:{}", area.name),
            confidence,
            why,
        },
        area.audience.clone(),
    ))
}

/// Audience words decide when one list wins in the title; else the area's
/// default audience stands, high only when the area guess is high and no
/// audience word disagrees.
fn guess_audience(
    title: &BTreeSet<String>,
    details: &BTreeSet<String>,
    area: Option<&(Guess, String)>,
) -> Option<Guess> {
    let mut votes: Vec<(usize, &str)> = vocabulary()
        .audience_keywords
        .iter()
        .map(|(aud, kws)| {
            let n = kws
                .iter()
                .map(|k| 2 * usize::from(title.contains(k)) + usize::from(details.contains(k)))
                .sum();
            (n, aud.as_str())
        })
        .filter(|(n, _)| *n > 0)
        .collect();
    votes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let default = area.map(|(g, aud)| (g.confidence, aud.as_str()));
    match (votes.first(), default) {
        (Some(&(top, aud)), _) if top >= 2 && top >= votes.get(1).map_or(0, |v| v.0) + 2 => {
            let agrees = default.is_some_and(|(_, d)| d == aud);
            Some(Guess {
                tag: format!("audience:{aud}"),
                confidence: Confidence::High,
                why: if agrees {
                    "audience words and area agree".into()
                } else {
                    "audience words".into()
                },
            })
        }
        (votes_top, Some((area_conf, aud))) => {
            let disagrees = votes_top.is_some_and(|&(_, v)| v != aud);
            Some(Guess {
                tag: format!("audience:{aud}"),
                confidence: if area_conf == Confidence::High && !disagrees {
                    Confidence::High
                } else {
                    Confidence::Low
                },
                why: "the area's default audience".into(),
            })
        }
        (Some(&(_, aud)), None) => Some(Guess {
            tag: format!("audience:{aud}"),
            confidence: Confidence::Low,
            why: "weak audience words".into(),
        }),
        (None, None) => None,
    }
}

/// Guess the required facets a row lacks. A facet the row already carries
/// is never guessed, so a human tag always stands.
pub fn classify(e: &Value) -> Vec<Guess> {
    let title = words(&format!("{} {}", text_of(e, "title"), text_of(e, "slug")));
    let details = words(&text_of(e, "details"));
    let area = guess_area(e, &title, &details);
    let mut out = Vec::new();
    if !has_facet(e, "area") {
        if let Some((g, _)) = &area {
            out.push(g.clone());
        }
    }
    if !has_facet(e, "audience") {
        // A human-set area drives the default audience too.
        let human_area = row_tags(e)
            .iter()
            .find_map(|t| t.strip_prefix("area:"))
            .and_then(|name| vocabulary().areas.iter().find(|a| a.name == name))
            .map(|a| {
                (
                    Guess {
                        tag: format!("area:{}", a.name),
                        confidence: Confidence::High,
                        why: String::new(),
                    },
                    a.audience.clone(),
                )
            });
        if let Some(g) = guess_audience(&title, &details, human_area.as_ref().or(area.as_ref())) {
            out.push(g);
        }
    }
    out
}

/// True when every `want` tag is on the row (AND).
pub fn has_all(e: &Value, want: &[String]) -> bool {
    let have = row_tags(e);
    want.iter().all(|w| have.iter().any(|h| h == w))
}

/// The facet tags two rows share that carry relatedness signal: area,
/// harness and topic. Audience and release are shared by too many rows.
pub fn shared_signal_tags<'a>(a: &'a Value, b: &Value) -> Vec<&'a str> {
    let theirs = row_tags(b);
    row_tags(a)
        .into_iter()
        .filter(|t| {
            t.split_once(':')
                .is_some_and(|(f, _)| matches!(f, "area" | "harness" | "topic"))
        })
        .filter(|t| theirs.contains(t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_vocabulary_refuses_an_unknown_value_and_names_the_allowed_ones() {
        for ok in [
            "audience:user",
            "area:mail",
            "harness:codex",
            "release:0.4.8",
            "release:0.5.0rc1",
            "topic:rust-idiom",
            "main-repair",
        ] {
            assert_eq!(check(ok), Ok(()), "{ok}");
        }
        let err = check("audience:everyone").unwrap_err();
        assert!(
            err.contains("allowed: user, contributor, internal"),
            "{err}"
        );
        assert!(check("area:nowhere").unwrap_err().contains("mux"));
        assert!(check("colour:red")
            .unwrap_err()
            .contains("unknown tag facet"));
        assert!(check("release:next").is_err());
        assert!(check("topic:two.words").is_err());
    }

    #[test]
    fn the_classifier_stamps_high_only_on_a_clear_lead_and_never_over_a_human_tag() {
        let mux = json!({"title": "mux pane keeper loses the portal on restart", "project": "fno"});
        let guesses = classify(&mux);
        assert_eq!(guesses[0].tag, "area:mux");
        assert_eq!(guesses[0].confidence, Confidence::High);
        assert_eq!(guesses[1].tag, "audience:user");
        assert_eq!(guesses[1].confidence, Confidence::High);

        let flaky = json!({"title": "flaky shard rerun in ci", "project": "fno"});
        let g = classify(&flaky);
        assert_eq!(
            (g[0].tag.as_str(), g[1].tag.as_str()),
            ("area:ci", "audience:contributor")
        );

        let vague = json!({"title": "make the thing better", "project": "etl"});
        assert!(classify(&vague)
            .iter()
            .all(|g| g.confidence == Confidence::Low));

        let tagged = json!({"title": "mux pane", "tags": ["area:mail", "audience:internal"]});
        assert!(classify(&tagged).is_empty());
    }
}
