use super::*;

const NOW: i64 = 1_800_000_000_000;

/// FNO_CONFIG is process-global: a test that points it at its own config
/// takes this lock, or a parallel config reader interleaves mid-test.
static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .expect("valid ms")
        .to_rfc3339()
}

fn row(kind: &str, ms: i64, data: Value) -> Value {
    json!({"ts": stamp(ms), "type": kind, "data": data})
}

fn entry_row(pr: u64, head: &str, ms: i64) -> Value {
    row(
        ENTRY_EVENT,
        ms,
        json!({"slug": "o/r", "pr": pr, "head": head, "head_ref": format!("b{pr}")}),
    )
}

fn entry(pr: u64, priority: u8, ts_ms: i64) -> Entry {
    Entry {
        pr,
        head: format!("h{pr}"),
        head_ref: format!("b{pr}"),
        repair: false,
        priority,
        ts_ms,
    }
}

fn idle_state() -> State {
    fold(&[], DEFAULT_MAX_BATCH)
}

fn batch(prs: &[u64], base: &str) -> Batch {
    Batch {
        id: "1".into(),
        prs: prs.to_vec(),
        sha: "batch-sha".into(),
        base: base.into(),
        repair: false,
        reran: false,
        since_ms: NOW,
    }
}

fn with_active(b: Batch) -> State {
    State {
        active: Some(b),
        ..idle_state()
    }
}

fn step(entries: &[Entry], state: &State, main: &str, main_red: bool, gate: &Gate) -> Step {
    let flakes = HashSet::from(["plugin-tree".to_string()]);
    decide(&View {
        entries,
        state,
        main,
        main_red,
        gate,
        flakes: &flakes,
        now_ms: NOW + 60_000,
    })
}

fn build(prs: &[u64]) -> Step {
    Step::Build { prs: prs.to_vec() }
}

#[test]
fn a_batch_builds_in_queue_order_with_a_repair_entry_first_and_alone() {
    let mut entries: Vec<Entry> = (1..=8).map(|pr| entry(pr, 2, pr as i64)).collect();
    entries[7].priority = 1;
    let state = idle_state();
    assert_eq!(
        step(&entries, &state, "m0", false, &Gate::Pending),
        build(&[8, 1, 2, 3, 4, 5])
    );
    // A red main builds nothing but the repair entry.
    assert_eq!(
        step(&entries, &state, "m0", true, &Gate::Pending),
        Step::Idle
    );
    let mut repair = entry(9, 3, 99);
    repair.repair = true;
    entries.push(repair);
    assert_eq!(
        step(&entries, &state, "m0", true, &Gate::Pending),
        build(&[9])
    );
    assert_eq!(
        step(&entries, &state, "m0", false, &Gate::Pending),
        build(&[9])
    );
    // A batch built before main turned red stands down, so the repair builds.
    let parked = with_active(batch(&[1, 2], "m0"));
    assert_eq!(
        step(&entries, &parked, "m0", true, &Gate::Green),
        Step::Rebuild { main_moved: false }
    );
}

/// The node's done line: two PRs, each green alone, whose combination fails
/// the gate. The queue lands exactly one and ejects the other.
#[test]
fn two_green_prs_whose_combination_fails_land_only_one() {
    let entry_rows = [entry_row(1, "h1", NOW), entry_row(2, "h2", NOW + 1)];
    let open = HashMap::from([(1, "h1".to_string()), (2, "h2".to_string())]);
    let (mut steps, mut on_main): (Vec<Value>, Vec<u64>) = (Vec::new(), Vec::new());
    let flakes = HashSet::new();
    for tick in 0..20 {
        let ms = NOW + 10 + tick;
        let state = fold(&steps, DEFAULT_MAX_BATCH);
        let entries = live_entries(&entry_rows, &state, &open);
        let main = format!("m{}", on_main.len());
        // Red only when main plus the batch carries both PRs.
        let gate = match &state.active {
            Some(b) => {
                let all: HashSet<u64> = on_main.iter().chain(&b.prs).copied().collect();
                if all.contains(&1) && all.contains(&2) {
                    Gate::Red {
                        failing: vec!["guards".into()],
                    }
                } else {
                    Gate::Green
                }
            }
            None => Gate::Pending,
        };
        let view = View {
            entries: &entries,
            state: &state,
            main: &main,
            main_red: false,
            gate: &gate,
            flakes: &flakes,
            now_ms: ms,
        };
        let id = state
            .active
            .as_ref()
            .map(|b| b.id.clone())
            .unwrap_or_default();
        let data = match decide(&view) {
            Step::Idle => break,
            Step::Build { prs } => json!({"step": "build", "batch": ms.to_string(), "prs": prs,
                "sha": format!("s{ms}"), "base": main}),
            Step::Land { .. } => {
                let prs = state
                    .active
                    .as_ref()
                    .map(|b| b.prs.clone())
                    .unwrap_or_default();
                // GitHub may still list a landed PR as open: the pinned head
                // in the land row is what keeps it out of the next batch.
                let heads: Vec<(u64, String)> =
                    prs.iter().map(|pr| (*pr, format!("h{pr}"))).collect();
                on_main.extend(&prs);
                json!({"step": "land", "batch": id, "prs": prs, "heads": heads})
            }
            Step::Split { left, right } => {
                json!({"step": "split", "batch": id, "left": left, "right": right})
            }
            Step::Eject { pr, .. } => json!({"step": "eject", "batch": id, "pr": pr,
                "head": format!("h{pr}"), "ends_batch": true}),
            other => panic!("unexpected step {other:?}"),
        };
        steps.push(row(STEP_EVENT, ms, data));
    }
    assert_eq!(on_main, vec![1]);
    let words: Vec<String> = steps.iter().map(|r| text(r, "/data/step")).collect();
    assert_eq!(words, ["build", "split", "build", "land", "build", "eject"]);
}

