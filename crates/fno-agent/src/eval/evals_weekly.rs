//! The trace-side reports on `evals report`: `--decay` (failures by model,
//! context band and compaction count) and `--weekly` (the user's slice, judge
//! agreement, stale goldens and the failure trend). The daily trace tick runs
//! the weekly leg, which writes one report file per ISO week.
//!
//! Traces hold failures only, so a decay cell is that cell's share of the
//! model's failures. No all-turns count exists to make it a per-turn rate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{json, Value};

const BANDS: [&str; 4] = ["0-25", "25-50", "50-75", "75-100"];
const COMPACTIONS: [&str; 4] = ["0", "1", "2", "3+"];
/// A model with fewer measured failures than this prints n and no share.
const MIN_SHARE_N: usize = 20;
/// Label rows the user reads each week.
const SLICE_N: usize = 20;
/// A golden with this many scored grades in the window and no failure
/// catches nothing.
const STALE_MIN_GRADES: usize = 4;
const STALE_DAYS: i64 = 28;
const TREND_WEEKS: i64 = 4;

fn reading(trace: &Value, key: &str) -> Option<f64> {
    trace.get(key)?.pointer("/value")?.as_f64()
}

fn band(percent: f64) -> &'static str {
    match percent {
        p if p < 25.0 => BANDS[0],
        p if p < 50.0 => BANDS[1],
        p if p < 75.0 => BANDS[2],
        _ => BANDS[3],
    }
}

fn cells(counts: &BTreeMap<&'static str, usize>, keys: &[&str]) -> Value {
    let measured: usize = keys.iter().filter_map(|k| counts.get(*k)).sum();
    let mut out = serde_json::Map::new();
    for key in keys.iter().copied().chain(["unmeasured"]) {
        let n = counts.get(key).copied().unwrap_or(0);
        let share = (key != "unmeasured" && measured >= MIN_SHARE_N)
            .then(|| crate::evals_trend::round4(n as f64 / measured as f64));
        out.insert(key.to_string(), json!({"n": n, "share": share}));
    }
    Value::Object(out)
}

/// The decay fold over failure traces, one row per model.
pub(crate) fn decay(traces: &[Value]) -> Value {
    #[derive(Default)]
    struct Model {
        n: usize,
        context: BTreeMap<&'static str, usize>,
        compactions: BTreeMap<&'static str, usize>,
    }
    let mut models: BTreeMap<String, Model> = BTreeMap::new();
    for trace in traces {
        let name = crate::evals_labels::fact_str(&trace["model"]).unwrap_or("unmeasured");
        let model = models.entry(name.to_string()).or_default();
        model.n += 1;
        let context = reading(trace, "context_percent").map_or("unmeasured", band);
        *model.context.entry(context).or_default() += 1;
        let compaction =
            reading(trace, "compaction_count").map_or("unmeasured", |c| match c as u64 {
                0 => "0",
                1 => "1",
                2 => "2",
                _ => "3+",
            });
        *model.compactions.entry(compaction).or_default() += 1;
    }
    let rows: Vec<Value> = models
        .into_iter()
        .map(|(name, m)| {
            json!({
                "model": name,
                "n": m.n,
                "context_percent": cells(&m.context, &BANDS),
                "compaction_count": cells(&m.compactions, &COMPACTIONS),
            })
        })
        .collect();
    json!({
        "basis": "failure traces only: a share is the cell's part of the model's measured failures, not a per-turn rate",
        "min_share_n": MIN_SHARE_N,
        "models": rows,
    })
}

fn render_decay(report: &Value) -> String {
    let mut out = format!("decay: {}\n", report["basis"].as_str().unwrap_or(""));
    for model in report["models"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n{} (n={})\n",
            model["model"].as_str().unwrap_or("?"),
            model["n"]
        ));
        for (axis, keys) in [
            ("context_percent", &BANDS[..]),
            ("compaction_count", &COMPACTIONS[..]),
        ] {
            let line: Vec<String> = keys
                .iter()
                .chain(&["unmeasured"])
                .map(|key| {
                    let cell = model
                        .get(axis)
                        .and_then(|cells| cells.get(*key))
                        .unwrap_or(&Value::Null);
                    match cell["share"].as_f64() {
                        Some(share) => format!("{key}: {:.0}% n={}", share * 100.0, cell["n"]),
                        None => format!("{key}: n={}", cell["n"]),
                    }
                })
                .collect();
            out.push_str(&format!("  {axis:<17} {}\n", line.join("  ")));
        }
    }
    out
}

