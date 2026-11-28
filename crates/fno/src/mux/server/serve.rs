use super::mouse_route::{bounded_scroll_target, MOUSE_WHEEL_LINES};
use super::*;

pub(super) async fn serve(
    listener: std::os::unix::net::UnixListener,
    socket: &Path,
    session_name: String,
    pane_children: PaneChildRoster,
    signal_rx: mpsc::Receiver<CoreMsg>,
    shutdown_complete: Arc<AtomicBool>,
    owner: Option<OwnerLease>,
) -> i32 {
    let Some(listener) = bind_listener(listener) else {
        return 1;
    };

    // One shared pane-tagged output channel + one exit channel for all PTY
    // reader threads. Squads (and their first panes) are born from attaches;
    // nothing is spawned upfront.
    let (out_tx, out_rx) = mpsc::channel::<(u64, PaneChunk)>(256);
    let (exit_tx, exit_rx) = mpsc::channel::<u64>(64);
    let (core_tx, core_rx) = mpsc::channel::<CoreMsg>(256);
    // Attached-client count for the periodic readers: Core owns the
    // sender; each reader holds a receiver as its work gate + 0->1 wakeup.
    let (client_count_tx, client_count_rx) = watch::channel(0usize);
    let persisted_pane_floor = crate::squad_store::load().next_pane_id;
    let initial_agents = read_guard_agents().await;
    let next_pane_id = pane_id_floor(
        persisted_pane_floor,
        initial_agents.as_deref().unwrap_or(&[]),
    );
    let mut core = new_core(
        session_name,
        pane_children,
        out_tx,
        exit_tx,
        core_tx.clone(),
        client_count_tx,
        next_pane_id,
    );

    spawn_registry_reader(core_tx.clone(), client_count_rx.clone());
    tokio::task::spawn_blocking(pasted_images::sweep_dead_sessions);

    crate::board_reader::spawn(core_tx.clone(), client_count_rx.clone());

    spawn_pane_stats_tick(core_tx.clone());

    // The squad-key cache, shared by the per-connection handshake tasks. The
    // blocking git resolution runs there (spawn_blocking), NEVER on the core
    // loop - a hung git may delay ONE attach by the 2s timeout, but every
    // pane and peer keeps streaming (the drive-freeze class).
    let resolver = Arc::new(Mutex::new(Resolver::default()));

    // Accept loop: handshake each connection off the core loop's back.
    let conns_alive = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    spawn_accept_loop(
        listener,
        core_tx.clone(),
        resolver,
        core.pane_stats.clone(),
        conns_alive.clone(),
    );

    crate::mux_log!(info; "fno mux: serving {}", socket.display());

    // Re-adopt surviving keeper panes BEFORE any attach can restore: this
    // runs synchronously before the loop drains its first CoreMsg, so an
    // early attach's restore sees adopted panes as already-live members.
    core.keeper_readopt();

    let idle = IdleExit::new(client_count_rx.clone(), owner.is_some());
    let mut core_loop = CoreLoop {
        core,
        out_rx,
        exit_rx,
        core_rx,
        signal_rx,
        idle,
        ticks: LoopTicks::start().await,
        // Build-drift retirement: the watch stats its own executable
        // off-loop every 5th tick and retires through Flow::Shutdown only on a
        // drifted verdict at a fully quiet tick. The machinery lives in
        // server/drift_retire.rs.
        drift_watch: drift_retire::RetireWatch::new(),
        // diagnostics: which panes' output the CORE LOOP has seen. Pairs
        // with the pty reader thread's own first-chunk line to split "shell never
        // spoke" from "core loop never drained it".
        e2e_first_out: HashSet::new(),
        // Explicit e2e-only fault seam: after a real pane registration, park the
        // core-loop thread forever so SIGTERM cannot be handled by this loop.
        core_wedge_e2e: std::env::var_os("FNO_E2E_CORE_WEDGE").is_some(),
        core_wedge_armed: false,
        owner,
        conns_alive,
    };

    let (flow, cause) = core_loop.run().await;
    let core = &mut core_loop.core;
    if flow == Flow::Shutdown {
        crate::mux_log!(info;
            "fno mux: shutting down ({cause}, {} panes)",
            core.panes.len()
        );
        // Capture only from a safe restore state and current store generation.
        core.record_exit(cause);
        core.kill_all_panes();
        core.bye_all("session ended");
        // Give writer tasks a beat to flush the Byes; a lost Bye reads as
        // "session ended (server closed)" client-side, so this is best-effort.
        tokio::time::sleep(BYE_FLUSH).await;
    }
    shutdown_complete.store(true, Ordering::Release);
    0
}

/// The std listener made non-blocking and handed to tokio; `None` (logged)
/// when either step fails.
fn bind_listener(listener: std::os::unix::net::UnixListener) -> Option<tokio::net::UnixListener> {
    if let Err(e) = listener.set_nonblocking(true) {
        crate::mux_log!(error; "fno mux: listener setup failed: {e}");
        return None;
    }
    match tokio::net::UnixListener::from_std(listener) {
        Ok(l) => Some(l),
        Err(e) => {
            crate::mux_log!(error; "fno mux: listener setup failed: {e}");
            None
        }
    }
}

