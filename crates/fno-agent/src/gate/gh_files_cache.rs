//! One PR files fetch per process: the gate's repeated
//! `gh api .../pulls/<n>/files` reads answered from a memo.
//!
//! One decide pass paginated PR files 3-4 times (overlap evidence, the
//! paint comment-only probe, the third overlap context, the evals gate),
//! and the decide+effect double pass repeated the set. The gate pins the
//! head and refuses on head change, so within one gate process the files
//! answer for a PR is stable; the memo keys on the full argv, so each
//! projection shape answers from the same fetch. The reviews read is
//! deliberately NOT cached: new reviews can land mid-decide.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type Answer = Arc<(bool, String)>;

fn memo() -> &'static Mutex<HashMap<Vec<String>, Answer>> {
    static MEMO: OnceLock<Mutex<HashMap<Vec<String>, Answer>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn files_key(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("api")
        && args.iter().skip(1).any(|arg| {
            // The path sits after the command wherever its flags end
            // (`--paginate` carries no value), so recognize it by shape
            // rather than by position.
            let base = arg.split('?').next().unwrap_or(arg);
            base.ends_with("/files") && base.contains("/pulls/")
        })
}

/// The memoized answer for one files argv, or None when the argv is not a
/// pulls/<n>/files read (the caller runs gh as today).
pub(crate) fn answer(args: &[String]) -> Option<(bool, String)> {
    if !files_key(args) {
        return None;
    }
    memo()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&args.to_vec())
        .map(|a| (a.0, a.1.clone()))
}

/// Store one files answer. A failed read stores nothing, so the next
/// projection retries the network as today.
pub(crate) fn remember(args: &[String], value: (bool, String)) {
    if !files_key(args) || !value.0 {
        return;
    }
    memo()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(args.to_vec(), Arc::new(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_pulls_files_argv_is_memoized() {
        assert!(files_key(&argv(&[
            "api",
            "repos/o/r/pulls/9/files?per_page=100&page=1",
        ])));
        assert!(files_key(&argv(&[
            "api",
            "--paginate",
            "repos/o/r/pulls/9/files"
        ])));
        assert!(files_key(&argv(&[
            "api",
            "repos/{owner}/{repo}/pulls/9/files",
        ])));
        assert!(!files_key(&argv(&[
            "api",
            "/repos/{owner}/{repo}/pulls/9/reviews?per_page=100",
        ])));
        assert!(!files_key(&argv(&["pr", "view", "9"])));
    }

    #[test]
    fn failed_reads_store_nothing_and_hits_need_a_remember() {
        let args = argv(&["api", "repos/o/r/pulls/9/files"]);
        assert_eq!(answer(&args), None, "empty memo answers None");
        remember(&args, (false, "boom".into()));
        assert_eq!(answer(&args), None, "a failed read stores nothing");
        remember(&args, (true, "rows".into()));
        assert_eq!(answer(&args), Some((true, "rows".to_string())));
    }
}