fn week_of(at: DateTime<Utc>) -> String {
    let week = at.iso_week();
    format!("{}-W{:02}", week.year(), week.week())
}

fn stamp(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// This week's label rows for the user to check, picked by a hash of the
/// week and the row so the pick is stable within a week and moves after it.
fn slice(labels: &[Value], now: DateTime<Utc>) -> Vec<Value> {
    let week = week_of(now);
    let since = now - Duration::days(7);
    let mut picked: Vec<(String, Value)> = labels
        .iter()
        .filter(|row| stamp(&row["created_at"]).is_some_and(|at| at >= since && at <= now))
        .map(|row| {
            let key = format!(
                "{week}|{}|{}",
                row["session_id"].as_str().unwrap_or(""),
                row["judge"].as_str().unwrap_or("")
            );
            let pick = json!({
                "session_id": row["session_id"], "judge": row["judge"],
                "label": row["label"], "span_ref": row["span_ref"], "reason": row["reason"],
            });
            (blake3::hash(key.as_bytes()).to_hex().to_string(), pick)
        })
        .collect();
    picked.sort_by(|a, b| a.0.cmp(&b.0));
    picked
        .into_iter()
        .take(SLICE_N)
        .map(|(_, row)| row)
        .collect()
}

/// Goldens with enough scored grades in the window and no failure among them.
fn stale_goldens(history: &[crate::evals_trend::Row], now: DateTime<Utc>) -> Vec<Value> {
    let since = now - Duration::days(STALE_DAYS);
    let mut tally: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for row in history {
        // A capability task that always passes graduates; it is never stale.
        if !crate::evals_trend::is_scored(row)
            || row.raw_str("tier") == Some("capability")
            || !row.ts().is_some_and(|ts| ts >= since)
        {
            continue;
        }
        let cell = tally.entry(row.task_id.as_str()).or_default();
        cell.0 += 1;
        cell.1 += usize::from(!row.pass);
    }
    tally
        .into_iter()
        .filter(|(_, (grades, fails))| *grades >= STALE_MIN_GRADES && *fails == 0)
        .map(|(task, (grades, _))| json!({"golden": task, "grades": grades, "fails": 0}))
        .collect()
}

/// Failure traces per signal kind for each of the last weeks, oldest first.
/// Seed traces carry their import day, not a failure day, so they stay out.
fn trend(traces: &[Value], now: DateTime<Utc>) -> Value {
    let weeks: Vec<String> = (0..TREND_WEEKS)
        .rev()
        .map(|back| week_of(now - Duration::weeks(back)))
        .collect();
    let mut kinds: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for trace in traces {
        let kind = trace["signal_kind"].as_str().unwrap_or("unknown");
        if kind == "seed_correction" {
            continue;
        }
        let Some(at) = stamp(&trace["created_at"]) else {
            continue;
        };
        if let Some(slot) = weeks.iter().position(|w| *w == week_of(at)) {
            let counts = kinds
                .entry(kind.to_string())
                .or_insert_with(|| vec![0; weeks.len()]);
            if let Some(count) = counts.get_mut(slot) {
                *count += 1;
            }
        }
    }
    json!({"weeks": weeks, "kinds": kinds})
}

/// The weekly report: the user's slice, judge agreement against the user's
/// hand labels under the rubric bar, stale goldens and the failure trend.
pub(crate) fn weekly(state: &Path, history: &Path, now: DateTime<Utc>) -> Value {
    let traces = crate::evals_labels::load_traces(state);
    let rows = crate::evals_trend::read_rows(&history.to_string_lossy(), None, None);
    let judges = crate::evals_labels::calibration_rows(state);
    json!({
        "week": week_of(now),
        "slice": slice(&crate::evals_labels::label_rows(state), now),
        "judges": judges,
        "bar": {
            "min_labels": crate::evals::rubric::MIN_LABELS,
            "agreement": crate::evals::rubric::PASS_AGREEMENT,
            "kappa": crate::evals::rubric::PASS_KAPPA,
        },
        "stale_goldens": stale_goldens(&rows, now),
        "trend": trend(&traces, now),
        "learnings": crate::evals_ledger::read(state).len(),
    })
}

fn render_weekly(report: &Value) -> String {
    let bar = &report["bar"];
    let mut out = format!(
        "weekly {}\n\nslice ({} rows; check each by setting the trace's layer_verdict or right_action with basis user_set):\n",
        report["week"].as_str().unwrap_or("?"),
        report["slice"].as_array().map_or(0, Vec::len)
    );
    for row in report["slice"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}\t{}\t{}\t{}\n",
            row["session_id"].as_str().unwrap_or("?"),
            row["judge"].as_str().unwrap_or("?"),
            row["label"].as_str().unwrap_or("?"),
            row["span_ref"].as_str().unwrap_or("?"),
        ));
    }
    out.push_str(&format!(
        "\njudges (bar: n >= {}, agreement >= {}, kappa >= {}; unproved until it holds):\n",
        bar["min_labels"], bar["agreement"], bar["kappa"]
    ));
    for judge in report["judges"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}\thand_n={} agreement={} kappa={} proved={}\n",
            judge["judge"].as_str().unwrap_or("?"),
            judge["hand_n"],
            judge["agreement"],
            judge["kappa"],
            judge["proved"],
        ));
    }
    out.push_str(&format!(
        "\nstale goldens (>= {STALE_MIN_GRADES} scored grades in {STALE_DAYS} days, no failure; retire or harden):\n"
    ));
    for golden in report["stale_goldens"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}\tgrades={}\n",
            golden["golden"].as_str().unwrap_or("?"),
            golden["grades"]
        ));
    }
    let weeks: Vec<&str> = report
        .pointer("/trend/weeks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    out.push_str(&format!("\nfailure trend ({}):\n", weeks.join(" ")));
    let kinds = report.pointer("/trend/kinds").and_then(Value::as_object);
    for (kind, counts) in kinds.into_iter().flatten() {
        let counts: Vec<String> = counts
            .as_array()
            .into_iter()
            .flatten()
            .map(Value::to_string)
            .collect();
        out.push_str(&format!("  {kind:<18} {}\n", counts.join(" ")));
    }
    out.push_str(&format!(
        "\nlearnings ledger rows: {}\n",
        report["learnings"]
    ));
    out
}

