//! Is the budget spent? Wall-clock and cost caps from the manifest and config, checked against the ledger.

use super::*;

/// The trip carries its cap and the value that crossed it, so the
/// termination event names the axis with numbers: a lead reading a
/// budget route sees what was spent against what, not just the axis word.
/// A malformed cap trips fail-closed with `cap_*: None` - the trip is real,
/// the number is unreadable.
#[derive(Debug, PartialEq)]
pub(super) enum BudgetTrip {
    WallClock {
        cap_min: Option<u64>,
        elapsed_min: u64,
    },
    Cost {
        cap_usd: Option<f64>,
        spent_usd: f64,
    },
    TeamCost {
        cap_usd: Option<f64>,
        spent_usd: f64,
    },
    AccountCost {
        account: String,
        cap_usd: Option<f64>,
        spent_usd: f64,
    },
}

/// The session's bound account and its per-session cost cap as read from
/// config: `None` when the record sets no cap, `Some(Err(raw))` when the
/// accounts config is unreadable (fail-closed downstream).
pub(super) struct AccountCap {
    pub account: String,
    pub cap: Option<Result<f64, String>>,
}

/// Resolve an `Option<Result<T, String>>` budget cap for use in check_budget.
/// - None => absent (no cap)
/// - Some(Ok(v)) => valid cap value
/// - Some(Err(raw)) => malformed: fail-closed, treat as cap exceeded immediately
pub(super) enum ResolvedCap<T> {
    Absent,
    Valid(T),
    Malformed(String),
}

pub(super) fn resolve_cap<T: Copy>(cap: &Option<Result<T, String>>) -> ResolvedCap<T> {
    match cap {
        None => ResolvedCap::Absent,
        Some(Ok(v)) => ResolvedCap::Valid(*v),
        Some(Err(raw)) => ResolvedCap::Malformed(raw.clone()),
    }
}

pub(super) fn check_budget(
    manifest: &Manifest,
    settings: &Settings,
    now: &DateTime<Utc>,
    ledger_path: &Path,
    account: Option<&AccountCap>,
) -> Option<BudgetTrip> {
    let attended = manifest.attended;

    // Wall-clock cap: prefer manifest value, then settings
    let wall_cap = match resolve_cap(&manifest.budget_wall_clock_cap_minutes) {
        ResolvedCap::Absent => {
            if attended {
                resolve_cap(&settings.attended_wall_cap_minutes)
            } else {
                resolve_cap(&settings.unattended_wall_cap_minutes)
            }
        }
        other => other,
    };

    match wall_cap {
        ResolvedCap::Malformed(raw) => {
            eprintln!("loop-check: malformed budget cap '{raw}' - failing closed; fix the config");
            return Some(BudgetTrip::WallClock {
                cap_min: None,
                elapsed_min: 0,
            });
        }
        ResolvedCap::Valid(cap) => {
            if let Some(ca_str) = &manifest.created_at {
                if let Ok(created) = ca_str.parse::<DateTime<Utc>>() {
                    // Guard against negative elapsed (clock skew / future created_at)
                    let duration = now.signed_duration_since(created);
                    let elapsed_min = if duration.num_minutes() < 0 {
                        0u64
                    } else {
                        duration.num_minutes() as u64
                    };
                    if elapsed_min >= cap {
                        return Some(BudgetTrip::WallClock {
                            cap_min: Some(cap),
                            elapsed_min,
                        });
                    }
                }
            }
        }
        ResolvedCap::Absent => {}
    }

    // Cost cap: prefer manifest value, then nested settings, then flat budget_cap
    let cost_cap = match resolve_cap(&manifest.budget_cost_cap_usd) {
        ResolvedCap::Absent => {
            let nested = if attended {
                resolve_cap(&settings.attended_cost_cap_usd)
            } else {
                resolve_cap(&settings.unattended_cost_cap_usd)
            };
            match nested {
                ResolvedCap::Absent => resolve_cap(&settings.flat_budget_cap),
                other => other,
            }
        }
        other => other,
    };

    // One meter reading feeds the session arm and the per-account arm; the
    // meter is read only when a cost cap of either kind can trip.
    let meter_usd = if matches!(cost_cap, ResolvedCap::Valid(_))
        || account.is_some_and(|acct| matches!(resolve_cap(&acct.cap), ResolvedCap::Valid(_)))
    {
        crate::team_spend::session_meter(
            &crate::paths::AgentsHome::from_env()
                .otel_dir()
                .join("otel.db"),
            ledger_path,
            manifest.session_id.as_deref(),
            manifest.harness_session_id.as_deref(),
        )
        .usd()
    } else {
        None
    };

    match cost_cap {
        ResolvedCap::Malformed(raw) => {
            eprintln!("loop-check: malformed budget cap '{raw}' - failing closed; fix the config");
            Some(BudgetTrip::Cost {
                cap_usd: None,
                spent_usd: 0.0,
            })
        }
        ResolvedCap::Valid(cap) => {
            if let Some(cost) = meter_usd {
                if cost >= cap {
                    return Some(BudgetTrip::Cost {
                        cap_usd: Some(cap),
                        spent_usd: cost,
                    });
                }
            }
            None
        }
        ResolvedCap::Absent => None,
    }

    // Per-account cap: the session's bound account's own per-session
    // ceiling, checked against the same meter reading the session arm uses.
    // Sits between the session cap and the team cap so the session cap wins
    // a tie and the fleet-wide cap still sees a spared run.
    .or_else(|| {
        let acct = account?;
        match resolve_cap(&acct.cap) {
            ResolvedCap::Absent => None,
            ResolvedCap::Malformed(raw) => {
                eprintln!(
                    "loop-check: malformed per-account cost cap '{raw}' on account '{}' - failing closed; fix the config",
                    acct.account
                );
                Some(BudgetTrip::AccountCost {
                    account: acct.account.clone(),
                    cap_usd: None,
                    spent_usd: 0.0,
                })
            }
            ResolvedCap::Valid(cap) => {
                let spent = meter_usd?;
                (spent >= cap).then_some(BudgetTrip::AccountCost {
                    account: acct.account.clone(),
                    cap_usd: Some(cap),
                    spent_usd: spent,
                })
            }
        }
    })
    .or_else(|| {
        let cap = match resolve_cap(&settings.team_cost_cap_usd) {
            ResolvedCap::Absent => return None,
            ResolvedCap::Malformed(raw) => {
                eprintln!("loop-check: malformed team budget cap '{raw}' - failing closed; fix the config");
                return Some(BudgetTrip::TeamCost { cap_usd: None, spent_usd: 0.0 });
            }
            ResolvedCap::Valid(cap) => cap,
        };
        let session = manifest.harness_session_id.as_deref()?;
        let spend = crate::team_spend::team_spend(session).ok()?;
        (spend.metered_usd >= cap).then_some(BudgetTrip::TeamCost {
            cap_usd: Some(cap),
            spent_usd: spend.metered_usd,
        })
    })
}

