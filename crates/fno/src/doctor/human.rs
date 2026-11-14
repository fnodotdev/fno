//! The human output renderer (doctor.py:2138-2883), section order pinned by
//! the frozen text golden.

use std::path::Path;

use serde_json::{json, Value};

use super::report;

fn emit(err: bool, line: &str) {
    if err {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

pub(crate) use super::report::emit_blockers;

#[path = "human_de.rs"]
mod de;

const REVIEW_WINDOW_S: i64 = 900;

fn review_invocation_lines() -> Vec<String> {
    let path = report::project_events_path();
    if !path.exists() {
        return vec![
            "fno doctor: review invocations: no event journal; no sent attempt to lose".to_string(),
        ];
    }
    scan_review_journal(&path)
}

fn scan_review_journal(path: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return vec![format!(
                "fno doctor: review invocations: unmeasurable ({e})"
            )];
        }
    };
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::seconds(REVIEW_WINDOW_S);
    let window_start = now - chrono::Duration::seconds(2 * REVIEW_WINDOW_S);
    let mut sent: Vec<(String, chrono::DateTime<chrono::Utc>, Value)> = Vec::new();
    let mut attested: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let journal = de::parse::<de::JournalEvent>(&event);
        let event_type = journal.r#type.as_deref().unwrap_or_default();
        if event_type == "review_invocation" {
            let Some(inv) = journal
                .data
                .as_ref()
                .and_then(|d| d.invocation_id.as_deref())
            else {
                continue;
            };
            let ts = journal.ts.as_deref().unwrap_or_default();
            let Ok(t) = chrono::DateTime::parse_from_rfc3339(&ts.replace('Z', "+00:00")) else {
                continue;
            };
            sent.retain(|(id, _, _)| id != inv);
            sent.push((
                inv.to_string(),
                t.with_timezone(&chrono::Utc),
                event.clone(),
            ));
        } else if event_type == "review_attestation" {
            if let Some(inv) = journal
                .data
                .as_ref()
                .and_then(|d| d.invocation_id.as_deref())
            {
                attested.push(inv.to_string());
            }
        }
    }
    finish_review_scan(sent, attested, cutoff, window_start)
}

fn finish_review_scan(
    sent: Vec<(String, chrono::DateTime<chrono::Utc>, Value)>,
    attested: Vec<String>,
    cutoff: chrono::DateTime<chrono::Utc>,
    window_start: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let lost: Vec<_> = sent
        .iter()
        .filter(|(id, t, _)| *t >= window_start && *t <= cutoff && !attested.contains(id))
        .collect();
    if lost.is_empty() {
        return vec![format!(
            "fno doctor: review invocations: scan complete; none lost in the last {}m",
            REVIEW_WINDOW_S / 60
        )];
    }
    vec![format!(
        "fno doctor: review invocations: {} sent attempt(s) have no matching attestation after {}m",
        lost.len(),
        REVIEW_WINDOW_S / 60
    )]
}

pub(crate) fn emit_human(rep: &Value, src: Option<&Path>, err: bool, cargo_present: bool) {
    let view = de::parse::<de::DoctorReport>(rep);
    for line in review_invocation_lines() {
        emit(err, &line);
    }
    emit_source_sync(&view, err);
    emit_status(&view, src, err);
    emit_rust_binary(&view, err, cargo_present);
    emit_binary_rev(&view, err);
    emit_components(&view, err);
    emit_daemon_drift(&view, err);
    emit_mux_front_door(&view, err);
    emit_gh_proxy(&view, err);
    emit_stale_servers(&view, err);
    emit_running_components(&view, err);
    emit_orphans(&view, err);
    emit_pr_watch(&view, err);
    emit_control_plane_arms(&view, err);
    emit_web_bridge(rep, err);
    emit_observer_unavailable(&view, err);
    emit_fd_limit(&view, err);
    emit_dead_letter(&view, err);
    emit_managed_block(&view, err);
    emit_surface_lines(&view, err);
    emit_groom(&view, err);
    emit_post_merge_sync(&view, err);
    emit_evals(&view, err);
    emit_launch_agents(&view, err);
    emit_silent_switches(&view, err);
    emit_review_gap(&view, err);
    emit_optout_residue(&view, err);
    emit_plugin_cache(&view, err);
    emit_codex_dual(&view, err);
    emit_pre_push(&view, err);
    emit_plugin_hooks(&view, err);
    emit_codex_foreign(&view, err);
    emit_codex_app_server(&view, err);
}

fn emit_source_sync(view: &de::DoctorReport, err: bool) {
    let ss = view.source_checkout_sync.as_ref();
    let status = ss.and_then(|s| s.status.as_deref()).unwrap_or_default();
    let behind = ss.and_then(|s| s.behind);
    if status == "behind" {
        let unit = if behind == Some(1) {
            "commit"
        } else {
            "commits"
        };
        emit(err, &format!(
            "fno doctor: resolved source checkout is {} {} behind origin/main; freshness is relative to stale local source. Sync the checkout before trusting verification.",
            behind.unwrap_or(0), unit
        ));
    } else if status == "unknown" && ss.is_some_and(|s| s.source_head.is_some()) {
        emit(err, &format!(
            "fno doctor: source checkout sync is unknown ({}); freshness against origin/main is unmeasured.",
            ss.and_then(|s| s.detail.as_deref())
                .filter(|d| !d.is_empty())
                .unwrap_or("no detail")
        ));
    }
}

fn emit_status(view: &de::DoctorReport, src: Option<&Path>, err: bool) {
    let status = view.status.as_deref().unwrap_or_default();
    if status == "fresh" {
        emit(err, &format!(
            "fno doctor: installed fno is up to date with source at {} (rev {}). Unmerged work in another branch or worktree is not included, and `fno` runs this installed build from anywhere. To exercise another checkout, build it with `cargo build` under <checkout>/crates and run that target's fno.",
            src.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "the resolved source checkout".to_string()),
            view.source_rev.as_deref().unwrap_or("unknown")
        ));
        return;
    }
    if status == "stale" {
        emit_status_stale(view, src, err);
        return;
    }
    emit_status_unknown(src, err);
}

