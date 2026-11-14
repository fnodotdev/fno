//! Cut over: stop the source first so two writers never share the
//! transcript, register the target row, hand the claims over, resume, verify
//! on the target, and only then retire the source.

use super::{
    login, read_rows_on, run_cmd, run_cmds, sh_quote, ssh_argv, ssh_cmd, Cmd, MoveError, MovePlan,
    Shell, Step,
};
use std::time::Duration;

/// How long verify waits for the target, and how often it looks.
const VERIFY_POLLS: u32 = 24;
const VERIFY_EVERY: Duration = Duration::from_secs(5);

fn fno_local(args: &[&str]) -> Cmd {
    Cmd {
        program: "fno",
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

/// The holder string both machines use for a session's node claims.
fn holder(plan: &MovePlan) -> String {
    format!("target-session:{}", plan.session_id)
}

/// The registry name `adopt` gives a session: `cc-<first eight id chars>`.
fn adopted_name(plan: &MovePlan) -> String {
    format!("cc-{}", plan.session_id.chars().take(8).collect::<String>())
}

/// Register the session on the target under the source name.
pub fn target_row_cmds(plan: &MovePlan) -> Vec<Cmd> {
    let adopt = format!("fno agents registry adopt {}", plan.session_id);
    let rename = format!(
        "fno agents registry rename {} {}",
        adopted_name(plan),
        sh_quote(&plan.name)
    );
    vec![
        ssh_cmd(&plan.host, &login(&adopt)),
        ssh_cmd(&plan.host, &login(&rename)),
    ]
}

/// Take over each `node:` claim on the target from the same holder string.
pub fn claims_cmds(plan: &MovePlan) -> Vec<Cmd> {
    let who = holder(plan);
    plan.claims
        .iter()
        .map(|key| {
            let take = format!(
                "fno agents claim acquire {key} --holder {who} --handover-from {who} --ttl 12h --pid-unavailable -R {}",
                sh_quote(&format!("moved from {}", plan.from_host))
            );
            ssh_cmd(&plan.host, &login(&take))
        })
        .collect()
}

/// Resume on the target, so the session takes one turn there.
pub fn resume_cmd(plan: &MovePlan) -> Cmd {
    let message = format!("moved from {}; continue", plan.from_host);
    let resume = format!(
        "fno agents session resume {} --message {}",
        sh_quote(&plan.name),
        sh_quote(&message)
    );
    ssh_cmd(&plan.host, &login(&resume))
}

/// Remove the source row and its stopped session, so a mail wake cannot
/// revive it. The claims moved at handover: releasing them by the shared
/// holder string would drop the target's.
pub fn retire_cmd(plan: &MovePlan) -> Cmd {
    fno_local(&["agents", "session", "rm", &plan.name, "--force"])
}

/// The target lists the session live under the same name and id, and its
/// transcript is longer than the copied length.
fn target_has_moved(shell: &dyn Shell, plan: &MovePlan, copied: u64) -> bool {
    let live = read_rows_on(shell, &plan.host).is_ok_and(|rows| {
        rows.iter().any(|r| {
            r.name == plan.name
                && r.harness_session_id.as_deref() == Some(plan.session_id.as_str())
                && r.liveness.as_deref() == Some("alive")
        })
    });
    let probe = format!("wc -c < {}", plan.transcript.path.display());
    let grown = shell
        .run("ssh", &ssh_argv(&plan.host, &login(&probe)))
        .ok()
        .filter(|o| o.code == 0)
        .and_then(|o| o.stdout.trim().parse::<u64>().ok())
        .is_some_and(|n| n > copied);
    live && grown
}

/// Poll the target up to `polls` times, calling `wait` between looks.
pub fn verify(
    plan: &MovePlan,
    shell: &dyn Shell,
    polls: u32,
    wait: &dyn Fn(),
) -> Result<(), MoveError> {
    let copied = std::fs::metadata(&plan.transcript.path)
        .map_err(|e| MoveError::failed(Step::Verify, format!("source transcript: {e}")))?
        .len();
    for i in 0..polls {
        if target_has_moved(shell, plan, copied) {
            return Ok(());
        }
        if i + 1 < polls {
            wait();
        }
    }
    Err(MoveError::failed(
        Step::Verify,
        format!(
            "{} showed no live row with a newer transcript within {} seconds",
            plan.host,
            u64::from(polls) * VERIFY_EVERY.as_secs()
        ),
    ))
}

pub(super) fn run(plan: &MovePlan, step: Step, shell: &dyn Shell) -> Result<(), MoveError> {
    match step {
        Step::StopSource => run_cmd(
            shell,
            step,
            &fno_local(&["agents", "session", "stop", &plan.name]),
        )
        .map(drop),
        Step::TargetRow => run_cmds(shell, step, &target_row_cmds(plan)),
        Step::ClaimsHandover => run_cmds(shell, step, &claims_cmds(plan)),
        Step::Resume => run_cmd(shell, step, &resume_cmd(plan)).map(drop),
        Step::Verify => verify(plan, shell, VERIFY_POLLS, &|| {
            std::thread::sleep(VERIFY_EVERY);
        }),
        _ => {
            run_cmd(shell, step, &retire_cmd(plan))?;
            println!(
                "retired on {}: row and stopped session {}",
                plan.from_host, plan.name
            );
            Ok(())
        }
    }
}
