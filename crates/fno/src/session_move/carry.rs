//! Carry the transcript, its `<sid>/` sidecar dir, the origin record and the
//! lead canon doc to the same paths on the target. The source is stopped
//! first, so no writer shares the file during the copy.

use super::{login, rsync_cmd, run_cmds, ssh_argv, ssh_cmd, Cmd, MoveError, MovePlan, Shell, Step};
use std::path::Path;

/// `mkdir -p` of a file's parent on the target.
fn mkdir_parent(host: &str, file: &Path) -> Cmd {
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    ssh_cmd(host, &login(&format!("mkdir -p {}", dir.display())))
}

/// Copy one file to the same path on the target.
fn file_cmds(host: &str, file: &Path) -> Vec<Cmd> {
    let path = file.display().to_string();
    vec![mkdir_parent(host, file), rsync_cmd(host, &path, &path, &[])]
}

/// The transcript, its sidecar and its origin record.
pub fn transcript_cmds(plan: &MovePlan) -> Vec<Cmd> {
    let mut cmds = file_cmds(&plan.host, &plan.transcript.path);
    if let Some(sidecar) = &plan.sidecar {
        let dir = format!("{}/", sidecar.display());
        cmds.push(rsync_cmd(&plan.host, &dir, &dir, &[]));
    }
    if let Some(origin) = &plan.origin_record {
        cmds.push(rsync_cmd(
            &plan.host,
            &origin.display().to_string(),
            &origin.display().to_string(),
            &[],
        ));
    }
    cmds
}

/// The lead canon doc, when the row carries a scope with one.
pub fn canon_cmds(plan: &MovePlan) -> Vec<Cmd> {
    plan.canon_doc
        .as_deref()
        .map(|doc| file_cmds(&plan.host, doc))
        .unwrap_or_default()
}

/// Copy one canon doc to the target. The `--all` form calls this for a lead.
pub fn carry_canon_doc(shell: &dyn Shell, host: &str, doc: &Path) -> Result<(), MoveError> {
    run_cmds(shell, Step::CanonDocSync, &file_cmds(host, doc))
}

/// The byte length of the target's copy of the transcript.
fn remote_len(shell: &dyn Shell, plan: &MovePlan) -> Option<u64> {
    let probe = format!("wc -c < {}", plan.transcript.path.display());
    let out = shell
        .run("ssh", &ssh_argv(&plan.host, &login(&probe)))
        .ok()?;
    (out.code == 0).then(|| out.stdout.trim().parse().ok())?
}

pub(super) fn run(plan: &MovePlan, step: Step, shell: &dyn Shell) -> Result<(), MoveError> {
    if step == Step::CanonDocSync {
        return run_cmds(shell, step, &canon_cmds(plan));
    }
    run_cmds(shell, step, &transcript_cmds(plan))?;
    let local = std::fs::metadata(&plan.transcript.path)
        .map_err(|e| MoveError::failed(step, format!("source transcript: {e}")))?
        .len();
    match remote_len(shell, plan) {
        Some(n) if n == local => Ok(()),
        other => Err(MoveError::failed(
            step,
            format!("target transcript is {other:?} bytes, source is {local}"),
        )),
    }
}