fn emit_status_stale(view: &de::DoctorReport, src: Option<&Path>, err: bool) {
    let src_label = src
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".into());
    let missing_keys: Vec<String> = view.missing_config_keys.clone().unwrap_or_default();
    let marker_stale = view.marker_stale == Some(true);
    if !missing_keys.is_empty() {
        let first = missing_keys[0].clone();
        let inst = view.installed_rev.as_deref();
        let srcrev = view.source_rev.as_deref();
        let mut msg = format!(
            "fno doctor: config schema is STALE (deployed is missing {} config key(s), e.g. {first}).",
            missing_keys.len()
        );
        if let (Some(i), Some(s)) = (inst, srcrev) {
            if i != s {
                msg.push_str(&format!(" Installed rev {i} != source {s}."));
            }
        }
        emit(
            err,
            &format!("{msg} Run `fno doctor update` (or `fno doctor --fix`)."),
        );
        return;
    }
    if marker_stale {
        emit(err, &format!(
            "fno doctor: installed fno is behind {src_label} (installed rev {} != source {}). Run `fno doctor update` (or `fno doctor --fix`).",
            view.installed_rev.as_deref().unwrap_or("None"),
            view.source_rev.as_deref().unwrap_or("None")
        ));
        return;
    }
    let ri = view.rust_installed_rev.as_deref();
    let rs = view.rust_source_rev.as_deref();
    if let (Some(ri), Some(rs)) = (ri, rs) {
        emit(err, &format!(
            "fno doctor: rust bins STALE (installed {} != source {}). Run fno doctor update (or fno doctor --fix).",
            short12(ri), short12(rs)
        ));
        return;
    }
    emit(err, "fno doctor: rust bins STALE (a deployed component does not match the source build). Run fno doctor update (or fno doctor --fix); the component lines below name which one.");
}

fn emit_status_unknown(src: Option<&Path>, err: bool) {
    if src.is_none() {
        emit(
            err,
            "fno doctor: status unknown (no source checkout to compare against).",
        );
        return;
    }
    emit(
        err,
        "fno doctor: status unknown (installed rev undeterminable).",
    );
}

fn short12(v: &str) -> &str {
    let n = v.char_indices().nth(12).map(|(i, _)| i).unwrap_or(v.len());
    &v[..n]
}

fn emit_rust_binary(view: &de::DoctorReport, err: bool, cargo_present: bool) {
    let bin = view.rust_binary.as_deref();
    let rust_stale = view.rust_stale == Some(true);
    if bin.is_none() && !cargo_present {
        emit(
            err,
            "fno doctor: rust fno-agents binary: not found (cargo leg not applicable).",
        );
        return;
    }
    if rust_stale {
        let ri = view.rust_installed_rev.as_deref().unwrap_or_default();
        let rs = view.rust_source_rev.as_deref().unwrap_or_default();
        let label = bin.unwrap_or("(cargo-installed)");
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {label} rust bins STALE (installed {} != source {}). Run fno doctor update (or fno doctor --fix).",
            short12(ri), short12(rs)
        ));
        return;
    }
    if !cargo_present {
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {} revision not tracked (no cargo-installed binary; the rust staleness check applies to the cargo-installed binary only).",
            bin.unwrap_or("(cargo-installed)")
        ));
        return;
    }
    let ri = view.rust_installed_rev.as_deref();
    let rs = view.rust_source_rev.as_deref();
    let label = bin.unwrap_or("(cargo-installed)");
    if ri.is_some() && rs.is_some() && ri == rs {
        emit(
            err,
            &format!(
                "fno doctor: rust fno-agents binary: {label} rust bins fresh (rev {}).",
                short12(ri.unwrap_or_default())
            ),
        );
        return;
    }
    if ri.is_none() {
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {label} rust revision unknown (binary does not self-report a crates/ rev; rebuild via fno doctor update)."
        ));
        return;
    }
    emit(
        err,
        &format!("fno doctor: rust fno-agents binary: {label} rust revision unknown."),
    );
}

fn emit_binary_rev(view: &de::DoctorReport, err: bool) {
    let Some(rev) = view.binary_rev.as_deref() else {
        return;
    };
    emit(
        err,
        &format!(
            "fno doctor: rust fno-agents binary built at HEAD {} (build provenance).",
            short12(rev)
        ),
    );
}

fn emit_components(view: &de::DoctorReport, err: bool) {
    let Some(components) = view.components.as_deref() else {
        return;
    };
    let non_fresh: Vec<&de::ComponentRow> = components
        .iter()
        .filter(|c| {
            let st = c.status.as_deref().unwrap_or_default();
            st != "fresh" && st != "updated"
        })
        .collect();
    if components.is_empty() {
        return;
    }
    if !non_fresh.is_empty() {
        for row in non_fresh {
            if let Some(line) = row.line.as_deref() {
                emit(err, &format!("fno doctor: {line}"));
            }
        }
        return;
    }
    let names: Vec<String> = components
        .iter()
        .filter_map(|c| c.component.clone())
        .collect();
    let drifted = view
        .daemon_drift
        .as_ref()
        .is_some_and(|d| !d.is_null() && *d != Value::Bool(false));
    if drifted {
        emit(
            err,
            &format!(
                "fno doctor: components: {}/{} fresh on disk; a daemon drift note follows.",
                components.len(),
                components.len()
            ),
        );
        return;
    }
    emit(
        err,
        &format!(
            "fno doctor: components: {}/{} fresh ({}).",
            components.len(),
            components.len(),
            names.join(", ")
        ),
    );
}

fn emit_daemon_drift(view: &de::DoctorReport, err: bool) {
    let drifted = view
        .daemon_drift
        .as_ref()
        .is_some_and(|d| !d.is_null() && *d != Value::Bool(false));
    if drifted {
        emit(
            err,
            &format!(
                "fno doctor: note: {}",
                view.daemon_drift.clone().unwrap_or(Value::Null)
            ),
        );
    }
}

