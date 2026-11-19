//! The synthetic-tail detector: did the model's last turn actually run?
//!
//! When a turn dies on an API error (auth failure, weekly limit, a poisoned
//! request), the claude harness writes the assistant record itself with
//! `model: "<synthetic>"` and the error text in the content. The session's
//! process can be perfectly alive, so process and socket probes answer live
//! while no model has run since. This module reads the LAST assistant record
//! of the transcript and classifies it, the backstop the watchdog and resume
//! use when the StopFailure hook did not fire (or before it existed).
//!
//! The model field is the discriminator, not the words: a lead that TALKS
//! about the outage writes "Not logged in" in its own reply, under a real
//! model name. Only the harness-authored placeholder counts.

use serde_json::Value;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::Path;

/// The claude harness-authored assistant record placeholder.
pub(crate) const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Transcripts grow without bound and only the last assistant turn matters,
/// so read the trailing 256 KiB (the bound the watchdog tail already uses).
const TAIL_BYTES: u64 = 256 * 1024;

/// Why the model never ran, from the synthetic reply's text. The remedy
/// differs per kind: respawn fixes a logout, a weekly limit wants a wait or
/// an account switch, and an API 400 poisons the transcript itself, so a
/// respawn would replay the bad turn (a fresh worker is the fix there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyntheticWhy {
    SignedOut,
    WeeklyLimit,
    Api400,
    Other,
}

impl SyntheticWhy {
    /// The status word the session list and the watchdog verdicts carry.
    /// Two public words: `signed_out` (a respawn fixes it) and
    /// `harness_error` (the harness answered for the model; the text says why).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SyntheticWhy::SignedOut => "signed_out",
            SyntheticWhy::WeeklyLimit | SyntheticWhy::Api400 | SyntheticWhy::Other => {
                "harness_error"
            }
        }
    }

    /// Only a logout is fixed by restarting the same session. A weekly limit
    /// comes back on the same account, and an API 400 replays the poisoned
    /// turn, so neither may take a respawn arm.
    pub(crate) fn respawn_fixes(self) -> bool {
        matches!(self, SyntheticWhy::SignedOut)
    }
}

/// The transcript's last assistant record, when the harness wrote it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SyntheticTail {
    pub why: SyntheticWhy,
    /// The first part of the synthetic reply's text, for mail and verdicts.
    pub excerpt: String,
}

/// The status word a synthetic tail earns over the session list's activity
/// read. The activity words (writing/quiet/parked/unknown) all assume a
/// model that can run; a synthetic last turn is the proof it cannot, so the
/// why decides the word. `orphaned` and `refused` keep their words: those
/// are positive verdicts the tail cannot improve.
pub(crate) fn status_override(current: &str, why: SyntheticWhy) -> &'static str {
    match current {
        "orphaned" => "orphaned",
        "refused" => "refused",
        _ => why.as_str(),
    }
}

/// The claude transcript record, parsed once into a typed shape (R1): the
/// discriminator is `type`, and only an assistant record carries a message
/// this detector reads.
#[derive(serde::Deserialize)]
struct TranscriptRecord {
    #[serde(rename = "type")]
    kind: String,
    message: Option<AssistantMessage>,
}

/// The assistant payload: the model name and the rendered text blocks.
#[derive(serde::Deserialize)]
struct AssistantMessage {
    model: Option<String>,
    content: Option<Vec<ContentBlock>>,
}

/// One content block; only its text part feeds the classifier.
#[derive(serde::Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

/// Classify a parsed transcript tail (records oldest-first). The LAST
/// assistant record decides; anything after it (the next user prompt) is
/// later speech, not a model turn. `None` when no assistant record sits in
/// the window: the honest answer is "no synthetic evidence", never a claim.
pub(crate) fn from_records(records: &[Value]) -> Option<SyntheticTail> {
    let last_assistant = records.iter().rev().find_map(|rec| {
        let parsed: TranscriptRecord = serde_json::from_value(rec.clone()).ok()?;
        (parsed.kind == "assistant").then_some(parsed)
    })?;
    let message = last_assistant.message?;
    if message.model.as_deref() != Some(SYNTHETIC_MODEL) {
        return None;
    }
    let text = message
        .content
        .unwrap_or_default()
        .iter()
        .filter_map(|block| block.text.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let why = classify(trimmed);
    let cut = trimmed
        .char_indices()
        .nth(160)
        .map(|(i, _)| i)
        .unwrap_or(trimmed.len());
    Some(SyntheticTail {
        why,
        excerpt: trimmed[..cut].to_string(),
    })
}

/// Resolve and read one transcript, then classify its tail. `None` when the
/// file is missing, unreadable, or carries no synthetic last turn.
pub(crate) fn from_transcript(path: &Path) -> Option<SyntheticTail> {
    let size = std::fs::metadata(path).ok()?.len();
    let start = size.saturating_sub(TAIL_BYTES);
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if size > TAIL_BYTES {
        // A mid-file seek lands inside a line; drop it.
        lines.next();
    }
    let parsed: Vec<Value> = lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|rec| rec.is_object())
        .collect();
    from_records(&parsed)
}