/// The core's starting state: the channels it owns, the pane-id floor, and
/// every per-session map empty.
fn new_core(
    session_name: String,
    pane_children: PaneChildRoster,
    out_tx: mpsc::Sender<(u64, PaneChunk)>,
    exit_tx: mpsc::Sender<u64>,
    self_tx: mpsc::Sender<CoreMsg>,
    client_count_tx: watch::Sender<usize>,
    next_pane_id: u64,
) -> Core {
    Core {
        session: Session::default(),
        panes: HashMap::new(),
        pane_watch: HashMap::new(),
        pane_stats: Arc::new(RwLock::new(HashMap::new())),
        pane_stats_emit_failures: Arc::new(AtomicU64::new(0)),
        pane_children,
        clients: Vec::new(),
        next_pane_id,
        next_squad_id: 1,
        sizing: TabSizing::from_env(),
        session_name,
        shells: shell_candidates(std::env::var_os("SHELL").as_deref()),
        out_tx,
        exit_tx,
        self_tx,
        input_echo: HashMap::new(),
        anchors: HashMap::new(),
        agents: Vec::new(),
        agents_read_ok: false,
        journal: crate::spawn_journal::JournalCache::load(),
        launch_desk: Default::default(),
        branch_by_cwd: HashMap::new(),
        project_by_cwd: HashMap::new(),
        thread_pins: thread_pin::pins_from_store(&crate::squad_store::load().squads),
        tail_by_session: HashMap::new(),
        ctx_by_session: HashMap::new(),
        truth_by_name: HashMap::new(),
        truth_seq: 0,
        backlog: Vec::new(),
        backlog_lanes: Vec::new(),
        backlog_stale: false,
        backlog_holders: HashMap::new(),
        observers: Default::default(),
        links: Default::default(),
        backlog_join: crate::backlog_view::BacklogJoin::default(),
        claim_eligible: HashSet::new(),
        claims: HashMap::new(),
        composer: composer_guard::ComposerGuard::default(),
        touch_last_emit: HashMap::new(),
        wheel_gate: HashMap::new(),
        wheel_lines: crate::digest_overlay::wheel_lines(
            &std::env::current_dir().unwrap_or_default(),
        ),
        wheel_passthrough_rate: crate::digest_overlay::wheel_passthrough_rate(
            &std::env::current_dir().unwrap_or_default(),
        ),
        click_claims: HashMap::new(),
        touch_emit_failures: Arc::new(AtomicU64::new(0)),
        started_at: crate::server_stats::stamp_now(),
        client_count: client_count_tx,
        seen: HashSet::new(),
        attached: HashMap::new(),
        worker_pane: HashMap::new(),
        worker_session_pane: HashMap::new(),
        held_workers: HashMap::new(),
        detached_panes: HashMap::new(),
        diff_pane: None,
        recall: crate::server::focus_recall::FocusRecall::default(),
        portals: BTreeMap::new(),
        portal_noticed: false,
        squad_members: HashMap::new(),
        template_specs: HashMap::new(),
        pending_template_restores: Vec::new(),
        external_lifecycle: Vec::new(),
        persist_degraded_notified: false,
        shared_identity_notified: HashSet::new(),
        restored: false,
        restore_pending: false,
        spawn_flight: Default::default(),
        restore_hold: Default::default(),
        store_generations: HashMap::new(),
        pre_restore_squads: HashSet::new(),
        topology_dirty: false,
        last_topology_flush: None,
        reentry_verdict: None,
        staged_resume_argv: None,
        batch_plans: HashMap::new(),
        pending_thread_reply: None,
        keeper_adopted: Vec::new(),
        shell_rc_dirs: HashMap::new(),
        portal_session_guards: BTreeMap::new(),
    }
}

// The off-loop registry reader (4a-G2): a 1s interval task stats/reads
// BOTH the fno-agent registry AND claude's daemon roster on the
// blocking pool, unions them into the agent row set (TTL aging + roster
// liveness upgrade + foreign rows included), and sends it to the core only
// when the MERGED set changed. Each file is behind its own mtime+len gate,
// so a roster-only change publishes and an idle tick reads nothing. The
// render path never touches either file (AC2-UI; the origin freeze class),
// and staleness stays bounded by this one interval.
fn spawn_registry_reader(core_tx: mpsc::Sender<CoreMsg>, count_rx: watch::Receiver<usize>) {
    let reg_path = agents_view::registry_path();
    let roster_path = agents_view::roster_path();
    tokio::spawn(async move {
        let reader = RegistryReader {
            core_tx,
            reg_path,
            roster_path,
            state: agents_view::ReaderState::default(),
            // Carried across ticks so the tail pass can run even when
            // the row set did not move: the uuid set to look up, and the last
            // map pushed, so an unchanged result stays off the wire.
            last_uuids: Vec::new(),
            last_tails: HashMap::new(),
            last_ctx: HashMap::new(),
            // Shared so the path cache survives across blocking-pool passes.
            tail_reader: std::sync::Arc::new(std::sync::Mutex::new(
                crate::transcript_tail::TailReader::new(),
            )),
            // cwd -> project, resolved once per cwd per server lifetime (git
            // runs on the blocking pool, like `squad::Resolver`).
            project_memo: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            last_truth: Instant::now(),
            // Shared with the detached probe tasks: clear means no probe is
            // in flight. Held by [`TruthProbeLatch`], which clears it on drop.
            truth_in_flight: Arc::new(AtomicBool::new(false)),
            // Logged once per outage, never per tick: the fallback is a
            // fact about the environment, and a fleet with no daemon must
            // not pay one line per second for it. The recovery clears it.
            fallback_logged: false,
            // Same once-only discipline for the wedged-probe skip.
            latch_wedge_logged: false,
            // (v48) Launch order for AgentTruth probes, so an out-of-order
            // completion cannot clobber a fresher result (see CoreMsg::AgentTruth).
            truth_probe_seq: 0,
        };
        // The models.dev catalog refresh: once at start, then hourly (a
        // stat per hour when fresh). Pricing never depends on a user who
        // never opens the composer; a failed fetch keeps the old cache
        // and the next hour retries. The refresh itself re-stats and
        // spawns only on a stale cache, so the steady state is one stat.
        let mut catalog_every = tokio::time::interval(Duration::from_secs(3600));
        catalog_every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        reader.run(count_rx, tick, catalog_every).await;
    });
}