/// The gh proxy shim line, the read-side of guard diet: plain `gh` on
/// every caller's PATH should resolve to the transparent proxy shim, so a
/// read carries the App token and a stale answer costs an ETag revalida-
/// tion instead of core quota. Shadowed is the mac1 finding: the shim
/// exists but a raw gh resolves first.
fn emit_gh_proxy(view: &de::DoctorReport, err: bool) {
    let shim = view.gh_proxy_shim.as_deref();
    let path_gh = view.path_gh.as_deref();
    match view.gh_proxy.as_deref() {
        Some("dangling") => emit(
            err,
            &format!(
                "fno doctor: gh proxy: {} execs a binary that is gone; every plain gh call fails until it is restored. Run `fno doctor update` to re-point or remove the shim.",
                path_gh.unwrap_or("?")
            ),
        ),
        Some("active") => emit(
            err,
            &format!(
                "fno doctor: gh proxy: {} resolves first on PATH (active); plain gh reads carry the App token.",
                shim.unwrap_or("?")
            ),
        ),
        Some("shadowed") => emit(
            err,
            &format!(
                "fno doctor: gh proxy: shim at {} is shadowed by {}; plain gh skips it. Put the shim dir ahead: export PATH=\"<shim-dir>:$PATH\".",
                shim.unwrap_or("?"),
                path_gh.unwrap_or("nothing on PATH")
            ),
        ),
        Some("stale-shim") => emit(
            err,
            &format!(
                "fno doctor: gh proxy: {} is not an fno wrapper; a raw gh or a stale copy sits in the shim dir. Run `fno doctor update` to install or refresh the shim.",
                shim.unwrap_or("?")
            ),
        ),
        Some("not-installed") => emit(
            err,
            "fno doctor: gh proxy: shim not installed; plain gh runs raw. Run `fno doctor update`; it shadows the first gh on PATH with the proxy shim.",
        ),
        _ => {}
    }
}

fn emit_mux_front_door(view: &de::DoctorReport, err: bool) {
    match view.mux_front_door.as_deref() {
        Some("active") => emit(
            err,
            &format!(
                "fno doctor: mux front door: `fno` -> {} (active).",
                view.mux_binary.as_deref().unwrap_or("?")
            ),
        ),
        Some("not-installed") => emit(
            err,
            "fno doctor: mux front door: crates/fno not cargo-installed; bare `fno` will not launch the mux. Run `fno doctor update` (or cargo install --path crates/fno).",
        ),
        Some("shadowed") => emit(
            err,
            &format!(
                "fno doctor: mux front door: installed at {} but `fno` on PATH resolves to {}; the mux is shadowed. Ensure the cargo bin dir precedes any Python `fno` on PATH.",
                view.mux_binary.as_deref().unwrap_or("?"),
                view.path_fno.as_deref().unwrap_or("nothing on PATH")
            ),
        ),
        _ => {}
    }
}

fn emit_stale_servers(view: &de::DoctorReport, err: bool) {
    for _sess in view.mux_server_stale.iter().flatten() {
        emit(
            err,
            "fno doctor: mux server below the wire compatibility floor; run `fno agents restart` to cut it over (auto-heals pane-less servers; add `--mux` to also end servers with live panes).",
        );
    }
}

fn emit_running_components(view: &de::DoctorReport, err: bool) {
    for row in view.running_components.iter().flatten() {
        let verdict = row.verdict.as_deref().unwrap_or_default();
        if verdict != "stale" {
            continue;
        }
        let pid = row
            .pid
            .as_ref()
            .map(|p| format!(" pid {p}"))
            .unwrap_or_default();
        emit(
            err,
            &format!(
                "fno doctor: {} '{}'{} is an older build ({}); on restart: {}; keeps {}.",
                row.component.as_deref().unwrap_or("?"),
                row.name.as_deref().unwrap_or("unnamed"),
                pid,
                row.evidence.as_deref().unwrap_or("?"),
                row.on_restart.as_deref().unwrap_or("?"),
                row.survives.as_deref().unwrap_or("?")
            ),
        );
    }
}

fn emit_orphans(view: &de::DoctorReport, err: bool) {
    let orphans: Vec<String> = view.orphan_files.clone().unwrap_or_default();
    if orphans.is_empty() {
        return;
    }
    emit(
        err,
        &format!(
            "fno doctor: found {} orphaned file(s) from removed capture paths (safe to delete): {}.",
            orphans.len(),
            orphans.join(", ")
        ),
    );
}

fn emit_pr_watch(view: &de::DoctorReport, err: bool) {
    let pw = view.pr_watch.as_ref();
    let verdict = pw.and_then(|p| p.verdict.as_deref()).unwrap_or_default();
    let detail = pw.and_then(|p| p.detail.as_deref()).unwrap_or_default();
    if verdict == "dead" || verdict == "wedged" {
        let fix = pw.and_then(|p| p.fix.as_deref()).unwrap_or_default();
        let fallback = if verdict == "dead" {
            "fno do pr watch install"
        } else {
            "fno do pr watch refresh"
        };
        let fix = if fix.is_empty() { fallback } else { fix };
        emit(err, &format!(
            "fno doctor: pr-watch {verdict} ({}); run `{fix}`, then verify with `fno do pr watch status`.",
            detail
        ));
    } else if verdict == "healthy-pending" {
        emit(
            err,
            &format!("fno doctor: pr-watch installed, awaiting first tick ({detail})."),
        );
    }
}

fn emit_control_plane_arms(view: &de::DoctorReport, err: bool) {
    let cpa = view.control_plane_arms.as_ref();
    if let Some(reason) = cpa.and_then(|c| c.unknown_reason.as_deref()) {
        emit(
            err,
            &format!(
                "fno doctor: control-plane arms readout unknown ({reason}); staleness is unmeasured."
            ),
        );
    }
    for arm in cpa.and_then(|c| c.red.as_deref()).into_iter().flatten() {
        if let Some(line) = arm.line.as_deref() {
            emit(err, &format!("fno doctor: control-plane arm {line}"));
        } else if arm.producer_evidence.as_deref() == Some("unobserved") {
            emit(
                err,
                &format!(
                    "fno doctor: control-plane arm {} is UNOBSERVED (no producer receipt in the journals)",
                    arm.arm.as_deref().unwrap_or("?")
                ),
            );
        } else {
            emit(
                err,
                &format!(
                    "fno doctor: control-plane arm {} is STALE (last tick {}s ago, interval {}s, skip: {})",
                    arm.arm.as_deref().unwrap_or("?"),
                    arm.age_s.unwrap_or(0),
                    arm.interval_s.unwrap_or(0),
                    arm.skip_reason.as_deref().unwrap_or("none")
                ),
            );
        }
    }
    for line in cpa
        .and_then(|c| c.dead_sinks.as_ref())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        emit(err, &format!("fno doctor: {line}"));
    }
}