/// The documented synthetic replies, matched on the words claude writes.
/// Anything else a synthetic record says is still a turn the model never
/// ran: `Other` keeps the harness_error verdict and carries the excerpt.
fn classify(text: &str) -> SyntheticWhy {
    let lower = text.to_lowercase();
    if lower.contains("not logged in") {
        SyntheticWhy::SignedOut
    } else if lower.contains("weekly limit") {
        SyntheticWhy::WeeklyLimit
    } else if lower.starts_with("api error: 400") {
        SyntheticWhy::Api400
    } else {
        SyntheticWhy::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(model: &str, text: &str) -> Value {
        json!({
            "type": "assistant",
            "message": {
                "model": model,
                "role": "assistant",
                "content": [{"type": "text", "text": text}]
            }
        })
    }

    fn user(text: &str) -> Value {
        json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
    }

    #[test]
    fn a_real_model_reply_is_never_synthetic_evidence() {
        let records = vec![assistant(
            "glm-5.3-flash",
            "Not logged in is what the outage showed.",
        )];
        assert_eq!(from_records(&records), None);
    }

    #[test]
    fn the_last_assistant_record_decides_not_the_last_record() {
        // The outage shape: synthetic reply, then the operator's next prompt.
        let records = vec![
            assistant("glm-5.3-flash", "all green"),
            assistant(SYNTHETIC_MODEL, "Not logged in. Please run /login"),
            user("continue the loop"),
        ];
        let tail = from_records(&records).expect("signed-out tail");
        assert_eq!(tail.why, SyntheticWhy::SignedOut);
        assert!(tail.why.respawn_fixes());
        assert_eq!(tail.why.as_str(), "signed_out");
    }

    #[test]
    fn recovery_after_the_outage_clears_the_verdict() {
        // Post-respawn: a real assistant reply is the LAST assistant record.
        let records = vec![
            assistant(SYNTHETIC_MODEL, "Not logged in. Please run /login"),
            user("continue"),
            assistant("glm-5.3-flash", "back on the node"),
        ];
        assert_eq!(from_records(&records), None);
    }

    #[test]
    fn the_three_documented_whys_classify() {
        let limit = vec![assistant(
            SYNTHETIC_MODEL,
            "API Error: Weekly limit reached. Wait or switch account.",
        )];
        assert_eq!(
            from_records(&limit).map(|t| t.why),
            Some(SyntheticWhy::WeeklyLimit)
        );
        assert!(
            !SyntheticWhy::WeeklyLimit.respawn_fixes(),
            "a weekly limit waits on the account; no resume arm may re-seat it"
        );
        let poisoned = vec![assistant(
            SYNTHETIC_MODEL,
            "API Error: 400 messages.34.content.2: Input tag 'mcp_tool_use' found",
        )];
        let tail = from_records(&poisoned).expect("api-400 tail");
        assert_eq!(tail.why, SyntheticWhy::Api400);
        assert!(!tail.why.respawn_fixes());
        assert_eq!(tail.why.as_str(), "harness_error");
        let other = vec![assistant(SYNTHETIC_MODEL, "API Error: 529 overloaded")];
        assert_eq!(
            from_records(&other).map(|t| t.why),
            Some(SyntheticWhy::Other)
        );
    }

    #[test]
    fn a_silent_synthetic_record_answers_none() {
        let empty = json!({
            "type": "assistant",
            "message": {"model": SYNTHETIC_MODEL, "role": "assistant", "content": []}
        });
        assert_eq!(from_records(&[empty]), None);
        assert_eq!(from_records(&[user("hello")]), None);
    }

    /// The override vocabulary: the activity words reword to the why's
    /// word, the two positive verdicts keep theirs.
    #[test]
    fn the_override_rewords_activity_words_only() {
        for current in ["writing", "quiet", "parked", "unknown"] {
            assert_eq!(
                status_override(current, SyntheticWhy::SignedOut),
                "signed_out",
                "a signed-out tail must reword {current}"
            );
            assert_eq!(
                status_override(current, SyntheticWhy::Api400),
                "harness_error",
                "a poisoned-request tail must reword {current}"
            );
        }
        assert_eq!(
            status_override("orphaned", SyntheticWhy::SignedOut),
            "orphaned",
            "a falsified row keeps its positive verdict"
        );
        assert_eq!(
            status_override("refused", SyntheticWhy::Api400),
            "refused",
            "a refusal keeps its word over the tail"
        );
    }
}
