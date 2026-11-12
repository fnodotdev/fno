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
) -> Option<(Guess, &'static Area)> {
    let project = text_of(e, "project");
    let mut scored: Vec<(usize, &Area)> = vocabulary()
        .areas
        .iter()
        .map(|area| {
            let mapped = 3 * usize::from(area.projects.contains(&project));
            let words: usize = area
                .keywords
                .iter()
                .map(|k| {
                    2 * usize::from(title.contains(k))
                        + usize::from(!title.contains(k) && details.contains(k))
                })
                .sum();
            (mapped + words, area)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    let &(top, area) = scored.first()?;
    let runner_up = scored.get(1).map_or(0, |s| s.0);
    let confidence = if top >= runner_up + 2 {
        Confidence::High
    } else {
        Confidence::Low
    };
    let why = format!("keyword score {top} vs {runner_up}");
    Some((
        Guess {
            tag: format!("area:{}", area.name),
            confidence,
            why,
        },
        area,
    ))
}

/// A clear audience-word lead in the title decides. Otherwise the area's
/// default audience stands, high only when the area is sure and no
/// audience word disagrees.
fn guess_audience(
    title: &BTreeSet<String>,
    details: &BTreeSet<String>,
    area: Option<(Confidence, &str)>,
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
    let lead = votes
        .first()
        .filter(|&&(top, _)| top >= 2 + votes.get(1).map_or(0, |v| v.0));
    let (aud, confidence, why) = match (lead, area, votes.first()) {
        (Some(&(_, aud)), _, _) => (aud, Confidence::High, "audience words"),
        (None, Some((conf, aud)), top) => {
            let agrees = conf == Confidence::High && top.is_none_or(|&(_, v)| v == aud);
            (
                aud,
                if agrees {
                    Confidence::High
                } else {
                    Confidence::Low
                },
                "the area's default audience",
            )
        }
        (None, None, Some(&(_, aud))) => (aud, Confidence::Low, "weak audience words"),
        (None, None, None) => return None,
    };
    Some(Guess {
        tag: format!("audience:{aud}"),
        confidence,
        why: why.into(),
    })
}

/// Guess the required facets a row lacks. A facet the row already carries
/// is never guessed, so a human tag always stands; a human area also sets
/// the default audience.
pub fn classify(e: &Value) -> Vec<Guess> {
    let title = words(&format!("{} {}", text_of(e, "title"), text_of(e, "slug")));
    let details = words(&text_of(e, "details"));
    let guessed = guess_area(e, &title, &details);
    let human_area = row_tags(e)
        .iter()
        .find_map(|t| t.strip_prefix("area:"))
        .and_then(|name| vocabulary().areas.iter().find(|a| a.name == name));
    let mut out = Vec::new();
    let area = match (human_area, &guessed) {
        (Some(a), _) => Some((Confidence::High, a.audience.as_str())),
        (None, Some((g, a))) => {
            if !has_facet(e, "area") {
                out.push(g.clone());
            }
            Some((g.confidence, a.audience.as_str()))
        }
        (None, None) => None,
    };
    if !has_facet(e, "audience") {
        out.extend(guess_audience(&title, &details, area));
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
