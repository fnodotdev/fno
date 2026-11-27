//! Typed reads for the PR status gate.
//!
//! The REST reader and the status composer picked GitHub API payloads apart
//! by string key. The views here parse each payload once and the readers
//! read fields; every read stays total, so a field of the wrong shape reads
//! None the way the `.get()` walk did. Fields a reader clones into output
//! whole (the passthrough projections) or presence-probes stay
//! presence-preserving `Value`; string-only readers take the typed string.
//! docs/rust-style.md R1.

use crate::lenient_de::{opt_bool, opt_i64, opt_string, opt_value_present};
use serde::Deserialize;
use serde_json::Value;

/// The REST pull JSON (`repos/<slug>/pulls/<n>`).
#[derive(Deserialize, Default)]
pub(crate) struct PullsTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) merged: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) merged_at: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) number: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) mergeable: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) mergeable_state: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) html_url: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) body: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) auto_merge: Option<Value>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) draft: Option<bool>,
}

/// One check-runs page: the rows plus the count that bounds the paging loop.
#[derive(Deserialize, Default)]
pub(crate) struct CheckRunsPageTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) check_runs: Option<Value>,
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) total_count: Option<i64>,
}

/// One workflow run from the runs listing.
#[derive(Deserialize, Default)]
pub(crate) struct RunTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) id: Option<i64>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
}

/// One check-run row the rollup projection maps.
#[derive(Deserialize, Default)]
pub(crate) struct CheckRunTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) name: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) status: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) conclusion: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) started_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) timeout: Option<Value>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) advisory: Option<bool>,
}

/// The row shape `read_checks_rows` builds, cut to the hold gate's fields:
/// the verdict bucket and the workflow file that keys the aggregate.
#[derive(Deserialize, Default)]
pub(crate) struct RowTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) bucket: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) workflow: Option<String>,
}

/// One element of the issue-labels read (`repos/<slug>/issues/<n>/labels`).
#[derive(Deserialize, Default)]
pub(crate) struct LabelTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
}

/// One legacy StatusContext row.
#[derive(Deserialize, Default)]
pub(crate) struct StatusRowTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) context: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) created_at: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) target_url: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) description: Option<Value>,
}

/// One rollup node of the GraphQL projection. The passthrough fields keep
/// their wire spelling.
#[allow(non_snake_case)]
#[derive(Deserialize, Default)]
pub(crate) struct NodeTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) name: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) status: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) conclusion: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) startedAt: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) detailsUrl: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) context: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) state: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) createdAt: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) targetUrl: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) description: Option<Value>,
}

/// The GraphQL pull object. The passthrough fields keep their wire
/// spelling.
#[allow(non_snake_case)]
#[derive(Deserialize, Default)]
pub(crate) struct PullFullTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) merged: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) mergedAt: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) headRefOid: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) headRefName: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) mergeable: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) mergeStateStatus: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) baseRefName: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) url: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) body: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) autoMergeRequest: Option<Value>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) isDraft: Option<bool>,
}

/// The composed status fields the pr-nudge user-gate park reads. Lenient:
/// a stale cached row that predates the draft key reads None there, which
/// is today's behavior.
#[derive(Deserialize, Default)]
pub(crate) struct StatusParkTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) settled: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) pr_state: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) draft: Option<bool>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) title: Option<String>,
}

/// The assembled pr_json projection every downstream reader consumes. The
/// passthrough fields keep their wire spelling.
#[allow(non_snake_case)]
#[derive(Deserialize, Default)]
pub(crate) struct ProjectionTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) statusCheckRollup: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) headRefOid: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) state: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) headRefName: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) mergeStateStatus: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) baseRefName: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) mergeable: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) pull_head_stale: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) workflowRuns: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) html_url: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) body: Option<Value>,
}

/// The prior status payload (same head, earlier read) the reuse checks read.
#[derive(Deserialize, Default)]
pub(crate) struct PriorTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) failures: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) verdict: Option<String>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) head: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rerun_recovered: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) recovered_failures: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) branch_history: Option<Value>,
}

/// One failure row of the prior payload.
#[derive(Deserialize, Default)]
pub(crate) struct JobIdTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) job_id: Option<String>,
}

/// The verdict counts object.
#[derive(Deserialize, Default)]
pub(crate) struct CountsTail {
    #[serde(default, deserialize_with = "opt_i64")]
    pub(crate) total: Option<i64>,
}

/// The failure-cause scan's answer.
#[derive(Deserialize, Default)]
pub(crate) struct CauseTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cause: Option<String>,
}

/// The cached job-log row.
#[derive(Deserialize, Default)]
pub(crate) struct LogTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) log: Option<String>,
}

/// One Actions job step.
#[derive(Deserialize, Default)]
pub(crate) struct StepRowTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) name: Option<String>,
}

/// The Actions job object's steps block.
#[derive(Deserialize, Default)]
pub(crate) struct StepsTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) steps: Option<Value>,
}

/// The detail entry the failure collector builds, presence-probed between
/// writes.
#[derive(Deserialize, Default)]
pub(crate) struct EntryProbe {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) step: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) first_error: Option<Value>,
}

/// The review-coverage projection the receipt ask reads.
#[derive(Deserialize, Default)]
pub(crate) struct ReviewsTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) optional_reviews_unresolved: Option<Value>,
}

/// The rerun-recovery fact row.
#[derive(Deserialize, Default)]
pub(crate) struct RecoveredTail {
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) recovered: Option<bool>,
}

/// The GitHub merge-blocker op's answer.
#[derive(Deserialize, Default)]
pub(crate) struct GithubMergeTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) blockers: Option<Value>,
}

/// The branch-history op's answer.
#[derive(Deserialize, Default)]
pub(crate) struct LineTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) line: Option<String>,
}

/// The review-activity probe's answer.
#[derive(Deserialize, Default)]
pub(crate) struct ActivityTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) blocker: Option<String>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
}

/// The authorized-merge receipt preview.
#[derive(Deserialize, Default)]
pub(crate) struct ReceiptTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) blockers: Option<Value>,
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) detail: Option<String>,
}

/// The composed status payload the green memo writes from.
#[derive(Deserialize, Default)]
pub(crate) struct ComposeTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) optional_reviews_unresolved: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) rerun_recovered: Option<Value>,
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) recovered_failures: Option<Value>,
}

/// The `status-ci` door op's request payload.
#[derive(Deserialize, Default)]
pub(crate) struct StatusCiTail {
    #[serde(default, deserialize_with = "opt_string")]
    pub(crate) cwd: Option<String>,
    #[serde(default, deserialize_with = "opt_bool")]
    pub(crate) drop_coverage: Option<bool>,
}

/// The commit-status endpoint's answer: the StatusContext rows.
#[derive(Deserialize, Default)]
pub(crate) struct StatusesTail {
    #[serde(default, deserialize_with = "opt_value_present")]
    pub(crate) statuses: Option<Value>,
}