/// The web-bridge section, printed only when it has something to say: the
/// empty-machine doctor output is golden-frozen, so a silent section is the
/// stable shape.
fn emit_web_bridge(rep: &Value, err: bool) {
    let Some(wb) = rep
        .get("web_bridge")
        .cloned()
        .and_then(|v| serde_json::from_value::<crate::doctor::report::WebBridgeReport>(v).ok())
    else {
        return;
    };
    if wb.plist && !wb.loaded {
        emit(
            err,
            "fno doctor: web bridge: plist present but the label is not loaded; run `fno mux web install-agent` again, or read `launchctl print gui/$UID/<label>`",
        );
    }
    if wb.loaded && !wb.bridge_running {
        emit(
            err,
            &format!(
                "fno doctor: web bridge: the agent is loaded but the bridge is {} ({}); if this repeats, read ~/.fno/logs/web-bridge.err.log - KeepAlive may be restarting a bridge that cannot bind. Permanent stop: `fno mux web uninstall-agent`.",
                wb.bridge_detail.state, wb.bridge_detail.detail
            ),
        );
    }
    for row in &wb.answer_urls {
        if row.verdict == "unreachable" {
            emit(
                err,
                &format!(
                    "fno doctor: web bridge: answer_base_url {} answers {} (unreachable); the phone answer buttons are down",
                    row.url, row.status
                ),
            );
        }
    }
}

fn emit_observer_unavailable(view: &de::DoctorReport, err: bool) {
    let pw = view.pr_watch.as_ref();
    let verdict = pw.and_then(|p| p.verdict.as_deref()).unwrap_or_default();
    if verdict != "disabled" && verdict != "dead" {
        return;
    }
    let standing = report::standing_dispatch_grant();
    if !standing {
        return;
    }
    let fix = pw
        .and_then(|p| p.fix.as_deref())
        .filter(|f| !f.is_empty())
        .unwrap_or("fno config set pr_watch.enabled true");
    emit(
        err,
        &format!(
            "fno doctor: observer_unavailable - auto_merge.grant=dispatch is standing, but the PR watcher is {verdict} ({}); recorded grants will not execute. Run `{fix}`, then verify with `fno do pr watch status`.",
            pw.and_then(|p| p.detail.as_deref()).unwrap_or_default()
        ),
    );
}

fn emit_fd_limit(view: &de::DoctorReport, err: bool) {
    let fd = view.fd_limit.as_ref();
    if fd.and_then(|f| f.verdict.as_deref()) != Some("low") {
        return;
    }
    let soft_v = fd.and_then(|f| f.soft.as_ref());
    let soft = soft_v
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".into());
    let floor = fd.and_then(|f| f.threshold).unwrap_or(1024);
    let launchd = fd.and_then(|f| f.launchd_soft.as_ref());
    if launchd.is_none() {
        emit(
            err,
            &format!("fno doctor: open-file soft limit is {soft} in THIS process (floor {floor})."),
        );
        emit(
            err,
            "Raise it in the launch context that starts this process (shell profile, service unit, or container limits)."
        );
        return;
    }
    let soft_num = soft_v.and_then(Value::as_i64);
    let floor2 = fd.and_then(|f| f.threshold).unwrap_or(1024);
    let below = soft_num.is_some() && soft_num.unwrap_or(0) <= floor2;
    if below {
        emit(
            err,
            &format!(
                "fno doctor: open-file soft limit is {} in THIS process (floor {}).",
                soft_num.unwrap_or(0),
                floor2
            ),
        );
    } else {
        emit(err, &format!(
            "fno doctor: open-file soft limit is {} in THIS process, but launchd's session default is {} (floor {}).",
            soft_num.unwrap_or(0),
            launchd.and_then(Value::as_i64).unwrap_or(0),
            floor2
        ));
    }
    let kern = fd.and_then(|f| f.kern_maxfiles);
    let kern_note = match kern {
        Some(k) => format!("kern.maxfiles is {k}, so the kernel is not the constraint"),
        None => "the kernel's own ceiling is far higher than either number".to_string(),
    };
    emit(err, &format!(
        "The limit is inherited from the launch context, not the machine: {kern_note}, and a login shell can read a healthy number while every spawned worker starves."
    ));
    emit(
        err,
        "Raise it for launchd children: sudo launchctl limit maxfiles 65536 unlimited",
    );
    emit(err, "The raise only reaches NEWLY launched processes, so restart the affected sessions afterwards. Persistence across reboot needs a LaunchDaemon plist.");
}

fn emit_dead_letter(view: &de::DoctorReport, err: bool) {
    let dl = view.dead_letter.as_ref();
    if dl.and_then(|d| d.drain_hook_wired) == Some(false) {
        emit(
            err,
            "fno doctor: a2a drain-self SessionStart hook is NOT wired; durable mail to this claude env will strand (senders see 'queued' forever). Wire inject-mail-drain-session-start.sh into hooks.json SessionStart.",
        );
    }
    let stale: Vec<String> = dl
        .and_then(|d| d.stale_unread.as_deref())
        .into_iter()
        .flatten()
        .filter_map(|row| row.handle.clone())
        .collect();
    if !stale.is_empty() {
        let mut uniq: Vec<String> = stale.clone();
        uniq.sort();
        uniq.dedup();
        emit(
            err,
            &format!(
                "fno doctor: {} unread a2a message(s) stranded on the bus for a dead handle ({}); no live session will ever drain them.",
                stale.len(),
                uniq.join(", ")
            ),
        );
    }
}

