//! How `fno mux workspace restore` walks its members : the split
//! (classify on the core loop, resolve claude re-entry plans off it) and
//! the bulk apply half that walks every candidate through
//! [`super::Core::resume_one`]. Extracted from server.rs under the
//! file-budget gate - the code the change touched moves with it.

use super::*;

/// One member's post-resume registry rebind: the restore walk just spawned
/// its pane, and the registry row still names the dead one.
struct RebindJob {
    member: String,
    harness: String,
    session: String,
    mux_session: String,
    pane: u64,
    pid: u32,
}

/// Whether the rebind may fire: the off-loop task needs the ambient runtime
/// the core loop runs on. A loop-less context (a unit test driving
/// `Core::handle` directly) has none, so it skips the rebind and replies
/// inline - the same shape the restore tests asserted before this door
/// existed. Shared with the gesture's single-row rebind.
pub(super) fn rebind_runtime_ready() -> bool {
    tokio::runtime::Handle::try_current().is_ok()
}

/// What the shelled muse revive answered, so the restore row reads the
/// truth instead of parsing the door's prose twice.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MuseRevival {
    /// Exit 0 with a launch receipt: a fresh host serves the session.
    Revived,
    /// Exit 0 with "already live": the host held the session all along.
    AlreadyLive,
}

impl super::Core {
    pub(super) fn handle_workspace_restore(
        &mut self,
        dry_run: bool,
        harness: Option<String>,
        member_session: Option<String>,
        reply: ControlReply,
    ) {
        if !self.restored {
            let _ = reply.send(ServerMsg::Err {
                code: crate::proto::err_code::RESTORE_NOT_RUN,
                msg: "startup restore has not run in this session yet: attach once, then re-run"
                    .into(),
            });
            return;
        }
        self.workspace_restore_start(dry_run, harness, member_session, reply);
    }

    /// The live, non-tombstoned worker members a restore acts on,
    /// as (worker name, member) pairs in stored order. `harness` narrows the
    /// run to one harness's members. A member with no worker name records no
    /// resumable identity, so it is never a candidate. The same
    /// classifier the startup loop answers to drops the Gone members here,
    /// so the command and the startup path can never disagree about who is a
    /// corpse.
    fn restore_candidates(
        &self,
        harness: Option<&str>,
    ) -> Vec<(String, crate::squad_store::StoredMember)> {
        let known_workers = self.known_worker_names();
        let receipts = crate::spawn_journal::scan_spawn_journal().receipts;
        self.squad_members
            .values()
            .flatten()
            .filter(|m| !m.tombstone)
            .filter(|m| m.worker.as_deref().is_some_and(|w| !w.trim().is_empty()))
            .filter(|m| harness.is_none_or(|h| m.harness.as_deref() == Some(h)))
            // Muse thread rows ride the muse pass below (the host revive),
            // never a pane spawn: the stored-member walk owns claude-family
            // panes, and a muse member here would report twice.
            .filter(|m| m.harness.as_deref() != Some("muse"))
            .filter(|m| {
                classify_member(m, known_workers.as_ref(), &receipts) != MemberVerdict::Gone
            })
            .map(|m| (m.worker.clone().expect("checked above"), m.clone()))
            .collect()
    }

    fn targeted_restore_candidates(
        &self,
        harness: Option<&str>,
        session: Option<&str>,
    ) -> Vec<(String, crate::squad_store::StoredMember)> {
        self.restore_candidates(harness)
            .into_iter()
            .filter(|(_, member)| {
                session.is_none_or(|sid| member.harness_session_id.as_deref() == Some(sid))
            })
            .collect()
    }

