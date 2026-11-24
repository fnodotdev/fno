//! The board's id mode for the crate-fno readers: `backlog.id_mode =
//! "sequential"` mints `<prefix>-<n>` ids, hex is the default, and every
//! read failure reads hex - the fail-open the settings receipt keeps.
//! One door read per process, the same per-process cache
//! `node_ref::is_sequential_tail` holds in fno-agent, because the query
//! parser calls the shape check per token.
//
// The contract build (no `app` feature) keeps this module compiling for
// the parse surface but reaches it through no live caller, so the
// dead-code pass names the whole chain; that is structural there, not a
// defect.
#![cfg_attr(not(feature = "app"), allow(dead_code))]

/// The mode-and-prefix pair the shape checks share. The pure predicate and
/// pattern builders take it as an argument so tests drive both modes
/// regardless of the machine's own config.
pub(crate) struct IdMode {
    pub sequential: bool,
    /// The configured prefix, trimmed of its trailing dash (`t`).
    pub prefix: String,
}

/// The facts through the sibling settings door (`state path settings-read`),
/// read once per process. An old sibling that predates the receipt's
/// `sequential` field answers hex - the `Default` derive covers it. The
/// contract build (no `app` feature) carries no door, so it reads hex
/// outright: the fail-open default.
#[cfg(feature = "app")]
pub(crate) fn facts() -> &'static IdMode {
    static FACTS: std::sync::OnceLock<IdMode> = std::sync::OnceLock::new();
    FACTS.get_or_init(|| {
        let read: Option<Receipt> =
            std::process::Command::new(crate::ui::digest_overlay::fno_agent_bin())
                .args(["state", "path", "settings-read"])
                // The door reads stdin to EOF: inherited stdin never ends
                // and the board thread blocks forever behind this read
                // (the mux e2e caught it as a stuck "reading board...").
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()
                .and_then(|child| child.wait_with_output().ok())
                .filter(|out| out.status.success())
                .and_then(|out| serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).ok());
        let read = read.unwrap_or_default();
        IdMode {
            sequential: read.sequential,
            prefix: read.prefix.trim_end_matches('-').to_string(),
        }
    })
}

/// The contract build's facts: hex, the documented default. `search_query`
/// is not feature-gated, so its shape check needs this to compile against.
#[cfg(not(feature = "app"))]
pub(crate) fn facts() -> &'static IdMode {
    static HEX: IdMode = IdMode {
        sequential: false,
        prefix: String::new(),
    };
    &HEX
}

/// The settings receipt fields the shape checks share, with hex defaults
/// for every absent field.
#[derive(serde::Deserialize, Default)]
struct Receipt {
    #[serde(default)]
    sequential: bool,
    #[serde(default)]
    prefix: String,
}

/// The sequential tail shape: one nonzero-leading digit run of 1 to 7
/// under exactly the configured prefix, sequential mode only. Pure over
/// the facts; the twin gate is `node_ref::is_sequential_tail` in
/// fno-agent.
pub(crate) fn sequential_tail_matches(mode: &IdMode, prefix: &str, tail: &str) -> bool {
    mode.sequential
        && prefix == mode.prefix
        && (1..=7).contains(&tail.len())
        && !tail.starts_with('0')
        && tail.bytes().all(|b| b.is_ascii_digit())
}

/// The sequential arm as a regex fragment (`t-[1-9][0-9]{0,6}`) for a
/// prefix. Pure; the twin builder is `node_ref::sequential_id_pattern_for`
/// in fno-agent. No escaping: a configured prefix is validated
/// `[a-z][a-z0-9]{0,7}`, never a regex metacharacter.
pub(crate) fn sequential_pattern_for(prefix: &str) -> String {
    format!("{}-[1-9][0-9]{{0,6}}", prefix.trim_end_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequential_tail_gate_holds_the_shape_and_the_mode() {
        let hex_mode = IdMode {
            sequential: false,
            prefix: "t".into(),
        };
        let seq = IdMode {
            sequential: true,
            prefix: "t".into(),
        };
        assert!(!sequential_tail_matches(&hex_mode, "t", "15"));
        assert!(sequential_tail_matches(&seq, "t", "15"));
        assert!(sequential_tail_matches(&seq, "t", "1"));
        // Another prefix, a zero lead, and an over-long run stay prose.
        assert!(!sequential_tail_matches(&seq, "u", "9"));
        assert!(!sequential_tail_matches(&seq, "t", "07"));
        assert!(!sequential_tail_matches(&seq, "t", "123456789"));
    }

    #[test]
    fn the_pattern_splices_the_escaped_prefix_and_the_digit_shape() {
        assert_eq!(sequential_pattern_for("t"), "t-[1-9][0-9]{0,6}");
        assert_eq!(sequential_pattern_for("t-"), "t-[1-9][0-9]{0,6}");
    }
}
