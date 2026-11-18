use super::*;

const NOW: i64 = 1_800_000_000_000;

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