fn emit_managed_block(view: &de::DoctorReport, err: bool) {
    let mb = view.managed_block.as_ref();
    if mb.and_then(|m| m.stale) == Some(true) {
        emit(
            err,
            &format!(
                "fno doctor: {} footnote block is v{} (current v{}); re-run `fno config setup` to refresh it.",
                mb.and_then(|m| m.file.as_deref()).unwrap_or("?"),
                mb.and_then(|m| m.stamped).unwrap_or(0),
                mb.and_then(|m| m.current).unwrap_or(1)
            ),
        );
    }
}

fn emit_surface_lines(view: &de::DoctorReport, err: bool) {
    let surf = view.harness_surface.as_ref();
    for line in surf
        .and_then(|s| s.opencode_doctor_lines.as_deref())
        .into_iter()
        .flatten()
    {
        if let Some(text) = line.text.as_deref() {
            emit(err, &format!("fno doctor: {text}"));
        }
    }
    emit_codex_context_window(surf, err);
    let plugin = surf.and_then(|s| s.codex_plugin.as_ref());
    let status = plugin.and_then(|p| p.status.as_deref()).unwrap_or_default();
    if status == "fresh" {
        emit(
            err,
            &format!(
                "fno doctor: codex plugin: fresh (channel={} version={} digest={}).",
                plugin.and_then(|p| p.channel.as_deref()).unwrap_or("?"),
                plugin
                    .and_then(|p| p.cache_version.as_deref())
                    .unwrap_or("?"),
                short12(
                    plugin
                        .and_then(|p| p.cache_digest.as_deref())
                        .unwrap_or_default()
                )
            ),
        );
    } else if !status.is_empty() {
        let detail = plugin
            .and_then(|p| p.issue.as_deref())
            .filter(|i| !i.is_empty())
            .unwrap_or(status);
        let enabled: Vec<String> = plugin
            .and_then(|p| p.enabled_plugin_ids.clone())
            .unwrap_or_default();
        let remedy = plugin.and_then(|p| p.remedy.as_deref()).unwrap_or("None");
        emit(
            err,
            &format!(
                "fno doctor: codex plugin: {} ({}; enabled={}); run `{}`.",
                status.to_uppercase(),
                detail,
                if enabled.is_empty() {
                    "none".to_string()
                } else {
                    enabled.join(", ")
                },
                remedy
            ),
        );
    }
}

fn emit_codex_context_window(surf: Option<&de::Surface>, err: bool) {
    let report = surf.and_then(|s| s.codex_context_window.as_ref());
    let overstated = report.and_then(|r| r.overstated) == Some(true);
    let leans = report.and_then(|r| r.leans_on_cached_cap) == Some(true);
    if !overstated && !leans {
        return;
    }
    let kept = report
        .and_then(|r| r.percent)
        .map(|p| format!(" (codex keeps {p})"))
        .unwrap_or_default();
    let mut line = format!(
        "fno doctor: codex {}={} with {}={} runs at an effective {}{kept}.",
        report
            .and_then(|r| r.model_source.as_deref())
            .unwrap_or_default(),
        report.and_then(|r| r.model.as_deref()).unwrap_or_default(),
        report
            .and_then(|r| r.window_source.as_deref())
            .unwrap_or_default(),
        report
            .and_then(|r| r.configured.as_ref())
            .map(Value::to_string)
            .unwrap_or_default(),
        report
            .and_then(|r| r.effective.as_ref())
            .map(Value::to_string)
            .unwrap_or_default()
    );
    if overstated {
        line.push_str(" The TUI footer shows the configured value, not this one.");
    }
    if leans {
        let cap = report.and_then(|r| r.max_context_window).unwrap_or(0);
        let base = report.and_then(|r| r.context_window).unwrap_or(cap);
        let stamp = report
            .and_then(|r| r.cache_fetched_at.as_deref())
            .unwrap_or_default();
        let client = report
            .and_then(|r| r.cache_client_version.as_deref())
            .unwrap_or_default();
        let fetched = match (stamp.is_empty(), client.is_empty()) {
            (false, false) => format!("(fetched {stamp} by codex {client})"),
            (false, true) => format!("(fetched {stamp})"),
            (true, false) => format!("(written by codex {client})"),
            (true, true) => "(the cache records no fetch time)".to_string(),
        };
        let model = report.and_then(|r| r.model.as_deref()).unwrap_or_default();
        let pct = report.and_then(|r| r.percent);
        let base_tier = pct
            .map(|p| (base as f64 * p / 100.0).floor() as i64)
            .unwrap_or(base);
        let provenance = format!(
            "{fetched}. That cap is served per fetching client, surface and originator both, and the cache records neither"
        );
        let synthesized = report.and_then(|r| r.base_synthesized) == Some(true);
        if cap > base {
            line.push_str(&format!(
                " models_cache.json holds {model} at {cap}, above its {base} base {provenance}. A base-tier fetch drops this to {base_tier}."
            ));
        } else if cap == base && !synthesized {
            line.push_str(&format!(
                " models_cache.json caps {model} at {cap}, its base {provenance}, so whichever launcher fetched last set it for every thread started since."
            ));
        } else {
            line.push_str(&format!(
                " models_cache.json puts {model} at {cap} {provenance}, so whichever launcher fetched last set it for every thread started since."
            ));
        }
        let daemon_live = report.and_then(|r| r.app_server_running) == Some(true);
        if daemon_live {
            line.push_str(" A codex app-server daemon is live here and every app-server fetch lands the base cap, so it will keep pulling this back down.");
        }
    }
    emit(err, &line);
}

