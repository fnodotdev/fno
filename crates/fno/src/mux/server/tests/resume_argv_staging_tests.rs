//! The x-eb79 staging test family: a non-claude gesture spawns the argv the
//! off-loop `fno-agents resume-argv` resolution staged, and a failed
//! resolution fails OPEN with the degradation named. The muse arm is the
//! exception this family also pins: its thread row has no terminal form,
//! so the gesture shells the resume door's host revive instead. Shrinks
//! server.rs under the file-budget gate; same treatment as the other
//! families here.

use super::*;
use crate::server::agent_actions::{fno_env_lock, write_fake_bin};

/// Pin FNO_AGENTS_BIN/HOME for the gesture's door shell and restore both on
/// scope exit, assert failures included. The agent_actions helper is
/// module-private by design; the staging family pins its own.
struct PinnedDoorEnv {
    prev_bin: Option<std::ffi::OsString>,
    prev_home: Option<std::ffi::OsString>,
}

impl PinnedDoorEnv {
    fn set(bin: impl AsRef<std::path::Path>, home: impl AsRef<std::path::Path>) -> Self {
        let prev_bin = std::env::var_os("FNO_AGENTS_BIN");
        let prev_home = std::env::var_os("FNO_AGENTS_HOME");
        std::env::set_var("FNO_AGENTS_BIN", bin.as_ref());
        std::env::set_var("FNO_AGENTS_HOME", home.as_ref());
        Self {
            prev_bin,
            prev_home,
        }
    }
}

impl Drop for PinnedDoorEnv {
    fn drop(&mut self) {
        match self.prev_bin.take() {
            Some(v) => std::env::set_var("FNO_AGENTS_BIN", v),
            None => std::env::remove_var("FNO_AGENTS_BIN"),
        }
        match self.prev_home.take() {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
    }
}

#[test]
fn resume_agent_runs_the_staged_resume_argv() {
    // AC3-HP + AC3-LOOP (the codex positive): the gesture spawns the
    // argv the off-loop resolution staged - grant and --cd included -
    // never the bare declared form.
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let cwd = std::env::temp_dir().join("fno-eb79-staged");
    std::fs::create_dir_all(&cwd).unwrap();
    let shell = core
        .spawn_pane(24, 80, cwd.to_string_lossy().as_ref())
        .unwrap();
    core.session.add_squad(
        7,
        vec![cwd.to_string_lossy().into_owned()],
        None,
        Tab {
            name: None,
            id: 70,
            root: Node::Leaf(shell),
            focus: shell,
        },
    );
    core.agents = vec![RegistryAgent {
        harness_session_id: Some("01a027ad-fe00-7c12-a116-9ee37c6bdfec".into()),
        harness: Some("codex".into()),
        name: "t-eb79-staged".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        exited: true,
        liveness: agents_view::Liveness::Dead,
        ..Default::default()
    }];
    let (c, mut rx) = client_with_rx(1);
    core.clients.push(c);
    core.staged_resume_argv = Some(vec![
        // argv[0] is an absolute benign binary (the house /bin/cat shape):
        // the spawn gate refuses a program not on PATH, and a CI runner has
        // no codex. The grant and --cd tokens ride positions 1.. verbatim;
        // the grant SPLICE itself is pinned by the fno-agents parity tests.
        "/bin/cat".into(),
        "-c".into(),
        "sandbox_workspace_write.writable_roots=[\"/wt/.fno/plans\"]".into(),
        "--cd".into(),
        "/wt".into(),
        "resume".into(),
        "01a027ad-fe00-7c12-a116-9ee37c6bdfec".into(),
    ]);
    core.command(
        1,
        Command::ResumeAgent {
            name: "t-eb79-staged".into(),
        },
    );
    let notices = drain_notices(&mut rx).join("\n");
    assert!(notices.contains("resumed t-eb79-staged"), "{notices}");
    let new_panes: Vec<u64> = core
        .panes
        .keys()
        .filter(|&&p| p != shell)
        .copied()
        .collect();
    assert_eq!(new_panes.len(), 1, "exactly one resumed pane");
    let entry = core.panes.get(&new_panes[0]).unwrap();
    assert_eq!(
        entry.cmd.as_deref(),
        Some("cat"),
        "the staged argv's program ran - the staging reached the spawn, not the declared form"
    );
    for pid in new_panes {
        core.reap_pane(pid);
    }
    core.reap_pane(shell);
    let _ = std::fs::remove_dir_all(&cwd);
}

/// The muse tap arm never reaches the staged-argv lane: an exited muse
/// thread row's revival is the resume door's MuseHostResume route (the
/// fno-held host relaunch on the same session id, the same door the CLI
/// resume command and the restore muse pass use). The headless host has no
/// terminal form, so the old fall-through minted a `muse resume <sid>`
/// pane - a second serve on the row's own socket, answered -32021
/// sessionInUse. The gesture shells the door once, spawns no pane, and the
/// door's verdict is the notice.
#[tokio::test]
async fn muse_row_tap_shells_the_resume_door_and_never_mints_a_pane() {
    let _serial = fno_env_lock();
    let cwd = std::env::temp_dir().join(format!("fno-muse-tap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).unwrap();
    let bin = cwd.join("fake-agents.sh");
    write_fake_bin(
        &bin,
        "#!/bin/bash\n\
         printf '%s\\n' \"$*\" >> \"$FNO_AGENTS_HOME/argv.log\"\n\
         echo 'fno agents resume: t-muse-tap revived on a fresh muse host (pid 4242, socket /tmp/t-muse-tap.sock)'\n\
         exit 0\n",
    );
    let _env = PinnedDoorEnv::set(&bin, &cwd);
    let (self_tx, mut self_rx) = mpsc::channel::<CoreMsg>(8);
    let mut core = empty_core();
    core.self_tx = self_tx;
    core.agents = vec![RegistryAgent {
        harness: Some("muse".into()),
        substrate: Some("thread".into()),
        name: "t-muse-tap".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        harness_session_id: Some("muse-sid-tap-1".into()),
        exited: true,
        liveness: agents_view::Liveness::Dead,
        ..Default::default()
    }];
    let (client, mut _rx) = client_with_rx(1);
    core.clients.push(client);
    core.command(
        1,
        Command::ResumeAgent {
            name: "t-muse-tap".into(),
        },
    );
    let notice = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match self_rx.recv().await {
                Some(CoreMsg::DispatchResult { notice, .. }) => break notice,
                Some(_) => continue,
                None => panic!("core channel closed before the revive verdict landed"),
            }
        }
    })
    .await
    .expect("the muse revive verdict within 10s");
    assert!(
        notice.contains("revived on a fresh muse host"),
        "the door's own verdict is the notice: {notice}"
    );
    // The regression: the old arm minted a pane. The revival is headless.
    assert!(
        core.panes.is_empty(),
        "a muse tap spawns no pane: {:?}",
        core.panes.keys().collect::<Vec<_>>()
    );
    // And the one shell was the resume door, never a spawn or a
    // resume-argv resolution.
    let log = std::fs::read_to_string(cwd.join("argv.log")).unwrap_or_default();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["resume t-muse-tap"],
        "exactly one resume-door call: {log}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}