fn report_path(state: &Path, week: &str) -> PathBuf {
    crate::evals::state_evals_dir(state)
        .join("weekly")
        .join(format!("{week}.json"))
}

/// The weekly leg: the first daily tick of an ISO week writes that week's
/// report once, then settles the learnings ledger. The report goes first, so
/// a graph read failure in the sweep never costs the week its report.
pub(crate) fn leg(
    state: &Path,
    now: DateTime<Utc>,
) -> Result<Value, crate::evals_ledger::LedgerError> {
    let path = report_path(state, &week_of(now));
    let mut wrote = false;
    if !path.exists() {
        std::fs::create_dir_all(crate::evals::state_evals_dir(state).join("weekly"))
            .map_err(|e| e.to_string())?;
        let report = weekly(state, &crate::evals::history_path(), now);
        let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        wrote = crate::evals_traces::create_once(&path, &bytes)?;
    }
    let recorded = crate::evals_ledger::sweep(state, now)?;
    Ok(json!({"weekly": wrote.then(|| path.display().to_string()), "learnings": recorded}))
}

/// `evals report --decay | --weekly [--json]`.
pub fn command(args: &[String]) -> i32 {
    let state = crate::evals_candidates::state_root();
    let json_out = crate::json_output::requested(args);
    let is_decay = args.iter().any(|a| a == "--decay");
    let report = if is_decay {
        decay(&crate::evals_labels::load_traces(&state))
    } else {
        let history = crate::evals::flag_lookup(args, "--history")
            .map(PathBuf::from)
            .unwrap_or_else(crate::evals::history_path);
        weekly(&state, &history, Utc::now())
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else if is_decay {
        print!("{}", render_decay(&report));
    } else {
        print!("{}", render_weekly(&report));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
        value.pointer(pointer).unwrap_or(&Value::Null)
    }

    fn trace(model: Option<&str>, percent: Option<f64>, compactions: Option<u64>) -> Value {
        let fact = |v: Value| json!({"value": v, "basis": "transcript_before_turn"});
        let none = json!({"value": null, "basis": "unmeasured", "reason": "test"});
        json!({
            "signal_kind": "user_correction",
            "created_at": "2026-11-18T00:00:00+00:00",
            "model": model.map_or(none.clone(), |m| fact(json!(m))),
            "context_percent": percent.map_or(none.clone(), |p| fact(json!(p))),
            "compaction_count": compactions.map_or(none, |c| fact(json!(c))),
        })
    }

    #[test]
    fn evals_weekly_decay_and_weekly_reports_read_traces_labels_and_history() {
        let mut traces: Vec<Value> = (0..30)
            .map(|i| {
                trace(
                    Some("opus"),
                    Some(if i < 20 { 10.0 } else { 60.0 }),
                    Some(i % 5),
                )
            })
            .collect();
        traces.push(trace(Some("opus"), None, None));
        traces.push(trace(Some("glm"), Some(80.0), Some(1)));
        traces.push(trace(None, None, None));
        let report = decay(&traces);
        let model = |name: &str| {
            at(&report, "/models")
                .as_array()
                .and_then(|m| m.iter().find(|m| at(m, "/model") == &json!(name)))
                .cloned()
                .unwrap()
        };
        let (opus, glm, unmeasured) = (model("opus"), model("glm"), model("unmeasured"));
        assert_eq!(at(&opus, "/n"), &json!(31));
        assert_eq!(at(&opus, "/context_percent/0-25/n"), &json!(20));
        assert_eq!(at(&opus, "/context_percent/0-25/share"), &json!(0.6667));
        assert_eq!(at(&opus, "/context_percent/50-75/n"), &json!(10));
        assert_eq!(at(&opus, "/context_percent/unmeasured/n"), &json!(1));
        assert!(at(&opus, "/context_percent/unmeasured/share").is_null());
        assert_eq!(at(&opus, "/compaction_count/3+/n"), &json!(12));
        assert_eq!(at(&glm, "/context_percent/75-100/n"), &json!(1));
        assert!(at(&glm, "/context_percent/75-100/share").is_null());
        assert_eq!(at(&unmeasured, "/context_percent/0-25/n"), &json!(0));
        assert!(render_decay(&report).contains("0-25: 67% n=20"));

        // The weekly report: slice, judges, stale goldens and trend.
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path();
        let traces = crate::evals::state_evals_dir(state).join("traces");
        std::fs::create_dir_all(&traces).unwrap();
        for (i, kind) in ["no_progress", "no_progress", "revert"].iter().enumerate() {
            let mut t = trace(Some("opus"), Some(30.0), Some(0));
            let fields = t.as_object_mut().unwrap();
            fields.insert("signal_kind".into(), json!(kind));
            fields.insert(
                "session_id".into(),
                json!({"value": format!("s{i}"), "basis": "event"}),
            );
            std::fs::write(traces.join(format!("t{i}.json")), t.to_string()).unwrap();
        }
        let labels: Vec<String> = (0..25)
            .map(|i| {
                json!({"session_id": format!("s{i}"), "judge": "layer", "judge_version": "v1",
                    "label": "model", "span_ref": format!("t-s{i}/ev"), "reason": "r",
                    "created_at": "2026-11-17T00:00:00+00:00"})
                .to_string()
            })
            .collect();
        std::fs::write(
            crate::evals::state_evals_dir(state).join("labels.jsonl"),
            labels.join("\n"),
        )
        .unwrap();
        let history = state.join("history.jsonl");
        let mut lines = Vec::new();
        for (task, tier, pass) in [
            ("steady", "regression", true),
            ("caught", "regression", true),
            ("caught", "regression", false),
            ("graduating", "capability", true),
        ] {
            for _ in 0..4 {
                lines.push(
                    json!({"task_id": task, "tier": tier, "pass": pass,
                    "status": "graded", "ts": "2026-11-10T00:00:00Z"})
                    .to_string(),
                );
            }
        }
        std::fs::write(&history, lines.join("\n")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-11-19T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report = weekly(state, &history, now);
        assert_eq!(at(&report, "/week"), &json!("2026-W47"));
        assert_eq!(
            at(&report, "/slice").as_array().map(Vec::len),
            Some(SLICE_N)
        );
        let again = weekly(state, &history, now);
        assert_eq!(at(&report, "/slice"), at(&again, "/slice"));
        let layer = at(&report, "/judges")
            .as_array()
            .and_then(|j| j.iter().find(|j| at(j, "/judge") == &json!("layer")))
            .unwrap();
        assert_eq!(at(layer, "/proved"), &json!(false));
        assert_eq!(
            at(&report, "/stale_goldens"),
            &json!([{"golden": "steady", "grades": 4, "fails": 0}])
        );
        assert_eq!(
            at(&report, "/trend/kinds/no_progress"),
            &json!([0, 0, 0, 2])
        );
        let text = render_weekly(&report);
        assert!(text.contains("unproved until it holds"), "{text}");
        assert!(text.contains("steady\tgrades=4"), "{text}");
    }
}