fn emit_groom(view: &de::DoctorReport, err: bool) {
    let gr = view.groom.as_ref();
    let state = gr.and_then(|g| g.state.as_deref()).unwrap_or_default();
    let agent_installed = gr.and_then(|g| g.agent_installed) == Some(true);
    if state == "never" {
        let remedy = if agent_installed {
            " despite an installed agent; check ~/.fno/logs/groom.err.log."
        } else {
            "; run `fno backlog groom --install-agent` to schedule it daily."
        };
        emit(
            err,
            &format!("fno doctor: backlog grooming has NEVER run{remedy}"),
        );
    } else if gr.and_then(|g| g.stale) == Some(true) {
        emit(err, &format!(
            "fno doctor: backlog grooming last ran {}h ago (the daily pass is not running); check `launchctl list | grep {}` and ~/.fno/logs/groom.err.log.",
            gr.and_then(|g| g.hours).unwrap_or(0.0) as i64,
            crate::setup_services::groom_label()
        ));
    }
}

fn emit_post_merge_sync(view: &de::DoctorReport, err: bool) {
    let pms = view.post_merge_sync.as_ref();
    if pms.and_then(|p| p.stale) == Some(true) {
        emit(err, &format!(
            "fno doctor: post-merge sync STALE - the canonical checkout is not synced with recent merges ({}); run `fno do pr sync-canonical --pr-number <n>` and check ~/.fno/logs/pr-watcher.err.log.",
            pms.and_then(|p| p.detail.as_deref()).unwrap_or_default()
        ));
    } else if pms.and_then(|p| p.state.as_deref()) == Some("unknown") {
        emit(err, &format!(
            "fno doctor: post-merge sync UNKNOWN - could not read merge state ({}); run `gh auth status`.",
            pms.and_then(|p| p.detail.as_deref()).filter(|d| !d.is_empty()).unwrap_or("gh unavailable or unauthenticated")
        ));
    }
}

fn emit_evals(view: &de::DoctorReport, err: bool) {
    let ev = view.evals.as_ref();
    if ev.is_none() || ev.and_then(|e| e.never_ran) == Some(true) {
        let detail = if ev.is_none() {
            "no eval history"
        } else {
            "no regression-tier run on record"
        };
        emit(
            err,
            &format!(
            "fno doctor: evals UNKNOWN ({detail}); run `fno doctor evals run --tier regression -y`."
        ),
        );
    } else if ev.and_then(|e| e.stale) == Some(true) {
        emit(err, &format!(
            "fno doctor: evals STALE - the newest regression-tier run is {}d old; run `fno doctor evals run --tier regression -y`.",
            ev.and_then(|e| e.age_days).unwrap_or(0)
        ));
    } else if ev
        .and_then(|e| e.regressed.as_deref())
        .is_some_and(|a| !a.is_empty())
    {
        let names: Vec<String> = ev.and_then(|e| e.regressed.clone()).unwrap_or_default();
        let window = ev.and_then(|e| e.window_days).unwrap_or(7);
        emit(err, &format!(
            "fno doctor: evals REGRESSING - {} dropped against the prior {window}d window; run `fno doctor evals trend`.",
            names.join(", ")
        ));
    }
}

fn emit_launch_agents(view: &de::DoctorReport, err: bool) {
    let agents = view.launch_agents.as_ref();
    if agents.and_then(|a| a.applicable) == Some(false) {
        emit(
            err,
            "fno doctor: LaunchAgent health: not applicable (no launchctl on this host).",
        );
    }
    for entry in agents.and_then(|a| a.dead.as_deref()).into_iter().flatten() {
        let label = entry.label.as_deref().unwrap_or("?");
        let remedy = if label == crate::setup_services::pr_watcher_label() {
            "run `fno do pr watch refresh`"
        } else {
            "re-run `fno doctor update` if the entry point moved"
        };
        let exit_code = entry.exit.as_ref();
        let exit_text = exit_code
            .map(Value::to_string)
            .unwrap_or_else(|| "exit unknown".to_string());
        emit(err, &format!(
            "fno doctor: LaunchAgent {label} last exited {exit_text} (it is installed but failing); check its log under ~/.fno/ and {remedy}."
        ));
    }
}

fn emit_silent_switches(view: &de::DoctorReport, err: bool) {
    let ss = view.silent_switches.as_ref();
    let stamp = ss.and_then(|s| s.posture.as_ref());
    if let Some(p) = stamp.and_then(|s| s.posture.as_deref()) {
        emit(
            err,
            &format!(
                "fno doctor: applied posture: {p} (applied {}, scope {}).",
                stamp
                    .and_then(|s| s.applied_at.as_ref())
                    .map(Value::to_string)
                    .unwrap_or_else(|| "?".into()),
                stamp
                    .and_then(|s| s.scope.as_ref())
                    .map(Value::to_string)
                    .unwrap_or_else(|| "?".into())
            ),
        );
    }
    for f in ss.and_then(|s| s.findings.as_deref()).into_iter().flatten() {
        emit_silent_finding(f, err);
    }
}

fn emit_silent_finding(f: &de::SilentFinding, err: bool) {
    let sw = f.switch.as_deref().unwrap_or_default();
    let cmd = f.command.as_deref().unwrap_or_default();
    let count = f.count.as_ref().map(Value::to_string).unwrap_or_default();
    let label = f.count_label.as_deref().unwrap_or_default();
    if f.direction.as_deref() == Some("inaction") {
        let clause = if count.is_empty() {
            " (idle)".to_string()
        } else {
            format!(" but {count} {label} waiting")
        };
        emit(
            err,
            &format!(
                "fno doctor: {sw} is OFF{clause}; nothing is happening. Run `{cmd}` to enable."
            ),
        );
    } else {
        let clause = if count.is_empty() {
            String::new()
        } else {
            format!(" ({count} {label})")
        };
        emit(err, &format!(
            "fno doctor: {sw} is ARMED{clause}; a green PR can merge unattended. Run `{cmd}` to disarm."
        ));
    }
    if let Some(cause) = f.cause.as_deref() {
        emit(err, &format!("  cause: {cause}"));
    }
}