// Stat + conditional read of one file behind an mtime+len gate,
// both off the core loop. Returns (fresh stamp, raw-if-changed).
async fn scan(
    path: std::path::PathBuf,
    cached: Option<(std::time::SystemTime, u64)>,
) -> (Option<(std::time::SystemTime, u64)>, Option<String>) {
    let stat_path = path.clone();
    let (stamp, table) = tokio::task::spawn_blocking(move || {
        let stamp = std::fs::metadata(&stat_path)
            .ok()
            .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
        (stamp, crate::registry_read::table_owns(&stat_path))
    })
    .await
    .unwrap_or((None, false));
    // An imported registry's rows live in graph.db, where no file
    // stat moves, so it is read every tick.
    let raw = if stamp != cached || table {
        tokio::task::spawn_blocking(move || crate::registry_read::registry_text(&path).ok())
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    (stamp, raw)
}

/// The off-loop registry reader's state, carried across its 1s ticks.
struct RegistryReader {
    core_tx: mpsc::Sender<CoreMsg>,
    reg_path: std::path::PathBuf,
    roster_path: std::path::PathBuf,
    state: agents_view::ReaderState,
    last_uuids: Vec<(String, Option<String>)>,
    last_tails: HashMap<String, String>,
    last_ctx: HashMap<String, String>,
    tail_reader: Arc<Mutex<crate::transcript_tail::TailReader>>,
    project_memo: Arc<Mutex<HashMap<String, agent_rows_apply::ProjectRef>>>,
    last_truth: Instant,
    truth_in_flight: Arc<AtomicBool>,
    fallback_logged: bool,
    latch_wedge_logged: bool,
    truth_probe_seq: u64,
}

impl RegistryReader {
    async fn run(
        mut self,
        mut count_rx: watch::Receiver<usize>,
        mut tick: tokio::time::Interval,
        mut catalog_every: tokio::time::Interval,
    ) {
        loop {
            // Gate the registry+roster read on an attached client.
            // The `changed()` arm IS the 0->1 kick: an attach wakes the
            // parked reader at once so the first overlay is fresh (AC3-FR).
            tokio::select! {
                _ = tick.tick() => {}
                _ = catalog_every.tick() => {
                    crate::model_catalog::refresh_if_stale(&crate::model_catalog::state_dir());
                }
                res = count_rx.changed() => {
                    if res.is_err() {
                        return; // Core dropped; server shutting down
                    }
                }
            }
            if *count_rx.borrow() == 0 {
                continue; // no viewer -> skip both file reads entirely
            }
            self.probe_truth_if_due();
            let (reg_stamp, reg_raw) = self.read_registry().await;
            let (roster_stamp, roster_raw) =
                scan(self.roster_path.clone(), self.state.roster_stamp()).await;
            let isolated = self.read_isolated().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let changed = self.state.tick(
                reg_stamp,
                move || reg_raw,
                roster_stamp,
                move || roster_raw,
                isolated,
                now,
            );
            if let Some(rows) = &changed {
                self.last_uuids = transcript_keys(rows);
            }
            // Tails resolve on EVERY tick (a transcript grows
            // with no registry change); TailReader re-reads known paths
            // each tick and paces only the discovery walk, which at 1Hz
            // was most of this loop's CPU.
            let uuids = self.last_uuids.clone();
            let reader = self.tail_reader.clone();
            let (tails, ctx) = tokio::task::spawn_blocking(move || {
                // Poison-recover rather than expect: a panic in one pass
                // must not blank the column on every later tick (the
                // cache is a HashMap, safe to reuse mid-poison).
                reader
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .tails_and_ctx(&uuids)
            })
            .await
            .unwrap_or_default();
            if !self.publish(changed, tails, ctx).await {
                return;
            }
        }
    }

    // (v48) Reachability evidence, one `agent.list` RPC for the
    // whole fleet on a slow sub-interval (the
    // `fno agent list` interpreter it replaced was the mux
    // server's largest child, 5-32s of Python every 60s). Each
    // probe runs as its own task so a slow daemon answer never
    // stalls the 1s registry tick; a failed probe sends nothing
    // and the last good map stands. The latch skips a tick whose
    // predecessor still runs, so a probe slower than the interval
    // stacks no second request. Ages are measured at probe
    // time, so between probes a row's displayed age lags by at
    // most this interval - invisible next to the 600s threshold
    // it feeds.
    fn probe_truth_if_due(&mut self) {
        if self.last_truth.elapsed() >= TRUTH_PROBE_EVERY {
            match TruthProbeLatch::begin(&self.truth_in_flight) {
                Some(latch) => {
                    self.last_truth = Instant::now();
                    self.truth_probe_seq += 1;
                    let seq = self.truth_probe_seq;
                    let tx = self.core_tx.clone();
                    tokio::spawn(async move {
                        let probe = probe_truth_map().await;
                        drop(latch);
                        if let Some(map) = probe {
                            let _ = tx.send(CoreMsg::AgentTruth { map, seq }).await;
                        }
                    });
                }
                None => {
                    // A probe still running a full interval after it
                    // started is wedged, not slow. The old code healed
                    // that by stacking a new probe; the latch cannot,
                    // so say so once instead of silently freezing
                    // every row's age at the last good reading.
                    if !self.latch_wedge_logged {
                        self.latch_wedge_logged = true;
                        crate::mux_log!(warn;
                            "fno mux: a fleet truth probe has run over {TRUTH_PROBE_EVERY:?}; \
                             skipping probes until it exits"
                        );
                    }
                }
            }
        }
    }

    // The registry leg subscribes to the daemon
    // when its socket answers (AC12): rows are SERVED, the stamp
    // domain is the same (mtime, len) the file scan gated with, and
    // an unchanged answer costs one stat server-side and one small
    // frame here - no file read on either side. When nothing
    // answers, the file scan below stands (AC13, the supported
    // no-daemon shape), logged once.
    async fn read_registry(&mut self) -> (Option<(std::time::SystemTime, u64)>, Option<String>) {
        match agents_view::watch_registry(
            self.state.reg_stamp(),
            &agents_view::supervisor_sock_path(),
        )
        .await
        {
            // Every tick retries the daemon, so a miss at spawn (a
            // daemon restarting beside the mux, a 2s round under
            // load) heals by itself. Log both edges, or the log
            // reads degraded for the server's whole life.
            Ok(answer) => {
                if self.fallback_logged {
                    self.fallback_logged = false;
                    crate::mux_log!(info; "fno mux: agent daemon answering again; rows are served");
                }
                answer
            }
            Err(error) => {
                if !self.fallback_logged {
                    self.fallback_logged = true;
                    crate::mux_log!(warn;
                        "fno mux: no agent daemon answering at {} ({error}); reading the registry file directly (degraded fallback, retried every tick)",
                        agents_view::supervisor_sock_path().display()
                    );
                    // Ruling d-e096c669: the mux starts fno's own
                    // pieces; a vendor tab is never what unblocks it.
                    // `fno-agent status` lazy-starts the daemon
                    // (client::call runs ensure_daemon). Once per
                    // fallback edge, off the loop, bounded, output
                    // dropped.
                    start_agent_daemon();
                }
                scan(self.reg_path.clone(), self.state.reg_stamp()).await
            }
        }
    }

    // Each registered isolated account's roster.json, folded
    // into the union tagged by account (managed accounts share
    // ~/.claude and add no dir). The config re-read is tiny and
    // gated on an attached viewer; each roster read is stamp-gated
    // per dir by `isolated_stamp`, so only a changed dir re-reads.
    async fn read_isolated(&self) -> Vec<agents_view::IsolatedRead> {
        let iso_paths = tokio::task::spawn_blocking(agents_view::isolated_roster_paths)
            .await
            .unwrap_or_default();
        let mut isolated = Vec::with_capacity(iso_paths.len());
        for (account, path) in iso_paths {
            let (stamp, raw) = scan(path, self.state.isolated_stamp(&account)).await;
            isolated.push(agents_view::IsolatedRead {
                account,
                stamp,
                raw,
            });
        }
        isolated
    }

    /// Pushes a changed row set (after its projects), or moved tails alone;
    /// false once the core loop is gone.
    async fn publish(
        &mut self,
        changed: Option<Vec<RegistryAgent>>,
        tails: HashMap<String, String>,
        ctx: HashMap<String, String>,
    ) -> bool {
        if let Some(rows) = changed {
            // (US4) Resolve the git branch per UNIQUE row cwd,
            // off the core loop, on the blocking pool - bounded file
            // reads only, per-cwd degradation on failure (AC1-FR). This
            // rides the change-gated emit: a branch only moves when the
            // row set does, so the reads stay off idle ticks.
            let cwds: Vec<String> = {
                let mut seen = std::collections::HashSet::new();
                rows.iter()
                    .map(|r| r.cwd.clone())
                    .filter(|c| !c.is_empty() && seen.insert(c.clone()))
                    .collect()
            };
            let projects = resolve_projects(self.project_memo.clone(), cwds.clone()).await;
            let branches = tokio::task::spawn_blocking(move || {
                cwds.into_iter()
                    .filter_map(|c| {
                        agents_view::resolve_branch(std::path::Path::new(&c)).map(|b| (c, b))
                    })
                    .collect::<HashMap<String, String>>()
            })
            .await
            .unwrap_or_default();
            self.last_tails = tails.clone();
            self.last_ctx = ctx.clone();
            if self
                .core_tx
                .send(CoreMsg::ProjectRoots(projects))
                .await
                .is_err()
            {
                return false; // core loop gone; the server is shutting down
            }
            if self
                .core_tx
                .send(CoreMsg::AgentRows {
                    rows,
                    branches,
                    tails,
                    ctx,
                    read_ok: self.state.read_ok(),
                })
                .await
                .is_err()
            {
                return false; // core loop gone; the server is shutting down
            }
        } else if tails != self.last_tails || ctx != self.last_ctx {
            // Rows unchanged but somebody said something: push the tails
            // alone rather than forcing a whole row set through.
            self.last_tails = tails.clone();
            self.last_ctx = ctx.clone();
            if self
                .core_tx
                .send(CoreMsg::AgentTails { tails, ctx })
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }
}

/// Lazy-starts the agent daemon off the loop, bounded, output dropped.
fn start_agent_daemon() {
    let bin = crate::digest_overlay::fno_agent_bin();
    tokio::spawn(async move {
        let mut command = crate::process_admission::tokio_command(&bin);
        command.args(["status"]).stdin(std::process::Stdio::null());
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::process_admission::tokio_output(&mut command),
        )
        .await
        {
            Ok(Ok(_)) => {
                crate::mux_log!(info; "fno mux: started the agent daemon")
            }
            Ok(Err(e)) => {
                crate::mux_log!(error; "fno mux: agent daemon start failed: {e}")
            }
            Err(_) => crate::mux_log!(error;
                "fno mux: agent daemon start failed: timed out after 10s"
            ),
        }
    });
}

// The tail key is the row's transcript identity: the claude
// uuid where one exists, else the harness session id (the
// rollout uuid a codex row carries). log_path rides beside
// it - a row naming its own transcript is read directly.
fn transcript_keys(rows: &[RegistryAgent]) -> Vec<(String, Option<String>)> {
    rows.iter()
        .filter_map(|r| {
            let uuid = r
                .claude_session_uuid
                .clone()
                .or_else(|| r.harness_session_id.clone())?;
            Some((uuid, r.log_path.clone()))
        })
        .collect()
}

/// The project of each cwd, memoized per server lifetime; git runs on the
/// blocking pool.
async fn resolve_projects(
    memo: Arc<Mutex<HashMap<String, agent_rows_apply::ProjectRef>>>,
    project_cwds: Vec<String>,
) -> HashMap<String, agent_rows_apply::ProjectRef> {
    tokio::task::spawn_blocking(move || {
        let mut out = HashMap::new();
        for cwd in project_cwds {
            let hit = memo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cwd)
                .cloned();
            let project = hit.unwrap_or_else(|| {
                let fresh = agent_rows_apply::resolve_project(&cwd);
                memo.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(cwd.clone(), fresh.clone());
                fresh
            });
            out.insert(cwd, project);
        }
        out
    })
    .await
    .unwrap_or_default()
}

