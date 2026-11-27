//! The twins: facts the fno crate and the fno-agent crate each hold a copy
//! of. fno links fno-agent and never the other way, so every check that
//! reads both copies lives here.

use fno_agent::drift as agent_drift;
use fno_agent::harness_capabilities::render_session_argv_with_ids;
use fno_agent::runtime::served_liveness as liveness;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// ONE declaration, TWO doors, byte-identical.
///
/// The mux viewport's renderer (`fno::agents_view::attach_form(...).render(...)`)
/// and the CLI command's renderer (`render_session_argv_with_ids`) are two
/// functions over the same declaration. For every harness the CONTRACT says
/// declares a form, both doors must render byte-identically; for every
/// harness it says declares none, the viewport must find none. Change either
/// door and this fails here, rather than the two drifting until a row opens
/// the wrong thing from one door.
#[test]
fn attach_argv_matches_the_mux_renderer() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fno-agent/src/harness_capabilities.toml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let caps: toml::Value = toml::from_str(&raw).expect("parse harness_capabilities.toml");
    let harness = caps.get("harness").expect("harness table");
    let uuid = "01a04546-28b2-7a41-ae4c-892bbeb8e295";

    for name in harness.as_table().expect("harness table").keys() {
        // Walk the path node by node: a slash-joined `get` is one key, not a
        // path.
        let mut node = harness.get(name).and_then(|n| n.get("resume_strategy"));
        for key in ["forms", "interactive_attach"] {
            node = node.and_then(|n| n.get(key));
        }
        let form_node = node.cloned();
        let kind = form_node
            .as_ref()
            .and_then(|f| f.get("kind"))
            .and_then(|k| k.as_str())
            .map(str::to_string);
        let declares = kind.as_deref().is_some_and(|k| k != "unsupported");
        // Which spelling the form declares, read off the contract itself -
        // the same question the CLI door's caller must answer.
        let tokens_takes_short = form_node
            .as_ref()
            .and_then(|f| f.get("tokens"))
            .and_then(|t| t.as_array())
            .map(|tokens| {
                tokens
                    .iter()
                    .filter_map(|t| t.as_str())
                    .any(|t| t == "{short_id}")
            });

        // The CLI door, over the same packaged contract fno-agent embeds,
        // asked in the spelling the contract declares.
        let cli = match (declares, tokens_takes_short) {
            (true, Some(true)) => {
                render_session_argv_with_ids(name, "interactive_attach", None, Some(uuid))
            }
            (true, Some(false)) => {
                render_session_argv_with_ids(name, "interactive_attach", Some(uuid), None)
            }
            _ => render_session_argv_with_ids(name, "interactive_attach", None, None),
        };
        // The viewport door.
        let viewport = fno::agents_view::attach_form(name).map(|form| form.render(uuid));
        match (declares, cli, viewport) {
            (true, Ok(cli), Some(viewport)) => {
                assert_eq!(
                    cli, viewport,
                    "{name}'s two attach doors drifted; both read one declaration"
                );
            }
            (false, Err(_), None) => {}
            (true, cli, viewport) => panic!(
                "{name} declares a form but the doors disagree (cli={cli:?}, viewport={viewport:?})"
            ),
            (false, cli, viewport) => panic!(
                "{name} declares no form but a door rendered anyway (cli={cli:?}, \
                 viewport={viewport:?})"
            ),
        }
    }
}

/// Law D3141 (command must exist): every door argv row resolves to a
/// real command. The runtime spawns `fno_agent::scrape::fno_bin()` with a
/// row of [`FNO_ARGVS`] - daemon arms, the check-in reader, the lead
/// loop - and those run for months unwatched, so a rename that drops a
/// word fails this test instead of failing a tick log. agents rows walk
/// the front crate's live menu - the surface the org->hq rename broke
/// for team_ledger and the check-in drain. mux rows hold against the
/// generated native tree. Rows on other roots (the merge-close arm's
/// `backlog reconcile`) stay covered end to end by their arm's own
/// tests plus the baseline ratchet, and are asserted here to stay on
/// those roots so an unexpected root change cannot sneak in.
#[test]
fn every_door_argv_row_resolves_to_a_real_command() {
    let tree = include_str!("../../../scripts/ci/native-command-tree.txt");
    for row in fno_agent::door_argv::FNO_ARGVS {
        let argv: &[&str] = row;
        let Some((root, _)) = argv.split_first() else {
            panic!("door argv row is empty");
        };
        match *root {
            "agents" => {
                let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
                match fno::agents_menu::classify(&args) {
                    Some(fno::agents_menu::Agents::Run(_)) => {}
                    other => panic!(
                        "door argv {argv:?} does not resolve through the agents menu: {other:?}"
                    ),
                }
            }
            "mux" => {
                let words: Vec<&str> = argv
                    .iter()
                    .take_while(|w| !w.starts_with('-'))
                    .copied()
                    .collect();
                let path = words.join(" ");
                assert!(
                    tree.lines()
                        .any(|l| l.starts_with(&path.as_str())
                            && l[path.len()..].starts_with('\t')),
                    "door argv {argv:?} is not a native-tree row: {path}"
                );
            }
            "backlog" => {
                // The backlog row carries no menu or tree row: hold it
                // to its exact words here, where the baseline ratchet
                // cannot see it.
                assert_eq!(
                    argv,
                    fno_agent::door_argv::MERGE_CLOSE_RECONCILE,
                    "the merge-close reconcile argv changed; re-check the door it names"
                );
            }
            other => panic!("door argv root {other:?} has no resolver here"),
        }
    }
}

