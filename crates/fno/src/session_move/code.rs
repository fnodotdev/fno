//! Carry the code: push the branch straight to the target's git dir over ssh
//! (a move needs no GitHub), add the worktree there, and sync a dirty tree
//! after the source stops.

use super::{
    login, rsync_cmd, run_cmd, run_cmds, sh_quote, ssh_argv, ssh_cmd, Cmd, MoveError, MovePlan,
    Shell, Step,
};

/// The receive-pack word that finds git under the target's login PATH. Git
/// appends the quoted repo path, which lands in `$0`.
const RECEIVE_PACK: &str = "sh -lc 'exec git-receive-pack \"$0\"'";

/// Push the branch to the target's common dir, then add the worktree there.
pub fn push_cmds(plan: &MovePlan) -> Vec<Cmd> {
    let url = format!("ssh://{}{}", plan.host, plan.common_dir.display());
    let push = Cmd {
        program: "git",
        args: vec![
            "-C".into(),
            plan.worktree.display().to_string(),
            "push".into(),
            format!("--receive-pack={RECEIVE_PACK}"),
            url,
            format!("HEAD:refs/heads/{}", plan.branch),
        ],
    };
    let add = format!(
        "git -C {} worktree add {} {}",
        plan.common_dir.display(),
        plan.worktree.display(),
        sh_quote(&plan.branch)
    );
    vec![push, ssh_cmd(&plan.host, &login(&add))]
}

/// The dirty-tree copy: the same path on the target, `.git` pointer file and
/// build output left alone.
pub fn sync_cmd(plan: &MovePlan) -> Cmd {
    let src = format!("{}/", plan.worktree.display());
    rsync_cmd(
        &plan.host,
        &src,
        &src,
        &["--delete", "--exclude", "/.git", "--exclude", "/target/"],
    )
}

/// True when the target path already carries the move's branch.
fn already_carried(plan: &MovePlan, shell: &dyn Shell) -> bool {
    let probe = format!(
        "git -C {} rev-parse --abbrev-ref HEAD",
        plan.worktree.display()
    );
    shell
        .run("ssh", &ssh_argv(&plan.host, &login(&probe)))
        .is_ok_and(|o| o.code == 0 && o.stdout.trim() == plan.branch)
}

pub(super) fn run(plan: &MovePlan, step: Step, shell: &dyn Shell) -> Result<(), MoveError> {
    match step {
        Step::PushCommits if already_carried(plan, shell) => Ok(()),
        Step::PushCommits => run_cmds(shell, step, &push_cmds(plan)),
        _ => run_cmd(shell, step, &sync_cmd(plan)).map(drop),
    }
}