#[test]
fn provider_defaults_to_auto_and_reads_literals_from_config() {
    let _config = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("mq-provider-test-{}", std::process::id()));
    let cfg = dir.join("config.toml");
    std::fs::create_dir_all(&dir).expect("tempdir");
    std::env::set_var("FNO_CONFIG", &cfg);
    // No config: auto is the default, and the default gate name stands.
    std::fs::write(&cfg, "").expect("write empty config");
    assert_eq!(provider(Path::new(".")), Provider::Auto);
    assert_eq!(gate_workflow(Path::new(".")), GATE_WORKFLOW);
    // Each literal wins; an unknown word falls back to auto.
    for (word, want) in [
        ("github", Provider::Github),
        ("fno", Provider::Fno),
        ("off", Provider::Off),
    ] {
        std::fs::write(&cfg, format!("[merge_queue]\nprovider = \"{word}\"\n"))
            .expect("write config");
        assert_eq!(provider(Path::new(".")), want, "word: {word}");
    }
    std::fs::write(&cfg, "[merge_queue]\nprovider = \"banana\"\n").expect("write config");
    assert_eq!(provider(Path::new(".")), Provider::Auto);
    // A configured gate name replaces the default.
    std::fs::write(&cfg, "[merge_queue]\ngate_workflow = \"my-gate.yml\"\n").expect("write config");
    assert_eq!(gate_workflow(Path::new(".")), "my-gate.yml");
    // queue_kind folds enabled + provider: the queue is opt-in, an
    // explicit fno or github answers without touching the network, and off
    // answers None while enabled stays true. FNO_CONFIG is process-global,
    // so the branches ride inside this one fn under its lock.
    std::fs::write(&cfg, "[merge_queue]\nenabled = true\nprovider = \"fno\"\n")
        .expect("write config");
    assert_eq!(queue_kind(Path::new(".")), Some(QueueKind::Fno));
    std::fs::write(
        &cfg,
        "[merge_queue]\nenabled = true\nprovider = \"github\"\n",
    )
    .expect("write config");
    assert_eq!(queue_kind(Path::new(".")), Some(QueueKind::Github));
    std::fs::write(&cfg, "[merge_queue]\nenabled = true\nprovider = \"off\"\n")
        .expect("write config");
    assert_eq!(queue_kind(Path::new(".")), None);
    std::fs::write(&cfg, "").expect("write empty config");
    // The queue is opt-in: no enabled line, no queue at all.
    assert_eq!(queue_kind(Path::new(".")), None);
    std::env::remove_var("FNO_CONFIG");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn main_moving_rebuilds_three_times_then_holds_until_main_stays_still() {
    let build_row = |ms, base: &str| {
        let data = json!({"step": "build", "batch": "1", "prs": [1], "sha": "s", "base": base});
        row(STEP_EVENT, ms, data)
    };
    let rebuild = |ms| {
        row(
            STEP_EVENT,
            ms,
            json!({"step": "rebuild", "batch": "1", "main_moved": true}),
        )
    };
    let entries = [entry(1, 2, 1)];
    let mut rows = vec![build_row(1, "m0")];
    let state = fold(&rows, DEFAULT_MAX_BATCH);
    assert_eq!(
        step(&entries, &state, "m1", false, &Gate::Green),
        Step::Rebuild { main_moved: true }
    );
    for (ms, base) in [(3, "m1"), (5, "m2"), (7, "m3")] {
        rows.push(rebuild(ms - 1));
        rows.push(build_row(ms, base));
    }
    let state = fold(&rows, DEFAULT_MAX_BATCH);
    assert_eq!(state.rebuilds, 3);
    let hold = Step::Hold {
        reason: "main_moving".into(),
    };
    assert_eq!(step(&entries, &state, "m4", false, &Gate::Pending), hold);
    rows.push(row(
        STEP_EVENT,
        8,
        json!({"step": "hold", "batch": "1", "main": "m4"}),
    ));
    let state = fold(&rows, DEFAULT_MAX_BATCH);
    assert_eq!(step(&entries, &state, "m5", false, &Gate::Pending), hold);
    assert_eq!(
        step(&entries, &state, "m4", false, &Gate::Pending),
        build(&[1])
    );
}

/// queue_kind folds enabled + provider + repo facts: a disabled or `off`
/// queue answers None before any repo read, an explicit `fno` answers Fno,
/// and `github` answers Github. The auto branch needs live repo reads, so
/// this test holds to the branches the config alone decides.

/// The pure auto pick: GitHub's queue only where a repo can have one and a
/// workflow listens on merge_group; everything else lands on fno's own
/// queue.
#[test]
fn the_auto_pick_queues_on_github_only_where_it_can_have_one() {
    // Public org repo with a merge_group listener: GitHub's queue.
    assert_eq!(auto_pick(true, false, false, true), QueueKind::Github);
    // Public personal repo: no GitHub queue even with a listener.
    assert_eq!(auto_pick(false, false, false, true), QueueKind::Fno);
    // Private org repo without Enterprise Cloud: fno's own queue.
    assert_eq!(auto_pick(true, true, false, true), QueueKind::Fno);
    // Private org repo on Enterprise Cloud: GitHub's queue.
    assert_eq!(auto_pick(true, true, true, true), QueueKind::Github);
    // A capable repo whose workflows never hear merge_group still queues,
    // on fno's own queue: the pick never skips.
    assert_eq!(auto_pick(true, false, false, false), QueueKind::Fno);
}