#[test]
fn the_vendored_fno_copy_matches_by_contract() {
    // The parity guard for crates/fno/src/install/build_drift.rs (vendored for
    // the mux server's quiet retirement): both copies must return the
    // same verdict for the same inputs, so the daemon and the mux server
    // can never disagree about whether the same installed build is
    // stale. The types are distinct across the two crates, so the guard
    // compares the one-word labels for identical field values.
    let cases = [
        ("/opt/a/fno", 1u64, "/opt/a/fno", 1u64),
        ("/opt/a/fno", 1, "/opt/a/fno", 2),
        ("/opt/a/fno", 1, "/opt/b/fno", 1),
    ];
    for (rpath, rsize, dpath, dsize) in cases {
        let running = agent_drift::ExeFingerprint {
            path: PathBuf::from(rpath),
            mtime_nanos: 1,
            size: rsize,
        };
        let on_disk = agent_drift::ExeFingerprint {
            path: PathBuf::from(dpath),
            mtime_nanos: 1,
            size: dsize,
        };
        let theirs_running = fno::build_drift::ExeFingerprint {
            path: PathBuf::from(rpath),
            mtime_nanos: 1,
            size: rsize,
        };
        let theirs_disk = fno::build_drift::ExeFingerprint {
            path: PathBuf::from(dpath),
            mtime_nanos: 1,
            size: dsize,
        };
        assert_eq!(
            agent_drift::drift_label(&agent_drift::classify(Some(&running), Some(&on_disk))),
            fno::build_drift::drift_label(&fno::build_drift::classify(
                Some(&theirs_running),
                Some(&theirs_disk)
            )),
            "verdict disagrees at {rpath}/{rsize} vs {dpath}/{dsize}"
        );
    }
    // Unknown parity: no basis to prove drift on either side.
    assert_eq!(
        agent_drift::drift_label(&agent_drift::classify(None, None)),
        fno::build_drift::drift_label(&fno::build_drift::classify(None, None)),
    );
}

const NOW: u64 = 1_800_000_000;

#[test]
fn the_vendored_rule_matches_the_fno_crate_module_by_contract() {
    // The parity guard that lets this mirror exist: the constants and
    // the observable rule must match the fno crate's module exactly.
    assert_eq!(
        liveness::SERVED_LIVENESS_CADENCE,
        fno::served_liveness::SERVED_LIVENESS_CADENCE
    );
    assert_eq!(
        liveness::SERVED_LIVENESS_MAX_AGE_SECS,
        fno::served_liveness::SERVED_LIVENESS_MAX_AGE_SECS
    );
    let word = Some("alive");
    for measured in [Some(NOW - 90), Some(NOW - 121), None] {
        assert_eq!(
            liveness::served_liveness_word(word, measured, NOW),
            fno::served_liveness::served_liveness_word(word, measured, NOW),
            "word gate disagrees at {measured:?}"
        );
        assert_eq!(
            liveness::served_liveness_basis(word, measured, NOW),
            fno::served_liveness::served_liveness_basis(word, measured, NOW),
            "basis disagrees at {measured:?}"
        );
    }
    // No measured input, however broken, reads null or
    // the stale verdict - both mirrors answer `unknown`.
    for (word, measured) in [
        (Some("alive"), Some(NOW - 121)),
        (Some("dead"), Some(NOW - 121)),
        (Some("alive"), None),
        (None, Some(NOW - 10)),
        (None, None),
    ] {
        assert_eq!(
            liveness::served_liveness_word(word, measured, NOW),
            "unknown",
            "word={word:?} measured={measured:?}"
        );
    }
}

#[test]
fn the_pi_attach_argv_is_identical_in_both_crates() {
    let session_id = "fno-tui-0001";
    assert_eq!(
        fno_agent::pi::pi_attach_argv(session_id),
        fno::agents_view::pi_attach_argv(session_id)
    );
}

#[test]
fn the_workspace_sidecar_reader_and_writer_name_one_file() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let home = fno_agent::paths::AgentsHome::at(dir.path());
    assert_eq!(
        fno::workspace_sidecar::path(dir.path()),
        fno_agent::fleet_workspace::path(&home),
        "reader and writer disagree on the file"
    );
}

#[test]
fn the_mux_exit_codes_and_keeper_version_match_the_agent_copies() {
    assert_eq!(
        fno_agent::attach::MUX_THREAD_NO_SERVER,
        fno::mux_cli::EXIT_NO_SERVER
    );
    assert_eq!(
        fno_agent::attach::MUX_THREAD_USAGE,
        fno::mux_cli::EXIT_USAGE
    );
    assert_eq!(
        fno_agent::attach::MUX_THREAD_UNANSWERED,
        fno::mux_cli::EXIT_CONTROL_UNANSWERED
    );
    assert_eq!(
        fno_agent::pane_keeper::PROTOCOL_VERSION,
        fno::pty::KEEPER_PROTOCOL_VERSION
    );
}

/// The daemon's pane-worker stop refusal prints one kill command. The mux's
/// real parser must accept it: the refusal once printed `main:76` while the
/// parser demanded a bare number.
#[test]
fn the_stop_refusal_pane_kill_parses_in_the_mux() {
    let printed = fno_agent::daemon::pane_kill_command("main", 76);
    let selector = printed
        .strip_prefix("fno mux pane kill ")
        .expect("the printed command is the pane kill command");
    let op = fno::cli_args::PaneOp::Kill(fno::cli_args::MuxTail { tail: Vec::new() });
    let parsed = fno::mux_cli::parse_pane_args(&op, &[selector.into()])
        .expect("the refusal's own command must parse");
    assert_eq!(parsed.session.as_deref(), Some("main"));
    assert!(matches!(
        parsed.cmd,
        fno::mux_cli::PaneCmd::Kill { pane: 76, .. }
    ));
}