// The per-pane counter cadence: a fixed 30s tick telling the core to
// snapshot and emit. Delay (not Burst) on a missed tick - counters are
// monotonic totals, so one late sample costs nothing a difference needs.
fn spawn_pane_stats_tick(core_tx: mpsc::Sender<CoreMsg>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PANE_STATS_CADENCE);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if core_tx.send(CoreMsg::PaneStatsTick).await.is_err() {
                return; // Core dropped; server shutting down
            }
        }
    });
}

/// Spawns the accept loop; each connection's handshake runs in its own task.
fn spawn_accept_loop(
    listener: tokio::net::UnixListener,
    accept_core_tx: mpsc::Sender<CoreMsg>,
    resolver: Arc<Mutex<Resolver>>,
    accept_stats: PaneStats,
    accept_conns: Arc<std::sync::atomic::AtomicUsize>,
) {
    tokio::spawn(async move {
        let mut next_id: u64 = 1;
        // Descriptor starvation is a STATE, not an event stream: while it
        // holds, every accept fails, and a 100ms retry would log ten lines
        // a second for as long as the famine holds. Log the two edges once,
        // retry at 1s inside the state, and stay silent between; other
        // accept errors keep today's 100ms + line.
        let mut starved = false;
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if starved {
                        starved = false;
                        crate::mux_log!(info; "fno mux: accept recovered after fd exhaustion");
                    }
                    let id = next_id;
                    next_id += 1;
                    // Peer pid names WHICH client process this is (the e2e
                    // harness logs its children's pids for the join).
                    e2e_log(format_args!(
                        "conn {id} accepted (peer pid {:?})",
                        stream.peer_cred().ok().and_then(|c| c.pid())
                    ));
                    let conn_core_tx = accept_core_tx.clone();
                    let conn_resolver = resolver.clone();
                    let conn_stats = accept_stats.clone();
                    // Count from the accept itself, not the task's first poll:
                    // a scheduler-starved newborn task would otherwise leave
                    // the reaper a mid-command window with conns_alive == 0.
                    let alive = ConnAlive::new(&accept_conns);
                    tokio::spawn(async move {
                        let _alive = alive;
                        handle_client(stream, conn_core_tx, conn_resolver, id, conn_stats).await;
                    });
                }
                Err(e) => {
                    // Inside the starvation state every error kind stays
                    // silent; edges log once and the sleep follows the state.
                    let famine = crate::process_admission::no_descriptor_left(&e);
                    if famine && !starved {
                        starved = true;
                        crate::mux_log!(warn; "fno mux: accept starved: {e}; retrying every second until a descriptor frees");
                    } else if !famine && !starved {
                        crate::mux_log!(error; "fno mux: accept failed: {e}");
                    }
                    tokio::time::sleep(Duration::from_millis(if famine || starved {
                        1000
                    } else {
                        100
                    }))
                    .await;
                }
            }
        }
    });
}

