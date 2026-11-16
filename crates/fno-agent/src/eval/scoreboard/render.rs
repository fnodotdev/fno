//! The scoreboard text renderers, ported byte-exactly from the Python
//! `_render*` functions in `cli/src/fno/scoreboard/cli.py`. The by-provider
//! view keeps its own renderer beside its fold (`scoreboard_provider::render`)
//! and the door prints that text verbatim. Python wrote through
//! `sys.stdout.write` with explicit newlines; each function here emits the
//! same bytes with `print!`.

use crate::lenient_de::parse;
use serde_json::{Map, Value};

#[path = "render_de.rs"]
mod de;

/// Python `_fmt`: `n/a` when unmeasurable (never a fake 0), a plain integer
/// when whole (no sci-notation for million-scale token counts), else one
/// decimal.
fn fmt(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "n/a".to_string();
    };
    match v {
        Value::Null => "n/a".to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => match n.as_f64() {
                Some(f) if f.is_finite() && f.fract() == 0.0 => (f as i64).to_string(),
                Some(f) => format!("{f:.1}"),
                None => "n/a".to_string(),
            },
        },
        _ => "n/a".to_string(),
    }
}

/// Python `str()` on the numbers an f-string prints bare: serde's compact
/// form matches it for both shapes the folds emit (`42` and `42.0`).
fn num_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `x or '?'` on the row's session id: an empty or missing string is `'?'`.
fn sid_or_q(row: &Value) -> String {
    parse::<de::ResultRow>(row)
        .session_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("?")
        .to_string()
}

