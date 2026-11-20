//! The spawn door's PR-watch liveness probe: one warning line, never a refusal.
//!
//! The lead skill asks for a pr-watch check at titling and in the once-pass
//! checklist (`skills/lead/references/once.md`), so nothing re-checks
//! liveness per dispatch: a lead can send a worker while the watcher is
//! down, and the PR the worker opens settles green with no settle mail and
//! no merge. This probe reads the same gather `pr-watch status` uses and
//! prints one stderr line naming the repair. A dead watcher must never
//! block a dispatch.

use crate::gate::pr_watch::status::{
    gather, launch_agents_dir, leg_facts, tick_watermarks, verdict,
};

/// The verdict words that mean down: absent, loaded but broken, or up and
/// delivering nothing. `disabled` is a deliberate config off and stays
/// silent, as do `healthy` and `healthy-pending`.
pub(crate) fn watcher_is_down(word: &str) -> bool {
    matches!(word, "dead" | "wedged" | "unhealthy")
}

/// The one warning line. Names the repair the setup wizard itself runs
/// (`fno-agent pr-watch install --ensure`, setup_run.rs).
fn warning_line(word: &str, detail: &str) -> String {
    format!(
        "fno agent spawn: pr-watch {word} ({detail}); a PR this worker opens \
         can settle green with no settle mail and no merge. Repair: \
         `fno-agent pr-watch install --ensure`."
    )
}

/// One probe per dispatch. A code payload reads the watcher and warns when
/// it is down; the payload test is the repo's own definition, the same gate
/// the codex git-grant refusal uses, so a node-bearing spawn probes too.
/// Any other payload skips the read and pays no launchctl subprocess.
pub fn warn_if_watcher_down(message: &str, node_present: bool) {
    if !crate::pane_spawn::codex::is_code_payload(message) && !node_present {
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let launch = launch_agents_dir();
    let inp = gather(&cwd, &launch);
    let marks = tick_watermarks(&inp.events);
    let leg = leg_facts(&inp);
    let v = verdict(&inp, &marks, &leg);
    if watcher_is_down(&v.word) {
        eprintln!("{}", warning_line(&v.word, &v.detail));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warn_set_covers_the_down_words_and_spares_deliberate_offs() {
        for word in ["dead", "wedged", "unhealthy"] {
            assert!(watcher_is_down(word), "{word} is down and must warn");
        }
        for word in ["disabled", "healthy", "healthy-pending"] {
            assert!(!watcher_is_down(word), "{word} must stay silent");
        }
        let line = warning_line("dead", "no plist");
        assert!(line.contains("fno-agent pr-watch install --ensure"));
        assert!(line.contains("no settle mail"));
    }
}