/// The session's bound account and its parsed per-session cost cap, read
/// from the registry row the stop hook's session id names and the accounts
/// config. None when the session carries no account: an unpinned row bills
/// the harness's ambient credentials, never a record, so there is no cap to
/// enforce, and a session with no target manifest never reaches this check
/// at all (no cap for a session that is not a target run).
pub(super) fn resolve_account_cap(
    cwd: &Path,
    harness_session_id: Option<&str>,
) -> Option<AccountCap> {
    let home = crate::paths::AgentsHome::from_env();
    resolve_account_cap_at(&home, cwd, harness_session_id)
}

pub(super) fn resolve_account_cap_at(
    home: &crate::paths::AgentsHome,
    cwd: &Path,
    harness_session_id: Option<&str>,
) -> Option<AccountCap> {
    let session_id = harness_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    let row = registry.entries.iter().find(|row| {
        [
            row.harness_session_id.as_deref(),
            row.session_id.as_deref(),
            row.related_session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|id| id.trim() == session_id)
    })?;
    let account = row
        .account_record_id
        .as_deref()
        .or(row.launch_account.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty() && !id.eq_ignore_ascii_case("default"))?;
    let cap = match crate::accounts::records::load(None, cwd) {
        // Fail closed: the session is billed to this account and its cap is
        // unreadable. The trip is real, the number is None - the same
        // posture as a malformed cap in the manifest.
        Err(error) => {
            eprintln!(
                "loop-check: accounts config unreadable for account '{account}' ({error}) - failing closed",
            );
            Some(Err(error.headline))
        }
        Ok(accounts) => match accounts.by_id(account) {
            Some(record) => record.cost_cap_usd_per_session.map(Ok),
            // A worktree session cannot see the canonical checkout's project
            // layer (candidates() reads the session cwd as its repo root), but
            // a lead spawning from the canonical checkout could have resolved
            // this account there. Look there before concluding no cap.
            None => canonical_layer_cap(cwd, account),
        },
    };
    Some(AccountCap {
        account: account.to_string(),
        cap,
    })
}

/// The account's cap as read from the canonical repo root's project layer,
/// when the session cwd's layers named no record. None outside a repo or
/// when the canonical root is the session cwd itself; an unreadable layer
/// fails closed like the session-cwd layers do.
fn canonical_layer_cap(cwd: &Path, account: &str) -> Option<Result<f64, String>> {
    let common_dir = crate::git_paths::common_dir(cwd)?;
    let project_root = common_dir.parent()?;
    if project_root == cwd {
        return None;
    }
    match crate::accounts::records::load(Some(project_root), cwd) {
        Err(error) => Some(Err(error.headline)),
        Ok(accounts) => accounts
            .by_id(account)
            .and_then(|record| record.cost_cap_usd_per_session)
            .map(Ok),
    }
}