    /// `fno mux workspace restore`, phase 1: split the run. A dry
    /// run never resolves plans (it reports classifications and spawns
    /// nothing), and a run with no claude members needs none, so both apply
    /// inline. Otherwise the claude members' re-entry plans resolve OFF the
    /// core loop (the BatchPlansReady shape) and the apply half re-enters
    /// with the verdicts in hand - no bare claude resume on this axis, and
    /// no per-member resolver wait landing on the loop.
    pub(super) fn workspace_restore_start(
        &mut self,
        dry_run: bool,
        harness: Option<String>,
        member_session: Option<String>,
        reply: ControlReply,
    ) {
        // The off-loop registry reader ticks independently and may never have
        // run in a session nobody has watched (a headless reboot-restore).
        // This command's classification IS a registry read, so it reads the file
        // itself rather than refuse members whose rows exist on disk.
        if let Some(rows) = restore_registry_rows() {
            self.agents = rows;
        }
        let claude_names: Vec<String> = self
            .targeted_restore_candidates(harness.as_deref(), member_session.as_deref())
            .into_iter()
            .filter(|(_, m)| m.harness.as_deref() == Some("claude"))
            .map(|(name, _)| name)
            .collect();
        let muse_names: Vec<String> = if harness.as_deref().is_none_or(|h| h == "muse") {
            self.muse_restore_candidates(member_session.as_deref())
                .into_iter()
                .map(|row| row.name)
                .collect()
        } else {
            Vec::new()
        };
        let portal_names = if member_session.is_none() {
            portal_reach::portals_needing_claude_plan(self)
        } else {
            Vec::new()
        };
        // Every member re-seats a row that already held a seat, so no spawn
        // gate is asked: with no off-loop resolution owed (claude plans,
        // portal plans, muse revives), the apply runs now.
        if claude_names.is_empty() && portal_names.is_empty() && muse_names.is_empty() {
            self.workspace_restore_apply(
                dry_run,
                harness,
                member_session,
                HashMap::new(),
                HashMap::new(),
                reply,
            );
            return;
        }
        let core_tx = self.self_tx.clone();
        tokio::spawn(async move {
            let plans = agent_actions::resolve_restore_plans(claude_names, portal_names).await;
            // The muse revives shell in the same off-loop batch: each shells
            // `fno-agents resume` bounded, and the apply reads the verdicts
            // like the claude plans. A dry run owes no shells.
            let mut muse_results: HashMap<String, Result<MuseRevival, String>> = HashMap::new();
            if !dry_run {
                let mut set = tokio::task::JoinSet::new();
                for name in muse_names {
                    set.spawn(async move {
                        (name.clone(), agent_actions::run_muse_revive(&name).await)
                    });
                }
                while let Some(joined) = set.join_next().await {
                    if let Ok((name, verdict)) = joined {
                        muse_results.insert(name, verdict);
                    }
                }
            }
            let _ = core_tx
                .send(CoreMsg::WorkspaceRestoreApply {
                    dry_run,
                    harness,
                    member_session,
                    plans,
                    muse_results,
                    reply,
                })
                .await;
        });
    }

    /// Registry muse thread rows the restore's muse pass owns: harness
    /// `muse`, thread substrate, a session id, placed in this session's
    /// workspaces by the same resolver the sideline asks. A headless muse
    /// row is one exec turn and never rides the host revive; a row without
    /// a workspace placement is not this session's to restore.
    fn muse_restore_candidates(
        &self,
        member_session: Option<&str>,
    ) -> Vec<crate::agents_view::RegistryAgent> {
        self.agents
            .iter()
            .filter(|a| {
                a.harness.as_deref() == Some("muse")
                    && a.substrate.as_deref() == Some("thread")
                    && a.harness_session_id
                        .as_deref()
                        .is_some_and(|s| !s.is_empty())
                    && member_session.is_none_or(|sid| a.harness_session_id.as_deref() == Some(sid))
            })
            .filter(|a| self.thread_workspace(a).is_some())
            .cloned()
            .collect()
    }