/// `x or '?'` on the row's pr_number: 0, empty, or absent is `'?'`.
fn pr_number_or_q(row: &Value) -> String {
    match parse::<de::ResultRow>(row).pr_number {
        Some(Value::Number(n)) if n.as_i64() == Some(0) => "?".to_string(),
        Some(Value::String(s)) if s.is_empty() => "?".to_string(),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

/// `_render_lanes`.
pub(crate) fn render_lanes(view: &Value) {
    let v: de::LanesView = parse(view);
    let win = v.since_days.unwrap_or(28);
    print!("fno whoami scoreboard --lanes (last {win}d)\n\n");
    let cov_raw = v.coverage.clone().unwrap_or_default();
    let cov: de::CoverageTail = parse(&cov_raw);
    print!("Coverage\n");
    print!("  rows in window:  {}\n", num_str(cov.rows.as_ref()));
    print!(
        "  provider:        {}/{}\n",
        num_str(cov.provider.as_ref()),
        num_str(cov.rows.as_ref())
    );
    print!(
        "  model:           {}/{}\n",
        num_str(cov.model.as_ref()),
        num_str(cov.rows.as_ref())
    );
    print!(
        "  effort:          {}/{}\n",
        num_str(cov.effort.as_ref()),
        num_str(cov.rows.as_ref())
    );
    let rows_n = cov.rows.as_ref().and_then(Value::as_i64).unwrap_or(0);
    let partial = ["provider", "model", "effort"]
        .iter()
        .any(|axis| cov_raw.get(axis).and_then(Value::as_i64).unwrap_or(0) < rows_n);
    if partial {
        print!("  ! axis coverage is partial; missing values are not a model verdict.\n");
    }

    print!("\nRetrospective\n");
    let empty = Vec::new();
    let retro = v
        .retrospective
        .as_ref()
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if retro.is_empty() {
        print!("  no execution rows in window.\n");
    } else {
        print!("  provider          model                    effort size runs ok% wall-min carveouts sample\n");
        for row in retro {
            let r: de::RetroRow = parse(row);
            print!(
                "  {:<16} {:<24} {:<6} {:<4} {:>4} {:>3}% {:>8.1} {:>9} {}\n",
                r.provider.as_deref().unwrap_or_default(),
                r.model.as_deref().unwrap_or_default(),
                r.effort.as_deref().unwrap_or_default(),
                r.size.as_deref().unwrap_or_default(),
                num_str(r.runs.as_ref()),
                num_str(r.ok_pct.as_ref()),
                r.wall_minutes.unwrap_or(0.0),
                num_str(r.carveouts_filed.as_ref()),
                r.sample_state.as_deref().unwrap_or_default(),
            );
        }
    }

    print!("\nLive\n");
    let live = v.live.as_ref().and_then(Value::as_array).unwrap_or(&empty);
    if live.is_empty() {
        print!("  no active lanes.\n");
    } else {
        print!("  provider          model                    effort occupancy cap headroom\n");
        for row in live {
            let r: de::LiveRow = parse(row);
            let cap = match &r.cap {
                Some(Value::Null) | None => "n/a".to_string(),
                Some(v) => num_str(Some(v)),
            };
            let headroom = match &r.headroom {
                Some(Value::Null) | None => "n/a".to_string(),
                Some(v) => num_str(Some(v)),
            };
            print!(
                "  {:<16} {:<24} {:<6} {:>9} {:>3} {:>8}\n",
                r.provider.as_deref().unwrap_or_default(),
                r.model.as_deref().unwrap_or_default(),
                r.effort.as_deref().unwrap_or_default(),
                num_str(r.occupancy.as_ref()),
                cap,
                headroom,
            );
        }
    }
    let rate_limited = v.rate_limited.clone().unwrap_or(Value::Null);
    print!(
        "\n  provider_rate_limited events: {}\n",
        num_str(Some(&rate_limited))
    );
}

/// `_render`, the main scoreboard.
pub(crate) fn render(sb: &Value) {
    let s: de::MainView = parse(sb);
    let win = s.since_days.unwrap_or(28);
    if s.state.as_deref() == Some("no_data") {
        print!("fno whoami scoreboard (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov_raw = s.coverage.clone().unwrap_or_default();
    let cov: de::CoverageTail = parse(&cov_raw);
    print!("fno whoami scoreboard (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.rows.as_ref()));
    print!(
        "  termination_reason:  {}%",
        num_str(
            cov.termination_reason_pct
                .map(|n| serde_json::Value::from(n))
                .as_ref()
        )
    );
    print!(
        "    node linkage:  {}%\n",
        num_str(cov.node_linkage_pct.map(serde_json::Value::from).as_ref())
    );
    // Silent-failure guard: whenever coverage is partial on EITHER axis, the
    // caveat rides on the same screen as any rate below. Never a bare rate.
    let term_pct = cov.termination_reason_pct.unwrap_or(0);
    let node_pct = cov.node_linkage_pct.unwrap_or(0);
    if term_pct < 100 || node_pct < 100 {
        print!(
            "  ! rates below reflect {term_pct}% termination / {node_pct}% node-linkage: a partial window is not a trend.\n"
        );
    }
    // Journal integrity rides the same screen as any rate it could bias.
    let ec_tail: de::EventCoverageTail = s
        .event_coverage
        .as_ref()
        .map(|ec| parse(ec))
        .unwrap_or_default();
    let incomplete = |ec: &de::EventCoverageTail| !ec.complete.unwrap_or(true);
    let show_ec = s
        .event_coverage
        .as_ref()
        .is_some_and(|_| incomplete(&ec_tail));
    if show_ec {
        print!(
            "  ! event journals incomplete: {} malformed line(s), {} unreadable file(s); omissions, not zeros.\n",
            ec_tail.malformed_lines.unwrap_or(0),
            ec_tail.unreadable_paths.unwrap_or(0),
        );
    }
    let emit_tail: de::EmissionTail = s
        .emission_failures
        .as_ref()
        .map(|e| parse(e))
        .unwrap_or_default();
    if s.emission_failures.is_some() && emit_tail.available == Some(true) {
        print!(
            "  ! touch emission failures: {} since {} (measured {}; server instance lifetime).\n",
            num_str(emit_tail.count.as_ref()),
            num_str(emit_tail.measured_since.as_ref()),
            num_str(emit_tail.measured_at.as_ref()),
        );
    } else if s.emission_failures.is_some() {
        print!(
            "  ! touch emission failures: unknown ({}).\n",
            emit_tail
                .reason
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("server unreachable"),
        );
    }
    // shipped is the merge; the terminal count rides beside it for one release.
    if let Some(shipped) = s.shipped_nodes.as_ref().filter(|v| !v.is_null()) {
        let by_term = num_str(s.shipped_by_terminal.as_ref());
        let classes = s.delivery_classes.clone().unwrap_or_default();
        let mut bits: Vec<String> = Vec::new();
        if let Some(obj) = classes.as_object() {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for name in keys {
                bits.push(format!("{} {name}", num_str(obj.get(name))));
            }
        }
        let no_row = s.merged_nodes_without_ledger_row.unwrap_or(0);
        let shipped_s = num_str(Some(shipped));
        print!(
            "\nShipped       {shipped_s} nodes (confirmed merge, doc or delivery evidence); by session terminal alone: {by_term}\n"
        );
        if !bits.is_empty() {
            print!("              by evidence: {}\n", bits.join(" "));
        }
        if no_row != 0 {
            print!("              merged nodes with no ledger row: {no_row}\n");
        }
        let shipped_n = shipped.as_f64().unwrap_or(0.0);
        let by_term_n = by_term.parse::<f64>().unwrap_or(0.0);
        if shipped_n > 0.0 && by_term_n < 0.9 * shipped_n {
            print!(
                "  ! terminal-only undercounts nodes whose PR merged after the session stopped; the merge is the count.\n"
            );
        }
    }
    if let Some(scope) = s.project_scope.as_ref().filter(|sv| !sv.is_null()) {
        let sc: de::ScopeTail = parse(scope);
        print!(
            "\nProject scope {}: {} nodes; {} unattributed, {} other-project row(s) kept out.\n",
            sc.project.as_deref().unwrap_or_default(),
            num_str(sc.nodes.as_ref()),
            num_str(sc.unattributed_rows.as_ref()),
            num_str(sc.other_project_rows.as_ref()),
        );
    }

    print!("\nStop-cause distribution\n");
    let stop_cause = s.stop_cause.clone().unwrap_or_default();
    let mut sc_items: Vec<(String, i64)> = stop_cause
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    if sc_items.is_empty() {
        print!("  (no termination_reason on any row in window)\n");
    } else {
        sc_items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (reason, n) in &sc_items {
            print!("  {reason:<14} {n}\n");
        }
    }

    let sp: de::SpendTail = s.spend.as_ref().map(|v| parse(v)).unwrap_or_default();
    print!("\nSpend split\n");
    print!(
        "  ship-terminal:   ${:.2}\n",
        sp.ship_terminal_usd.unwrap_or(0.0)
    );
    print!(
        "  wedge-terminal:  ${:.2}\n",
        sp.wedge_terminal_usd.unwrap_or(0.0)
    );
    print!("  other:           ${:.2}\n", sp.other_usd.unwrap_or(0.0));

    print!("\nAutonomy      ");
    let au: de::AutonomyTail = s.autonomy.as_ref().map(|v| parse(v)).unwrap_or_default();
    if au.available.unwrap_or(false) {
        print!(
            "{} human touches / shipped node ({} touches, {} nodes)\n",
            num_str(au.touches_per_shipped_node.as_ref()),
            num_str(au.touches.as_ref()),
            num_str(au.shipped_nodes.as_ref()),
        );
    } else {
        print!("n/a - {}\n", au.reason.as_deref().unwrap_or(""));
    }

    print!("Survival      ");
    let su: de::SurvivalTail = s.survival.as_ref().map(|v| parse(v)).unwrap_or_default();
    if su.available.unwrap_or(false) {
        let pending = su.pending.filter(|p| *p != 0);
        let suffix = match pending {
            Some(p) => format!(", {p} pending observation"),
            None => String::new(),
        };
        print!(
            "{}% ({}/{} shipped nodes{suffix})\n",
            num_str(su.rate_pct.as_ref()),
            num_str(su.survived.as_ref()),
            num_str(su.shipped_nodes.as_ref()),
        );
    } else {
        print!("n/a - {}\n", su.reason.as_deref().unwrap_or(""));
    }
}

/// `_render_calibration`.
pub(crate) fn render_calibration(cal: &Value) {
    let c: de::CalibrationView = parse(cal);
    print!("fno whoami scoreboard --calibration\n\n");
    let mut excl_bits: Vec<String> = Vec::new();
    if let Some(obj) = c.excluded.as_ref().and_then(Value::as_object) {
        if !obj.is_empty() {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                let n = obj[k].as_i64().unwrap_or(0);
                excl_bits.push(format!("{n} {k}"));
            }
        }
    }
    if let Some(u) = c.unattributed {
        if u != 0 {
            excl_bits.push(format!("{u} unattributed"));
        }
    }
    let excl_line = if excl_bits.is_empty() {
        String::new()
    } else {
        format!(" (excluded: {})", excl_bits.join(", "))
    };

    if c.state.as_deref() == Some("insufficient") {
        let n = num_str(c.n.as_ref());
        let need = num_str(c.need.as_ref());
        print!("  {n} verdicts so far, need >={need} for calibration.{excl_line}\n");
        return;
    }

    let n = num_str(c.n.as_ref());
    print!("  N={n} verdicts{excl_line}\n");
    if let Some(u) = c.untimed_outcomes {
        if u != 0 {
            print!("  ! {u} node(s) lack a timestamped ship row; their outcomes are conservative (any caused_by fix counts as bounced).\n");
        }
    }
    print!("\n");
    let outcomes = ["merged_clean", "bounced", "reverted"];
    print!("  {:<10}", "");
    for o in outcomes {
        print!("{o:>14}");
    }
    print!("\n");
    for verdict in ["pass", "concerns", "fail"] {
        print!("  {verdict:<10}");
        let row = c
            .table
            .as_ref()
            .and_then(|t| t.get(verdict))
            .cloned()
            .unwrap_or_default();
        for o in outcomes {
            let n = row.get(o).and_then(Value::as_i64).unwrap_or(0);
            print!("{n:>14}");
        }
        print!("\n");
    }
    let fp: de::FalsePositiveTail = c
        .false_positive
        .as_ref()
        .map(|v| parse(v))
        .unwrap_or_default();
    let count = num_str(fp.count.as_ref());
    let of_pass = num_str(fp.of_pass.as_ref());
    let rate = num_str(fp.rate_pct.as_ref());
    print!("\n  false-positive (pass -> bounced/reverted): {count}/{of_pass} ({rate}%)\n");
}

/// `_render_by_skill`.
pub(crate) fn render_by_skill(sb: &Value) {
    let v: de::BySkillView = parse(sb);
    let win = v.since_days.unwrap_or(28);
    if v.state.as_deref() == Some("no_data") {
        print!(
            "fno whoami scoreboard --by-skill (last {win}d)\n\n  no terminal sessions in window.\n"
        );
        return;
    }
    let cov_raw = v.coverage.clone().unwrap_or_default();
    let cov: de::CoverageTail = parse(&cov_raw);
    print!("fno whoami scoreboard --by-skill (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.rows.as_ref()));
    print!(
        "  attributed:          {}%\n",
        num_str(cov.attributed_pct.map(serde_json::Value::from).as_ref())
    );
    if cov.attributed_pct.unwrap_or(0) < 100 {
        print!(
            "  ! rows below reflect {}% attribution coverage - unattributed rows are listed, never dropped.\n",
            num_str(
                cov.attributed_pct
                    .map(serde_json::Value::from)
                    .as_ref()
            )
        );
    }
    print!("\n");
    print!(
        "  {:<32}{:<10}{:>6}{:>7}{:>9}{:>11}{:>10}  method\n",
        "skill", "version", "runs", "ship%", "revert%", "touch/run", "cost/run"
    );
    let empty = Vec::new();
    for row in v.rows.as_ref().and_then(Value::as_array).unwrap_or(&empty) {
        let r: de::SkillRow = parse(row);
        let revert = match &r.revert_rate_pct {
            Some(Value::Null) | None => "n/a".to_string(),
            Some(v) => format!("{}%", num_str(Some(v))),
        };
        print!(
            "  {:<32}{:<10}{:>6}{:>6}%{revert:>9}{:>11}{:>10.2}  {}\n",
            r.skill.as_deref().unwrap_or_default(),
            r.version.as_deref().unwrap_or_default(),
            r.runs.unwrap_or(0),
            r.ship_rate_pct.unwrap_or(0),
            num_str(r.touches_per_run.as_ref()),
            r.cost_per_run.unwrap_or(0.0),
            r.method.as_deref().unwrap_or_default(),
        );
    }
}

/// `_render_efficiency`.
pub(crate) fn render_efficiency(eff: &Value) {
    let v: de::EfficiencyView = parse(eff);
    let win = v.since_days.unwrap_or(28);
    if v.state.as_deref() == Some("no_data") {
        print!("fno whoami scoreboard --efficiency (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov_raw = v.coverage.clone().unwrap_or_default();
    let cov: de::CoverageTail = parse(&cov_raw);
    print!("fno whoami scoreboard --efficiency (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.rows.as_ref()));
    print!(
        "  loop-check join:     {}%",
        num_str(cov.loop_join_pct.map(serde_json::Value::from).as_ref())
    );
    print!(
        "    transcript:  {}%",
        num_str(cov.transcript_pct.map(serde_json::Value::from).as_ref())
    );
    print!(
        "    node linkage:  {}%\n",
        num_str(cov.node_linkage_pct.map(serde_json::Value::from).as_ref())
    );
    print!(
        "  outcome tracked:     {}% of shipped rows\n",
        num_str(
            cov.outcome_tracked_pct
                .map(serde_json::Value::from)
                .as_ref()
        )
    );
    let loop_join = cov.loop_join_pct.unwrap_or(0);
    let node_link = cov.node_linkage_pct.unwrap_or(0);
    if loop_join < 100 || node_link < 100 {
        print!(
            "  ! metrics below reflect {loop_join}% loop-join / {node_link}% node-linkage: a partial window is not a trend.\n"
        );
    }
    if let Some(u) = cov.ci_unparsed {
        if u != 0 {
            print!("  ! {u} loop_check fire(s) carried an unrecognized ci shape (emitter drift); their sessions' ci_reds are n/a, not counted as green.\n");
        }
    }
    print!("\nPer-outcome-class cost\n");
    print!(
        "  {:<20}{:>4}{:>10}{:>10}{:>11}{:>9}\n",
        "class", "n", "spend$", "med tok", "med fires", "med min"
    );
    if let Some(obj) = v.per_outcome_class.as_ref().and_then(Value::as_object) {
        let mut items: Vec<(&String, &Value)> = obj.iter().collect();
        items.sort_by(|a, b| {
            let na = parse::<de::ClassTail>(a.1).n.unwrap_or(0);
            let nb = parse::<de::ClassTail>(b.1).n.unwrap_or(0);
            nb.cmp(&na).then_with(|| a.0.cmp(b.0))
        });
        for (cls, b) in items {
            let ct: de::ClassTail = parse(b);
            print!(
                "  {cls:<20}{:>4}{:>10.2}{:>10}{:>11}{:>9}\n",
                ct.n.unwrap_or(0),
                ct.spend_usd.unwrap_or(0.0),
                fmt(ct.median_tokens.as_ref()),
                fmt(ct.median_fires.as_ref()),
                fmt(ct.median_duration_min.as_ref()),
            );
        }
    }

    if let Some(obj) = v.plan_vs_build_cost.as_ref().and_then(Value::as_object) {
        if !obj.is_empty() {
            print!("\nPlan vs build cost per node\n");
            print!("  {:<14}{:>10}{:>10}\n", "node", "plan$", "build$");
            let mut items: Vec<(&String, &Value)> = obj.iter().collect();
            items.sort_by(|a, b| {
                let pa = parse::<de::CostTail>(a.1).plan_usd.unwrap_or(0.0);
                let pb = parse::<de::CostTail>(b.1).plan_usd.unwrap_or(0.0);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            for (nid, c) in items {
                let ct: de::CostTail = parse(c);
                print!(
                    "  {nid:<14}{:>10.2}{:>10.2}\n",
                    ct.plan_usd.unwrap_or(0.0),
                    ct.build_usd.unwrap_or(0.0),
                );
            }
        }
    }

    print!("\nDistribution (rows with >=1 loop_check fire)\n");
    print!(
        "  {:<18}{:>10}{:>10}{:>6}\n",
        "metric", "median", "p90", "n"
    );
    let empty = Map::new();
    for metric in ["loop_fires", "ci_reds", "tokens_total", "duration_minutes"] {
        let d = v
            .distribution
            .as_ref()
            .and_then(|val| val.get(metric))
            .cloned()
            .unwrap_or(Value::Object(empty.clone()));
        let dt: de::DistTail = parse(&d);
        print!(
            "  {metric:<18}{:>10}{:>10}{:>6}\n",
            fmt(dt.median.as_ref()),
            fmt(dt.p90.as_ref()),
            dt.n.unwrap_or(0),
        );
    }
}

/// `_render_plan_fidelity`.
pub(crate) fn render_plan_fidelity(pf: &Value) {
    let v: de::PlanFidelityView = parse(pf);
    let win = v.since_days.unwrap_or(28);
    if v.state.as_deref() == Some("no_data") {
        print!("fno whoami scoreboard --plan-fidelity (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov_raw = v.coverage.clone().unwrap_or_default();
    let cov: de::CoverageTail = parse(&cov_raw);
    print!("fno whoami scoreboard --plan-fidelity (last {win}d)\n\n");
    print!(
        "  {} planned row(s), {}% joined to a delivery.\n\n",
        num_str(cov.planned_rows.as_ref()),
        num_str(cov.joined_pct.as_ref()),
    );
    let empty = Vec::new();
    for r in v
        .results
        .as_ref()
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let rv: de::ResultRow = parse(r);
        if rv.status.as_deref() == Some("unjoined") {
            print!("  {:<24} unjoined (plan not yet delivered)\n", sid_or_q(r));
            continue;
        }
        let ac_s = match rv.ac_coverage.as_ref() {
            Some(ac_val) => match ac_val.as_object() {
                Some(ac) if !ac.is_empty() => {
                    let ac_tail: de::AcTail = parse(ac_val);
                    format!(
                        "{}/{} ({}%)",
                        num_str(ac_tail.verified.as_ref()),
                        num_str(ac_tail.total.as_ref()),
                        num_str(ac_tail.pct.as_ref()),
                    )
                }
                _ => "n/a".to_string(),
            },
            None => "n/a".to_string(),
        };
        let dm = fmt(rv.data_model_surprise.as_ref());
        let drift = match rv.scope_drift.as_ref() {
            Some(sd_val) => match sd_val.as_object() {
                Some(sd) if !sd.is_empty() => {
                    let sd_tail: de::ScopeDriftTail = parse(sd_val);
                    sd_tail
                        .unplanned
                        .as_ref()
                        .and_then(Value::as_array)
                        .map(|a| a.len().to_string())
                        .unwrap_or_else(|| "0".to_string())
                }
                _ => "n/a".to_string(),
            },
            None => "n/a".to_string(),
        };
        let probes_s = match &rv.probes {
            Some(pr) if !pr.is_null() => {
                let o = pr.as_object().map(|o| !o.is_empty()).unwrap_or(false);
                if o {
                    let pt: de::ProbesTail = parse(pr);
                    format!(
                        "{}/{}",
                        num_str(pt.passed.as_ref()),
                        num_str(pt.declared.as_ref()),
                    )
                } else {
                    "n/a".to_string()
                }
            }
            _ => "n/a".to_string(),
        };
        let ctx_s = match &rv.context_outcome_trace {
            Some(t) => {
                let tt: de::TraceTail = parse(t);
                match tt.context.as_ref() {
                    Some(ctx) if !ctx.is_null() => {
                        let ct: de::CtxTail = parse(ctx);
                        match ct.bytes.as_ref() {
                            Some(b) if !b.is_null() => format!("{}B", num_str(Some(b))),
                            _ => "n/a".to_string(),
                        }
                    }
                    _ => "n/a".to_string(),
                }
            }
            None => "n/a".to_string(),
        };
        let outcome = match rv.outcome.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => "n/a".to_string(),
        };
        print!(
            "  {sid:<24} PR#{pr} AC {ac_s} | drift {drift} | data-model-surprise {dm} | deviations {dev} | probes {probes_s} | context {ctx_s} | outcome {outcome}\n",
            sid = sid_or_q(r),
            pr = pr_number_or_q(r),
            dev = fmt(rv.deviation_load.as_ref()),
        );
    }
    let cmp_tail: de::ComparisonTail = v
        .context_comparison
        .as_ref()
        .map(|cmp| parse(cmp))
        .unwrap_or_default();
    let label = cmp_tail.label.as_deref().unwrap_or("rejected");
    let reason = cmp_tail
        .reason
        .clone()
        .or_else(|| cmp_tail.claim.clone())
        .unwrap_or_else(|| Value::String("no contract".to_string()));
    print!(
        "\n  context comparison: {label} ({})\n",
        reason.as_str().unwrap_or("no contract"),
    );
}