/// The idle reaper's clock and the attach facts that gate its re-arm.
struct IdleExit {
    e2e: bool,
    enabled: bool,
    grace: Duration,
    count_rx: watch::Receiver<usize>,
    deadline: tokio::time::Instant,
    ever_attached: bool,
}

impl IdleExit {
    fn new(count_rx: watch::Receiver<usize>, owner_bound: bool) -> Self {
        // Shared ownerless servers persist across client detach. E2E servers and
        // owner-bound sandbox servers use the same graceful idle shutdown path;
        // sandbox pane output cannot extend the deadline after the last client
        // leaves, while a script-style E2E session can still re-arm on output.
        let idle_exit_e2e = std::env::var_os("FNO_E2E").is_some();
        let idle_exit_enabled = idle_exit_e2e || owner_bound;
        let idle_grace = Duration::from_millis(
            std::env::var("FNO_IDLE_EXIT_GRACE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000),
        );
        Self {
            e2e: idle_exit_e2e,
            enabled: idle_exit_enabled,
            grace: idle_grace,
            count_rx,
            deadline: tokio::time::Instant::now() + idle_grace,
            ever_attached: false,
        }
    }

    fn rearm_on_output(&mut self) {
        if self.enabled && (self.e2e || !self.ever_attached || *self.count_rx.borrow() > 0) {
            self.rearm();
        }
    }

    fn rearm(&mut self) {
        self.deadline = tokio::time::Instant::now() + self.grace;
    }
}

/// The core loop's fixed-cadence timers.
struct LoopTicks {
    pane_reap_tick: tokio::time::Interval,
    owner_check_tick: tokio::time::Interval,
    sidecar_tick: tokio::time::Interval,
}

impl LoopTicks {
    async fn start() -> Self {
        let mut pane_reap_tick = tokio::time::interval(Duration::from_secs(1));
        pane_reap_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut owner_check_tick = tokio::time::interval(Duration::from_secs(1));
        owner_check_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The fleet sidecar refresh: only a client render carried the publish,
        // so a machine with no attached client stopped writing and the beat read
        // the file stale after five minutes. The timer keeps an unattended
        // machine publishing; the throttle inside the publish coalesces this
        // with the client-render writes.
        let mut sidecar_tick = tokio::time::interval(crate::workspace_sidecar::every());
        sidecar_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval`'s first tick is immediate; consume it so the first scan lands
        // after one interval instead of duplicating startup's known-live state.
        // For the sidecar the same consumption also skips a boot-time write from
        // a core whose squads have not synced yet (an empty map would clobber a
        // still-fresh file from the previous server run).
        pane_reap_tick.tick().await;
        sidecar_tick.tick().await;
        Self {
            pane_reap_tick,
            owner_check_tick,
            sidecar_tick,
        }
    }
}

/// The core loop's state: the core, its receivers, and the clocks and e2e
/// seams the select arms read.
struct CoreLoop {
    core: Core,
    out_rx: mpsc::Receiver<(u64, PaneChunk)>,
    exit_rx: mpsc::Receiver<u64>,
    core_rx: mpsc::Receiver<CoreMsg>,
    signal_rx: mpsc::Receiver<CoreMsg>,
    idle: IdleExit,
    ticks: LoopTicks,
    drift_watch: drift_retire::RetireWatch,
    e2e_first_out: HashSet<u64>,
    core_wedge_e2e: bool,
    core_wedge_armed: bool,
    owner: Option<OwnerLease>,
    conns_alive: Arc<std::sync::atomic::AtomicUsize>,
}

impl CoreLoop {
    async fn run(&mut self) -> (Flow, &'static str) {
        // The word this server died of, set by every Flow::Shutdown break before
        // breaking. Unassigned on purpose: a future break without a cause fails
        // to compile, so the log and the events feed can never disagree. The
        // tail prints it once and record_exit carries it into the event.
        let cause;

        let flow = loop {
            let sync_deadline = pane_output::next_sync_deadline(&self.core);
            tokio::select! {
                _ = async { tokio::time::sleep_until(sync_deadline.unwrap().into()).await }, if sync_deadline.is_some() => {
                    pane_output::expire_sync_updates(&mut self.core, Instant::now());
                }
                chunk = self.out_rx.recv() => {
                    // out_tx lives in Core, so recv never yields None.
                    let Some((pid, item)) = chunk else { cause = "channel-closed"; break Flow::Shutdown };
                    self.on_output(pid, item);
                }
                exited = self.exit_rx.recv() => {
                    let Some(pid) = exited else { cause = "channel-closed"; break Flow::Shutdown };
                    if let Some(died) = self.on_child_exit(pid) {
                            cause = died;
                            break Flow::Shutdown;
                        }
                    }
                _ = self.ticks.pane_reap_tick.tick() => {
                    if let Some(died) = self.on_reap_tick() {
                            cause = died;
                            break Flow::Shutdown;
                        }
                    }
                msg = self.core_rx.recv() => {
                    // core_tx lives in the accept loop, so recv never yields None.
                    let Some(msg) = msg else { cause = "channel-closed"; break Flow::Shutdown };
                    if self.on_core_msg(msg) == Flow::Shutdown {
                        cause = "kill";
                        break Flow::Shutdown;
                    }
                }
                signal = self.signal_rx.recv() => {
                    let Some(signal) = signal else { cause = "channel-closed"; break Flow::Shutdown };
                    if self.core.handle(signal) == Flow::Shutdown {
                        cause = "signal";
                        break Flow::Shutdown;
                    }
                }
                // Attach and final-detach edges both start a bounded owner-sandbox
                // idle window. Shared ownerless servers leave this branch disabled.
                res = self.idle.count_rx.changed(), if self.idle.enabled => {
                    if res.is_ok() {
                        self.idle.ever_attached |= *self.idle.count_rx.borrow() > 0;
                        self.idle.rearm();
                    }
                }
                _ = self.ticks.owner_check_tick.tick(), if self.owner.is_some() => {
                    if self.owner.as_ref().is_some_and(|lease| !lease.alive()) {
                        let session = self.owner.as_ref().map(|lease| lease.session.as_str()).unwrap_or("unknown");
                        crate::mux_log!(info; "fno mux: sandbox owner {session} is gone; shutting down");
                        cause = "owner-gone";
                        break Flow::Shutdown;
                    }
                }
                // The sidecar publish rides `agent_rows`; with no client no
                // render runs, so this timer is the only writer left on an
                // unattended machine.
                _ = self.ticks.sidecar_tick.tick() => {
                    self.core.agent_rows();
                }
                // On grace with no client, shut down owner-bound sandboxes and
                // explicit test servers through Flow::Shutdown. Replicate the
                // CoreMsg::Kill teardown (kill every pane PTY + Flow::Shutdown so
                // SocketGuard unlinks); NEVER std::process::exit, which would
                // orphan the pane shells and leak the socket file.
                _ = tokio::time::sleep_until(self.idle.deadline), if self.idle.enabled => {
                    if let Some(died) = self.on_idle_deadline() {
                            cause = died;
                            break Flow::Shutdown;
                        }
                    }
            }
            // Loop-tail choke point: the out_rx/exit_rx arms mutate
            // `clients` via the dead-client sweeps (broadcast_pane /
            // sync_focused_modes / close_pane) without a `handle()` call, so the
            // handle-tail publish alone would leave the count stale on exactly
            // the orphan path the readers gate on.
            self.core.publish_client_count();
            self.idle.ever_attached |= !self.core.clients.is_empty();
        };
        (flow, cause)
    }

    fn on_output(&mut self, pid: u64, item: PaneChunk) {
        drain_pty_output(
            &mut self.core,
            &mut self.out_rx,
            Some((pid, item)),
            &mut self.e2e_first_out,
        );
        // Pane output is a liveness signal: re-arm the idle
        // reaper so a working client-less script session never reaps.
        self.idle.rearm_on_output();
    }

    fn on_child_exit(&mut self, pid: u64) -> Option<&'static str> {
        e2e_log(format_args!("pane {pid} child exited"));
        // The reader sends this only after enqueuing every output
        // chunk. Drain that channel before removing the pane so final
        // bytes cannot lose a select race between separate receivers.
        if drain_pty_output(
            &mut self.core,
            &mut self.out_rx,
            None,
            &mut self.e2e_first_out,
        ) {
            self.idle.rearm_on_output();
        }
        // A worker that died on its own (churn) tombstones its member
        // BEFORE the reap clears the mapping (AC4-EDGE).
        let ctx = self.core.member_ctx(pid);
        self.core.reconcile_member_close(ctx, true);
        if self.core.close_viewer_died(pid, "viewer exited") == Flow::Shutdown {
            return Some("last-pane-gone");
        }
        None
    }

    fn on_reap_tick(&mut self) -> Option<&'static str> {
        // Deferred repaint requests ride the same 1s pass.
        self.core.poll_input_echo();
        self.core.flush_due_composer_buffers();
        self.core.fire_due_nudges();
        self.core.reconcile_grid_sizes();
        self.core.refresh_portal_viewer_labels();
        // Snapshot first: reader completion guarantees all output for
        // these panes was enqueued before this point. Drain it, then
        // close exactly the snapshot even if another reader finishes
        // concurrently.
        let dead = self.core.dead_children_ready_to_reap();
        if !dead.is_empty()
            && drain_pty_output(
                &mut self.core,
                &mut self.out_rx,
                None,
                &mut self.e2e_first_out,
            )
        {
            self.idle.rearm_on_output();
        }
        if self.core.reap_dead_children(dead) == Flow::Shutdown {
            return Some("last-dead-pane-reaped");
        }
        // Drift retirement: a drifted verdict at a fully
        // quiet tick (no panes, clients, or connections) retires the
        // server so the next attach spawns the installed build. The
        // stat runs off-loop in server/drift_retire.rs.
        let core = &self.core;
        let conns_alive = &self.conns_alive;
        if let Some((running, on_disk)) = self.drift_watch.tick(|| {
            core.panes.is_empty()
                && *core.client_count.borrow() == 0
                && conns_alive.load(Ordering::Acquire) == 0
        }) {
            crate::mux_log!(warn;
                "fno mux: stale-build retire: on-disk binary changed ({} -> {}); \
                 no panes, clients, or connections; retiring so the next \
                 attach spawns the installed build",
                running.path.display(),
                on_disk.path.display()
            );
            return Some("stale-build");
        }
        None
    }

    fn on_core_msg(&mut self, msg: CoreMsg) -> Flow {
        // Coalesce resize storms PER CLIENT: only each client's
        // final geometry hits its viewed tab's clamp (AC1-FR). Other
        // messages drained here run after, in arrival order.
        let flow = if let CoreMsg::Resize { id, rows, cols } = msg {
            self.coalesce_resizes(id, rows, cols)
        } else if let CoreMsg::Mouse { id, pane, event } = msg {
            // Wheel-scroll coalescing (mirrors the resize-storm coalescer
            // above): fold a contiguous run of interpreted wheel ticks on
            // one pane into ONE broadcast. Each tick is applied IN ORDER so
            // vt.scroll's per-tick clamp is preserved (algebraic netting
            // would cancel a clamped tick and lose a reversal at a
            // boundary); only the intermediate frames are skipped, so a
            // reversal queued behind in-flight opposite ticks lands in one
            // frame instead of rubber-banding through every offset. A
            // non-scroll event (passthrough/select) or passive sender stops
            // the fold, so ordering and read-only gating stay unchanged.
            self.coalesce_wheel(id, pane, event)
        } else {
            self.core.handle(msg)
        };
        if flow == Flow::Shutdown {
            return flow;
        }
        if self.core_wedge_e2e && !self.core_wedge_armed && !self.core.panes.is_empty() {
            self.core_wedge_armed = true;
            e2e_log(format_args!("core wedge armed"));
            std::thread::park();
        }
        Flow::Continue
    }

    fn coalesce_resizes(&mut self, id: u64, rows: u16, cols: u16) -> Flow {
        let mut last: HashMap<u64, (u16, u16)> = HashMap::new();
        last.insert(id, (rows, cols));
        let mut order = vec![id];
        let mut pending = Vec::new();
        while let Ok(m) = self.core_rx.try_recv() {
            match m {
                CoreMsg::Resize { id, rows, cols } => {
                    if last.insert(id, (rows, cols)).is_none() {
                        order.push(id);
                    }
                }
                other => pending.push(other),
            }
        }
        let mut flow = Flow::Continue;
        for id in order {
            let (rows, cols) = last[&id];
            flow = self.core.handle(CoreMsg::Resize { id, rows, cols });
            if flow == Flow::Shutdown {
                break;
            }
        }
        for m in pending {
            if flow == Flow::Shutdown {
                break;
            }
            flow = self.core.handle(m);
        }
        flow
    }

    fn coalesce_wheel(&mut self, id: u64, pane: u64, event: MouseEvent) -> Flow {
        if self.core.is_passive(id) {
            self.core.handle(CoreMsg::Mouse { id, pane, event })
        } else if let Some(d0) = self.core.scroll_delta(pane, &event) {
            let before = self.core.scroll_offset(pane) as i32;
            self.core.scroll_tick(pane, d0);
            let mut trailer = None;
            while let Ok(m) = self.core_rx.try_recv() {
                if let CoreMsg::Mouse {
                    id: mid,
                    pane: mpane,
                    event: mev,
                } = &m
                {
                    if *mpane == pane && !self.core.is_passive(*mid) {
                        if let Some(d) = self.core.scroll_delta(pane, mev) {
                            self.core.scroll_tick(pane, d);
                            continue;
                        }
                    }
                }
                trailer = Some(m);
                break;
            }
            // Cap the fold's net move to one viewport: a fast trackpad
            // flick drops many ticks in a single drain and would
            // otherwise jump hundreds of lines at once ("too fast").
            // The in-order clamp above is intact; this only bounds the
            // aggregate, so a lone wheel notch (well under a screen)
            // passes through untouched.
            let after = self.core.scroll_offset(pane) as i32;
            let cap = (self.core.pane_rows(pane) as i32).max(MOUSE_WHEEL_LINES);
            let bounded = bounded_scroll_target(before, after, cap);
            if bounded != after {
                self.core.scroll_tick(pane, bounded - after);
            }
            if bounded != before {
                self.core.broadcast_pane(pane);
            }
            if let Some(m) = trailer {
                self.core.handle(m)
            } else {
                Flow::Continue
            }
        } else {
            self.core.handle(CoreMsg::Mouse { id, pane, event })
        }
    }

    fn on_idle_deadline(&mut self) -> Option<&'static str> {
        if *self.idle.count_rx.borrow() == 0
            && self.conns_alive.load(std::sync::atomic::Ordering::Acquire) == 0
        {
            crate::mux_log!(info; "fno mux: idle-exit: no client for grace window");
            return Some("idle-exit");
        }
        self.idle.rearm();
        None
    }
}