    /// `fno mux workspace restore`, phase 2: walk every candidate
    /// through [`super::Core::resume_one`] and report one row per member. A claude
    /// member whose plan refused (or never resolved) refuses with the
    /// resolver's own reason - never a bare claude resume - and
    /// `reentry_verdict` is cleared around every attempt so one member's
    /// verdict can never leak into the next one's argv.
    pub(super) fn workspace_restore_apply(
        &mut self,
        dry_run: bool,
        harness: Option<String>,
        member_session: Option<String>,
        mut plans: HashMap<String, Result<ReentryVerdict, String>>,
        muse_results: HashMap<String, Result<MuseRevival, String>>,
        reply: ControlReply,
    ) {
        use self::portal_reach::RESTORE_CLIENT;
        let candidates =
            self.targeted_restore_candidates(harness.as_deref(), member_session.as_deref());
        // One rebind job per resumed member whose registry row
        // carried a native session id: the pane is running, and the row it
        // left behind names the DEAD pane, so mail, pane send, and
        // `fno agents resume` refuse it until this lands. The jobs run
        // off-loop after the walk (the tail of this fn); the dry run
        // reports nothing and spawns nothing.
        let mut rebind_jobs: Vec<RebindJob> = Vec::new();
        // A worker NAME the store holds more than once (distinct sessions,
        // one display name - a supported state) refuses up front instead of
        // reaching resume_one: the second twin would find the first one's
        // pane through the name-only map and report "focused" while its own
        // session was never restored.
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for (name, _) in &candidates {
            *name_counts.entry(name.clone()).or_default() += 1;
        }
        let dims = (crate::vt::DEFAULT_ROWS, crate::vt::DEFAULT_COLS);
        // A reap receipt is the session's death record: a member it preserves
        // must not read as a live restore candidate, or restore resurrects a
        // session the fleet deliberately retired.
        let retired = crate::restore_gate::retired_receipt_session_ids().unwrap_or_default();
        let retired_leads = crate::restore_gate::retired_lead_refusals();
        let mut rows = Vec::with_capacity(candidates.len());
        for (name, member) in candidates {
            if let Some(reason) =
                crate::restore_gate::retired_refusal(member.harness_session_id.as_deref(), &retired)
                    .or_else(|| {
                        member
                            .harness_session_id
                            .as_deref()
                            .and_then(|session| retired_leads.get(session).cloned())
                    })
            {
                rows.push(restore_route_gate::refused_row(
                    name,
                    member.harness.clone(),
                    reason,
                ));
                continue;
            }
            if name_counts.get(name.as_str()).copied().unwrap_or(0) > 1 {
                rows.push(restore_route_gate::refused_row(
                    name,
                    member.harness.clone(),
                    "member name is ambiguous in the store; resume by exact session id".into(),
                ));
                continue;
            }
            let harness_name = member.harness.clone();
            // a routed codex member refuses before any staging. A pane
            // spawn's only env channel is an argv prefix (visible in ps), so it
            // cannot carry the route's key; `fno agents resume` is the door
            // that restores the route, and the member is marked refused here.
            if harness_name.as_deref() == Some("codex") {
                let row = self.agents.iter().find(|a| {
                    agent_harness_session_id(a)
                        == member
                            .harness_session_id
                            .as_deref()
                            .filter(|s| !s.is_empty())
                });
                if let Some(reason) =
                    row.and_then(|row| restore_route_gate::member_routed_codex_refusal(row, &name))
                {
                    rows.push(restore_route_gate::refused_row(name, harness_name, reason));
                    continue;
                }
            }
            // A claude member without a resolvable plan refuses here instead
            // of firing a stray off-loop resolution from the bulk path; the
            // single gesture keeps its own replay behavior.
            if !dry_run && harness_name.as_deref() == Some("claude") {
                match plans.get(&name) {
                    Some(Ok(_)) => {
                        self.reentry_verdict = plans.remove(&name).and_then(|r| r.ok());
                    }
                    Some(Err(reason)) => {
                        rows.push(restore_route_gate::refused_row(
                            name,
                            harness_name,
                            reason.clone(),
                        ));
                        continue;
                    }
                    None => {
                        rows.push(restore_route_gate::refused_row(
                            name,
                            harness_name,
                            "claude re-entry plan unresolved; resume it from the agent panel"
                                .into(),
                        ));
                        continue;
                    }
                }
            }
            let structural = member_structural_refusal(&member);
            // Pre-stage the sync render so the non-claude arm never
            // fires the off-loop resolution per bulk member: bulk restore
            // stays byte-identical to before.
            if harness_name.as_deref().is_some_and(|h| h != "claude") {
                self.staged_resume_argv = resume_argv_for(
                    harness_name.as_deref().unwrap_or(""),
                    member.harness_session_id.as_deref().unwrap_or(""),
                )
                .ok();
            }
            // The native session id rides the rebind job; `member`
            // moves into resume_one below.
            let rebind_session = (!dry_run)
                .then(|| member.harness_session_id.clone())
                .flatten();
            let outcome =
                self.resume_one(&name, Some(member), RESTORE_CLIENT, (0, 0), dims, dry_run);
            self.staged_resume_argv = None;
            self.reentry_verdict = None;
            let row = match outcome {
                ResumeOutcome::Resumed {
                    pane,
                    squad,
                    tab,
                    notice,
                    ..
                } => RestoreRow {
                    member: name,
                    harness: harness_name,
                    squad,
                    portal: None,
                    outcome: "resumed".into(),
                    pane: Some(pane),
                    tab: Some(tab),
                    reason: None,
                    notice,
                },
                ResumeOutcome::Focused { pane, squad, tab } => RestoreRow {
                    member: name,
                    harness: harness_name,
                    squad,
                    portal: None,
                    outcome: "focused".into(),
                    pane: Some(pane),
                    tab: Some(tab),
                    reason: None,
                    notice: None,
                },
                // The member's own structural gap outranks the generic
                // gesture notice in the REPORT: a no-form harness or a
                // `missing` session id is the specific reason AC5-ERR
                // demands. The gates themselves already ran.
                ResumeOutcome::Refused { reason } => restore_route_gate::refused_row(
                    name,
                    harness_name,
                    structural.unwrap_or(reason),
                ),
                ResumeOutcome::PlanPending => restore_route_gate::refused_row(
                    name,
                    harness_name,
                    "claude re-entry plan unresolved; resume it from the agent panel".into(),
                ),
                ResumeOutcome::Planned => RestoreRow {
                    member: name,
                    harness: harness_name,
                    squad: 0,
                    portal: None,
                    outcome: "planned".into(),
                    pane: None,
                    tab: None,
                    reason: None,
                    notice: None,
                },
            };
            // Collect the rebind job BEFORE the row consumes the
            // member: a resumed member whose row carried a native session id
            // and a child pid rebinds off-loop; the dry run never does.
            if !dry_run && row.outcome == "resumed" {
                if let (Some(harness), Some(sid)) = (
                    row.harness.as_deref(),
                    rebind_session.as_deref().filter(|s| !s.trim().is_empty()),
                ) {
                    if let Some(pane) = row.pane {
                        if let Some(pid) = self.panes.get(&pane).and_then(|p| p.pty.child_pid()) {
                            rebind_jobs.push(RebindJob {
                                member: row.member.clone(),
                                harness: harness.to_string(),
                                session: sid.to_string(),
                                mux_session: self.session_name.clone(),
                                pane,
                                pid,
                            });
                        }
                    }
                }
            }
            rows.push(row);
        }
        // The muse pass: registry muse thread rows placed in this session's
        // workspaces ride the host revive, not a pane spawn. The start phase
        // ran the shells off-loop and carried every verdict here, so a real
        // resumed/refused/focused verdict fills from `muse_results`; a dry
        // run reports planned; a verdict-less name refuses, naming the gap,
        // never reading as planned.
        if harness.as_deref().is_none_or(|h| h == "muse") {
            for agent in self.muse_restore_candidates(member_session.as_deref()) {
                let name = agent.name.clone();
                let squad = self.thread_workspace(&agent).unwrap_or(0);
                let reason = agent
                    .harness_session_id
                    .as_deref()
                    .and_then(|sid| crate::restore_gate::retired_refusal(Some(sid), &retired));
                if let Some(reason) = reason {
                    rows.push(restore_route_gate::refused_row(
                        name,
                        Some("muse".to_string()),
                        reason,
                    ));
                    continue;
                }
                let mut row = RestoreRow {
                    member: name.clone(),
                    harness: Some("muse".into()),
                    squad,
                    portal: None,
                    outcome: if dry_run {
                        "planned".into()
                    } else {
                        "refused".into()
                    },
                    pane: None,
                    tab: None,
                    reason: None,
                    notice: None,
                };
                if !dry_run {
                    match muse_results.get(&name) {
                        Some(Ok(MuseRevival::Revived)) => {
                            row.outcome = "resumed".into();
                            row.notice = Some("a fresh muse host serves the session".into());
                        }
                        Some(Ok(MuseRevival::AlreadyLive)) => {
                            row.outcome = "focused".into();
                            row.notice =
                                Some("the muse host held the session; nothing was launched".into());
                        }
                        Some(Err(why)) => {
                            row.reason = Some(why.clone());
                        }
                        None => {
                            row.reason = Some(
                                "the muse revive did not run in this restore; run it again".into(),
                            );
                        }
                    }
                }
                rows.push(row);
            }
        }
        if let Some(want) = member_session {
            if rows.is_empty() {
                rows.push(restore_route_gate::refused_row(
                    want,
                    harness,
                    "exact session is not a restorable workspace member".into(),
                ));
            }
        } else {
            rows.extend(portal_reach::portal_restore_rows(self, dry_run, &mut plans));
        }
        let resumed = rows.iter().filter(|r| r.outcome == "resumed").count();
        if resumed > 0 {
            self.push_layout(true);
        }
        if !dry_run {
            // Every refusal reaches the attached clients by name (AC5-ERR),
            // and a zero-inclusive summary answers "did restore do anything"
            // without reading a pane count.
            for row in rows.iter().filter(|r| r.outcome == "refused") {
                self.notice_all(format!(
                    "workspace restore: {} could not be resumed: {}",
                    row.member,
                    row.reason.as_deref().unwrap_or("no reason given"),
                ));
            }
            let focused = rows.iter().filter(|r| r.outcome == "focused").count();
            let refused = rows.iter().filter(|r| r.outcome == "refused").count();
            self.notice_all(format!(
                "workspace restore: {resumed} resumed, {focused} focused, {refused} refused"
            ));
        }
        if rebind_jobs.is_empty() || !rebind_runtime_ready() {
            let _ = reply.send(ServerMsg::WorkspaceRestored { rows });
            return;
        }
        // Off-loop rebind: each job shells `fno-agents pane-rebind`
        // bounded, and the reply waits for every receipt so the command's own
        // report carries it. The pane is running either way, so a failed
        // rebind degrades to its named notice on the row, never a refusal.
        tokio::spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for job in rebind_jobs {
                set.spawn(async move {
                    let notice = match agent_actions::run_pane_rebind(
                        &job.harness,
                        &job.session,
                        &job.mux_session,
                        job.pane,
                        job.pid,
                    )
                    .await
                    {
                        Ok(line) => line,
                        Err(reason) => format!("registry row rebind failed: {reason}"),
                    };
                    (job.member, notice)
                });
            }
            let mut notices: HashMap<String, String> = HashMap::new();
            while let Some(joined) = set.join_next().await {
                if let Ok((member, notice)) = joined {
                    notices.insert(member, notice);
                }
            }
            for row in rows.iter_mut() {
                if let Some(n) = notices.get(&row.member) {
                    // The codex resume form can land on its paused-goal
                    // prompt instead of a live turn; the receipt names it
                    // because the command cannot answer it from here.
                    let dialog = match row.harness.as_deref() {
                        Some("codex") => {
                            "; codex may show its resume prompt in the pane - answer it there"
                        }
                        _ => "",
                    };
                    row.notice = Some(match row.notice.take() {
                        Some(prev) => format!("{prev}; {n}{dialog}"),
                        None => format!("{n}{dialog}"),
                    });
                }
            }
            let _ = reply.send(ServerMsg::WorkspaceRestored { rows });
        });
    }
}