fn emit_review_gap(view: &de::DoctorReport, err: bool) {
    let gap = view.auto_merge_review_gap.as_ref();
    let Some(gap) = gap else {
        return;
    };
    let manifests = gap.manifests.unwrap_or(0);
    let breakdown = gap.breakdown.as_deref().unwrap_or_default();
    let label = if breakdown.is_empty() {
        format!("{manifests} armed manifest(s)")
    } else {
        format!("{manifests} armed manifest(s): {breakdown}")
    };
    let keys: Vec<String> = gap.keys.clone().unwrap_or_default();
    emit(
        err,
        &format!(
            "fno doctor: auto_merge is ARMED with zero review lanes - the merge gate requires no review for a code payload ({}; {label}). Remedy: {}",
            keys.join("; "),
            gap.remedy.as_deref().unwrap_or_default()
        ),
    );
}

fn emit_optout_residue(view: &de::DoctorReport, err: bool) {
    let findings = view
        .merge_gating_optouts
        .as_ref()
        .and_then(|m| m.residue.clone())
        .unwrap_or_default();
    for f in findings {
        emit(
            err,
            &format!(
                "fno doctor: {} remains set to its opt-out value in {} without a live claim ({}); run `{}` or let the reaper restore it.",
                f.key.as_deref().unwrap_or_default(),
                f.path.as_deref().unwrap_or_default(),
                f.claim_state.as_deref().unwrap_or_default(),
                f.command.as_deref().unwrap_or_default()
            ),
        );
    }
}

fn emit_plugin_cache(view: &de::DoctorReport, err: bool) {
    let pc = view.plugin_cache.as_ref();
    for root in pc.and_then(|p| p.roots.as_deref()).into_iter().flatten() {
        let role = if root.live == Some(true) {
            "live"
        } else {
            "second copy"
        };
        emit(
            err,
            &format!(
                "fno doctor: plugin root ({}, {role}): {}: {}{}",
                root.origin.as_deref().unwrap_or_default(),
                root.path.as_deref().unwrap_or_default(),
                root.status.as_deref().unwrap_or_default(),
                root.note.as_deref().unwrap_or_default()
            ),
        );
    }
    if let Some(pc) = pc {
        if pc.roots.as_ref().is_none_or(|a| a.is_empty()) {
            emit(
                err,
                &format!(
                    "fno doctor: plugin stage freshness unknown ({}).",
                    pc.detail.as_deref().unwrap_or_default()
                ),
            );
        }
    }
    for f in pc
        .and_then(|p| p.rename_findings.as_deref())
        .into_iter()
        .flatten()
    {
        emit(
            err,
            &format!(
                "fno doctor: old-name install: {}. Fix: {}",
                f.detail.as_deref().unwrap_or_default(),
                f.command.as_deref().unwrap_or_default()
            ),
        );
    }
}

fn emit_codex_dual(view: &de::DoctorReport, err: bool) {
    let dual = view
        .harness_surface
        .as_ref()
        .and_then(|s| s.codex_hooks_dual.as_ref());
    if dual == Some(&Value::Bool(true)) {
        emit(
            err,
            "fno doctor: codex hooks load from both config.toml and hooks.json; run `fno config setup cli-hooks-codex --migrate-legacy-hooks-json` to converge (or `fno doctor --codex-hooks` for detail)."
        );
    }
}

fn emit_pre_push(view: &de::DoctorReport, err: bool) {
    let pp = view.pre_push_hook.as_ref();
    if pp.and_then(|p| p.status.as_deref()) == Some("legacy") {
        emit(
            err,
            "fno doctor: the installed pre-push hook gates on the pushing checkout's branch, so it refuses every push from a canonical checkout on main. Run `bash scripts/install-pre-push-hook.sh` to replace it (the old file is backed up)."
        );
    }
}

fn emit_plugin_hooks(view: &de::DoctorReport, err: bool) {
    let ph = view.plugin_hooks.as_ref();
    if ph.and_then(|p| p.applicable) == Some(true) {
        for merr in ph
            .and_then(|p| p.manifest_errors.as_deref())
            .into_iter()
            .flatten()
        {
            emit(
                err,
                &format!(
                "fno doctor: hook manifest {}: {} [{}] - that harness loads no hooks (fail-open).",
                merr.issue.as_deref().unwrap_or_default(),
                merr.path.as_deref().unwrap_or_default(),
                merr.manifest.as_deref().unwrap_or_default()
            ),
            );
        }
        for miss in ph
            .and_then(|p| p.missing_scripts.as_deref())
            .into_iter()
            .flatten()
        {
            emit(
                err,
                &format!(
                    "fno doctor: hook references MISSING script {} [{}/{}] - the guard fails open with no signal.",
                    miss.script.as_deref().unwrap_or_default(),
                    miss.manifest.as_deref().unwrap_or_default(),
                    miss.event.as_deref().unwrap_or_default()
                ),
            );
        }
        for failure in ph.and_then(|p| p.failures.as_deref()).into_iter().flatten() {
            let stderr = failure.stderr.as_deref().unwrap_or_default();
            let tail = if stderr.is_empty() {
                String::new()
            } else {
                let cut: String = stderr.chars().take(160).collect();
                format!(" - {cut}")
            };
            emit(
                err,
                &format!(
                    "fno doctor: hook LAUNCH FAILED (rc={}): {} [{}/{}]{}",
                    failure
                        .rc
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| "None".into()),
                    failure.resolved.as_deref().unwrap_or_default(),
                    failure.manifest.as_deref().unwrap_or_default(),
                    failure.event.as_deref().unwrap_or_default(),
                    tail
                ),
            );
        }
    } else if !ph
        .and_then(|p| p.reason.as_deref())
        .unwrap_or_default()
        .is_empty()
    {
        emit(
            err,
            &format!(
                "fno doctor: plugin hook probe skipped ({}).",
                ph.and_then(|p| p.reason.as_deref()).unwrap_or_default()
            ),
        );
    }
}

fn emit_codex_foreign(view: &de::DoctorReport, err: bool) {
    let foreign = view
        .harness_surface
        .as_ref()
        .and_then(|s| s.codex_hooks_foreign_json.as_deref())
        .map(|a| a.len())
        .unwrap_or(0);
    if foreign > 0 {
        emit(err, &format!(
            "fno doctor: codex hooks: {foreign} foreign hook(s) in ~/.codex/hooks.json (informational; run `fno doctor --codex-hooks` for the per-command config.toml/hooks.json breakdown)."
        ));
    }
}

