use super::*;
use crate::store_remote::test_primary;

fn machine(id: &str, host: &str, beat_at: i64) -> Machine {
    Machine {
        machine_id: id.into(),
        host: host.into(),
        owner: "jason".into(),
        role: "worker".into(),
        beat_at,
        cpu: Some(0.25),
        takes_work: true,
        ..Machine::default()
    }
}

fn session(machine_id: &str, name: &str, id: &str, parent: Option<&str>) -> Session {
    Session {
        machine_id: machine_id.into(),
        name: name.into(),
        session: Some(id.into()),
        parent: parent.map(str::to_string),
        ..Session::default()
    }
}

#[test]
fn fleet_beacon_each_machine_replaces_only_its_own_rows() {
    let primary = test_primary::start();
    let a = machine("mac-a", "mbp", 1000);
    let b = machine("mac-b", "linux1", 1000);
    publish(
        &primary.remote,
        &a,
        &[session("mac-a", "w1", "aaaa1111", None)],
    )
    .unwrap();
    publish(
        &primary.remote,
        &b,
        &[session("mac-b", "w2", "bbbb2222", None)],
    )
    .unwrap();
    // A's next beat drops w1 and adds w3; B's rows stay as B wrote them.
    let a2 = Machine {
        beat_at: 1060,
        ..a.clone()
    };
    publish(
        &primary.remote,
        &a2,
        &[session("mac-a", "w3", "cccc3333", None)],
    )
    .unwrap();

    let Fleet {
        machines, sessions, ..
    } = read(&primary.remote).unwrap();
    assert_eq!(machines.len(), 2);
    assert_eq!(
        machines
            .iter()
            .find(|m| m.machine_id == "mac-a")
            .unwrap()
            .beat_at,
        1060
    );
    let names: Vec<_> = sessions
        .iter()
        .map(|s| (s.machine_id.as_str(), s.name.as_str()))
        .collect();
    assert_eq!(names, vec![("mac-a", "w3"), ("mac-b", "w2")]);
}

#[test]
fn fleet_beacon_stale_machine_reads_offline_and_child_names_cross_machine_parent() {
    let machines = vec![
        machine("mac-a", "mbp", 1000),
        machine("mac-b", "linux1", 400),
    ];
    let sessions = vec![
        session("mac-a", "lead", "abcdef123456", None),
        session("mac-b", "child", "99998888", Some("abcdef12")),
    ];
    let map = map_json(&machines, &sessions, "mac-a", 1010, DEFAULT_FRESH_S);
    let rows = map["machines"].as_array().unwrap();
    assert_eq!(rows[0]["fresh"], json!(true));
    assert_eq!(rows[0]["local"], json!(true));
    assert_eq!(rows[1]["fresh"], json!(false));
    assert_eq!(rows[1]["age_s"], json!(610));
    assert!(machine_line(&rows[1]).contains("offline 610s"));
    assert_eq!(rows[1]["sessions"][0]["parent_at"], json!("lead@mbp"));
    assert!(render(&map).contains("child - - - - <- lead@mbp"));
}

#[test]
fn fleet_beacon_route_roundtrips_and_nulls_for_a_beat_without_it() {
    let primary = test_primary::start();
    let a = machine("mac-a", "mbp", 1000);
    let mut routed = session("mac-a", "w1", "aaaa1111", None);
    routed.route = Some("zai".into());
    publish(&primary.remote, &a, &[routed]).unwrap();
    let Fleet { sessions, .. } = read(&primary.remote).unwrap();
    assert_eq!(sessions[0].route.as_deref(), Some("zai"));
    let map = map_json(&[a], &sessions, "mac-a", 1010, DEFAULT_FRESH_S);
    assert_eq!(map["machines"][0]["sessions"][0]["route"], json!("zai"));

    // A machine whose beat predates the table files no route row; the read
    // keeps null rather than mounting a wrong lane.
    let b = machine("mac-b", "linux1", 1000);
    publish(
        &primary.remote,
        &b,
        &[session("mac-b", "w2", "bbbb2222", None)],
    )
    .unwrap();
    let Fleet { sessions, .. } = read(&primary.remote).unwrap();
    let w2 = sessions.iter().find(|s| s.name == "w2").unwrap();
    assert_eq!(w2.route, None);
}

#[test]
fn a_beat_synthesizes_the_badge_from_its_own_readings() {
    // A machine whose mux serves no card still carries the glyph: the beat
    // answers from needs_input and the transcript status word.
    let top = json!({"workers": [
        {"name": "a", "status": "writing"},
        {"name": "b", "status": "quiet"},
        {"name": "c", "status": "parked"},
        {"name": "d", "status": "writing", "needs_input": true},
        {"name": "e", "status": "writing", "badge": "done"},
    ]});
    let sessions = sessions_from_top("mac-a", &top);
    let badge_of = |name: &str| {
        sessions
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .badge
            .clone()
    };
    assert_eq!(badge_of("a"), Some("working".into()));
    assert_eq!(badge_of("b"), None);
    assert_eq!(badge_of("c"), Some("done".into()));
    assert_eq!(badge_of("d"), Some("blocked".into()));
    assert_eq!(badge_of("e"), Some("done".into()));
}

#[test]
fn a_beat_samples_an_activity_ring_and_the_card_wins() {
    // The beat diffs the registry row's cumulative pair, keeps 8 cells, and
    // never clobbers a card the mux filed. A session the beat stops naming
    // loses its ring.
    let top = json!({"workers": [{"name": "a", "session_id": "s1",
        "tool_calls": 10, "tool_errors": 1}]});
    let mut rings = Default::default();
    let mut sessions = sessions_from_top("mac-a", &top);
    sample_beat_rings(&mut rings, &mut sessions);
    let s1 = sessions.iter().find(|s| s.name == "a").unwrap();
    assert_eq!(s1.activity, Some(json!([[10, 1]])));
    // Second beat: the delta is the cell, not the cumulative.
    let top2 = json!({"workers": [{"name": "a", "session_id": "s1",
        "tool_calls": 14, "tool_errors": 1}]});
    let mut sessions2 = sessions_from_top("mac-a", &top2);
    sample_beat_rings(&mut rings, &mut sessions2);
    let s = sessions2.iter().find(|s| s.name == "a").unwrap();
    assert_eq!(s.activity, Some(json!([[10, 1], [4, 0]])));
    // The card wins when the mux filed one.
    let top3 = json!({"workers": [{"name": "a", "session_id": "s1",
        "tool_calls": 20, "tool_errors": 0, "activity": [[1,1],[2,2]]}]});
    let mut sessions3 = sessions_from_top("mac-a", &top3);
    sample_beat_rings(&mut rings, &mut sessions3);
    let s3 = sessions3.iter().find(|s| s.name == "a").unwrap();
    assert_eq!(s3.activity, Some(json!([[1, 1], [2, 2]])));
}