fn emit_codex_app_server(view: &de::DoctorReport, err: bool) {
    let cas = view.codex_app_server.as_ref();
    if cas.and_then(|c| c.present) != Some(true) {
        emit(
            err,
            "fno doctor: codex app-server daemon not running (no daemon listening on its control socket); live mail to codex sessions demotes to durable. Start it BEFORE the codex TUI: `codex app-server daemon start` (or `codex app-server daemon bootstrap` for durable SSH-driven use). A session launched before the daemon cannot receive live mail without a restart."
        );
    }
}

/// The `--codex-hooks` arm's human render (doctor.py `_emit_codex_hooks_report`):
/// one summary line, then the actionable diagnostics.
pub(crate) fn emit_codex_hooks_report(result: &Value, err: bool) {
    let view = de::parse::<de::CodexHooksReport>(result);
    let status = view.status.as_deref().unwrap_or("");
    let wired = view.footnote_toml_wired.unwrap_or(false);
    let trust_map: Vec<(String, String)> = view.footnote_toml_trust.clone().unwrap_or_default();
    let trust_states: Vec<&str> = trust_map.iter().map(|(_, v)| v.as_str()).collect();
    let verified = view.footnote_toml_trust_verified.unwrap_or(false);
    let trust = if verified {
        "verified"
    } else if trust_states.contains(&"recorded-unverified") {
        "recorded-unverified"
    } else if wired {
        "missing"
    } else {
        "n/a"
    };
    let state = view.state.as_deref().unwrap_or("");
    emit(
        err,
        &format!(
            "fno doctor: codex hooks: {status} preferred=config.toml; footnote SessionStart={}; trust={trust}; layers={state}.",
            if wired { "wired" } else { "missing" }
        ),
    );
    for error in view.errors.as_deref().unwrap_or(&[]) {
        emit(
            err,
            &format!(
                "fno doctor: codex hooks: parse error: {}",
                error.as_str().unwrap_or("")
            ),
        );
    }
    for (key, state) in &trust_map {
        emit(
            err,
            &format!("fno doctor: codex hooks: trust state {state}: {key}"),
        );
    }
    if let Some(plugin_hooks) = view.plugin_hooks.as_ref() {
        let reason = plugin_hooks.reason.as_deref().unwrap_or("");
        match plugin_hooks.status.as_deref() {
            Some("ok") => emit(
                err,
                &format!(
                    "fno doctor: codex hooks: plugin-hook trust: ok ({} fno hooks trusted)",
                    plugin_hooks.trusted.unwrap_or(0)
                ),
            ),
            Some("untrusted") => emit(
                err,
                &format!(
                    "fno doctor: codex hooks: plugin-hook trust: {} of {} fno hooks codex would refuse; fix: `fno doctor --codex-hooks --fix`.",
                    plugin_hooks.not_trusted.unwrap_or(0),
                    plugin_hooks.checked.unwrap_or(0)
                ),
            ),
            _ => emit(
                err,
                &format!(
                    "fno doctor: codex hooks: plugin-hook trust: skipped ({}).",
                    if reason.is_empty() {
                        plugin_hooks.status.as_deref().unwrap_or("unknown")
                    } else {
                        reason
                    }
                ),
            ),
        }
    }
    if wired && trust != "verified" {
        if trust_states.contains(&"recorded-unverified") {
            emit(
                err,
                "fno doctor: codex hooks: approval record found, but its trusted_hash was not locally verified; refresh plugin-hook trust with `fno doctor --codex-hooks --fix`.",
            );
        } else {
            emit(
                err,
                "fno doctor: codex hooks: approve the footnote SessionStart hook in Codex, or refresh plugin-hook trust with `fno doctor --codex-hooks --fix`.",
            );
        }
    }
    if view.duplicate_layers == Some(true) {
        let hooks_path = view.hooks_json_path.as_deref().unwrap_or("");
        let config_path = view.config_path.as_deref().unwrap_or("");
        emit(
            err,
            &format!(
                "fno doctor: codex hooks: loading hooks from both {hooks_path} and {config_path}; config.toml is preferred."
            ),
        );
    }
    let json_hooks: Vec<&str> = view
        .footnote_json_hooks
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    if !json_hooks.is_empty() {
        emit(
            err,
            "fno doctor: codex hooks: run `fno config setup cli-hooks-codex --migrate-legacy-hooks-json` to remove only footnote-owned legacy JSON hooks.",
        );
    }
    let foreign: Vec<&str> = view
        .foreign_json_hooks
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    let config_path = view.config_path.as_deref().unwrap_or("");
    for command in foreign {
        emit(
            err,
            &format!(
                "fno doctor: codex hooks: foreign legacy JSON hook preserved: {command}; manually consolidate it into {config_path} if desired."
            ),
        );
    }
    if !wired && view.errors.as_deref().unwrap_or(&[]).is_empty() {
        let mut command = "fno config setup cli-hooks-codex".to_string();
        if !json_hooks.is_empty() {
            command += " --migrate-legacy-hooks-json";
        }
        emit(
            err,
            &format!("fno doctor: codex hooks: run `{command}` to wire the preferred TOML hook."),
        );
    }
}

/// The `--codex-bind` arm's human render (doctor.py `_emit_codex_bind_report`).
pub(crate) fn emit_codex_bind_report(result: &Value) {
    let view = de::parse::<de::CodexBind>(result);
    let bound = view.bound.unwrap_or(false);
    let version = view.codex_version.as_deref().unwrap_or("unknown");
    if bound {
        let session = view.session_id.as_deref().unwrap_or("");
        let oracle = view.oracle.as_deref().unwrap_or("");
        let elapsed = view.elapsed_s.unwrap_or(0);
        emit(
            false,
            &format!("fno doctor: codex bind: ok, session={session} oracle={oracle} elapsed={elapsed}s codex={version}"),
        );
    } else {
        let error = view.error.as_deref().unwrap_or("");
        emit(
            false,
            &format!("fno doctor: codex bind: FAILED codex={version}: {error}"),
        );
    }
}
