use super::*;
use std::cell::RefCell;

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

fn row(name: &str, harness: &str, scope: Option<&str>) -> Row {
    Row {
        name: name.into(),
        harness: harness.into(),
        harness_session_id: Some("sid-1".into()),
        cwd: Some("/work/tree".into()),
        node: Some("x-1".into()),
        role_scope: scope.map(String::from),
        liveness: Some("alive".into()),
    }
}

fn facts(dirty: bool) -> Facts {
    Facts {
        worktree: "/work/tree".into(),
        branch: "feature/x".into(),
        head: "abc1234".into(),
        dirty,
        transcript: Some(Transcript {
            path: "/store/projects/p/sid-1.jsonl".into(),
            store: "claude".into(),
            rel: "p/sid-1.jsonl".into(),
        }),
        canon_doc: None,
        common_dir: "/repo/.git".into(),
        sidecar: Some("/store/projects/p/sid-1".into()),
        origin_record: None,
    }
}

/// Records every call; answers the list and git reads the move makes.
struct Fake {
    rows_json: String,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new(rows_json: &str) -> Self {
        Fake {
            rows_json: rows_json.into(),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn ssh_calls(&self) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("ssh"))
            .count()
    }
}

impl Shell for Fake {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, Fault> {
        self.calls
            .borrow_mut()
            .push(format!("{program} {}", args.join(" ")));
        let stdout = match program {
            "fno" if args.first().map(String::as_str) == Some("agents") => self.rows_json.clone(),
            "git" if args.iter().any(|a| a == "--show-toplevel") => "/work/tree\n".into(),
            "git" if args.iter().any(|a| a == "--abbrev-ref") => "feature/x\n".into(),
            "git" if args.iter().any(|a| a == "--porcelain") => " M file\n".into(),
            _ => String::new(),
        };
        Ok(Output {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

fn locate_ok(_sid: &str) -> Option<Transcript> {
    facts(false).transcript
}

const ROWS: &str = r#"{"agents":[
  {"name":"w1","harness":"claude","harness_session_id":"sid-1","cwd":"/work/tree","node":"x-1"},
  {"name":"cx","harness":"codex","harness_session_id":"sid-2","cwd":"/work/tree"}
]}"#;

fn parse_args_reads_name_host_and_flags() {
    let opts = parse_args(&args(&["w1", "--to", "imac", "--dry-run"])).unwrap();
    assert_eq!(opts.name.as_deref(), Some("w1"));
    assert_eq!(opts.to.as_deref(), Some("imac"));
    assert!(opts.dry_run && !opts.all);
    let all = parse_args(&args(&["--all", "--to=imac"])).unwrap();
    assert!(all.all && all.name.is_none());
}

fn parse_args_refuses_what_it_cannot_run() {
    assert_eq!(parse_args(&args(&["--help"])), Err(ArgError::Help));
    for bad in [
        &["w1"][..],
        &["--to", "imac"][..],
        &["w1", "--all", "--to", "imac"][..],
        &["w1", "--to"][..],
        &["w1", "--to", "imac", "--force"][..],
        &["w1", "w2", "--to", "imac"][..],
    ] {
        assert!(
            matches!(parse_args(&args(bad)), Err(ArgError::Usage(_))),
            "{bad:?}"
        );
    }
}

fn a_dirty_claude_row_plans_every_step_in_order() {
    let plan = build_plan(
        &row("w1", "claude", Some("lead")),
        facts(true),
        "imac",
        "laptop",
    )
    .unwrap();
    let labels: Vec<&str> = plan.steps.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "preflight",
            "push commits",
            "stop source",
            "worktree sync",
            "transcript sync",
            "canon doc sync",
            "target row",
            "claims handover",
            "resume",
            "verify",
            "retire"
        ]
    );
    assert_eq!(plan.claims, ["node:x-1"]);
}

fn a_clean_tree_skips_only_the_worktree_sync() {
    let plan = build_plan(&row("w1", "claude", None), facts(false), "imac", "laptop").unwrap();
    assert!(!plan.steps.contains(&Step::WorktreeSync));
    assert_eq!(plan.steps.len(), 10);
}

fn the_source_stops_before_any_copy_and_retires_last() {
    let plan = build_plan(&row("w1", "claude", None), facts(true), "imac", "laptop").unwrap();
    let at = |s: Step| plan.steps.iter().position(|x| *x == s).unwrap();
    assert!(at(Step::StopSource) < at(Step::WorktreeSync));
    assert!(at(Step::StopSource) < at(Step::TranscriptSync));
    assert!(at(Step::Verify) < at(Step::Retire));
    assert_eq!(plan.steps.last(), Some(&Step::Retire));
}

fn a_missing_transcript_or_session_id_refuses() {
    let mut no_transcript = facts(false);
    no_transcript.transcript = None;
    assert!(matches!(
        build_plan(&row("w1", "claude", None), no_transcript, "imac", "laptop"),
        Err(Refusal::NoTranscript { .. })
    ));
    let mut no_sid = row("w1", "claude", None);
    no_sid.harness_session_id = None;
    assert_eq!(
        build_plan(&no_sid, facts(false), "imac", "laptop").unwrap_err(),
        Refusal::NoSessionId("w1".into())
    );
}

fn hosts_that_name_this_machine_or_no_machine_refuse() {
    assert!(check_host("imac2d2", "Laptop.local").is_ok());
    assert!(matches!(
        check_host("laptop", "Laptop.local"),
        Err(Refusal::SameHost(_))
    ));
    assert!(matches!(
        check_host("me@LAPTOP.local", "laptop"),
        Err(Refusal::SameHost(_))
    ));
    assert!(matches!(
        check_host("localhost", "laptop"),
        Err(Refusal::SameHost(_))
    ));
    assert!(matches!(
        check_host("-oProxyCommand=x", "laptop"),
        Err(Refusal::BadHost(_))
    ));
    assert!(matches!(
        check_host("a b", "laptop"),
        Err(Refusal::BadHost(_))
    ));
}

fn a_codex_row_exits_2_naming_codex_and_runs_no_ssh() {
    let shell = Fake::new(ROWS);
    let opts = parse_args(&args(&["cx", "--to", "imac"])).unwrap();
    assert_eq!(move_one(&opts, &shell, "laptop", &locate_ok), 2);
    assert_eq!(shell.ssh_calls(), 0);
    let text = Refusal::OtherHarness {
        name: "cx".into(),
        harness: "codex".into(),
    }
    .to_string();
    assert!(text.contains("codex") && text.contains("manual recipe"));
}

fn an_unknown_name_refuses_before_any_ssh() {
    let shell = Fake::new(ROWS);
    let opts = parse_args(&args(&["ghost", "--to", "imac"])).unwrap();
    assert_eq!(move_one(&opts, &shell, "laptop", &locate_ok), 2);
    assert_eq!(shell.ssh_calls(), 0);
}

fn dry_run_prints_the_steps_and_runs_no_ssh() {
    let shell = Fake::new(ROWS);
    let opts = parse_args(&args(&["w1", "--to", "imac", "--dry-run"])).unwrap();
    assert_eq!(move_one(&opts, &shell, "laptop", &locate_ok), 0);
    assert_eq!(shell.ssh_calls(), 0);
    let plan = build_plan(
        &row("w1", "claude", None),
        gather_facts(&shell, &row("w1", "claude", None), &locate_ok),
        "imac",
        "laptop",
    )
    .unwrap();
    let text = render_dry_run(&plan);
    assert!(text.contains("nothing is moved") && text.contains("4. worktree sync"));
}

fn a_refused_preflight_stops_nothing() {
    let shell = Fake::new(ROWS);
    let opts = parse_args(&args(&["w1", "--to", "imac"])).unwrap();
    // The fake answers the daemon probe with no output, so the daemon
    // check refuses: no step ran and nothing was stopped.
    assert_eq!(move_one(&opts, &shell, "laptop", &locate_ok), 2);
    assert!(!shell.calls.borrow().iter().any(|c| c.contains("stop")));
}

fn the_daemon_probe_tells_gui_born_from_ssh_and_job_born() {
    assert!(
        daemon_is_gui_born("claude daemon run XPC_SERVICE_NAME=application.com.x.1 HOME=/h")
            .is_ok()
    );
    // AC5: a plain terminal app birth reads the stub service name `0` and
    // inherits the user's gui bootstrap; it is healthy and must pass.
    assert!(daemon_is_gui_born(
        "claude daemon run XPC_SERVICE_NAME=0 __CFBundleIdentifier=com.mitchellh.ghostty"
    )
    .is_ok());
    assert!(daemon_is_gui_born("claude daemon run XPC_SERVICE_NAME=0").is_ok());
    // AC6: the ssh birth keeps its marker and still refuses.
    assert!(
        daemon_is_gui_born("claude daemon run SSH_CONNECTION=1.2.3.4 XPC_SERVICE_NAME=0").is_err()
    );
    assert!(daemon_is_gui_born("claude daemon run XPC_SERVICE_NAME=com.fno.job").is_err());
    assert!(daemon_is_gui_born("").is_err());
}

fn ssh_argv_puts_the_host_after_the_option_fence() {
    let argv = ssh_argv("imac", "echo hi");
    assert_eq!(argv.get(4).map(String::as_str), Some("--"));
    assert_eq!(argv.get(5).map(String::as_str), Some("imac"));
}

fn plan(dirty: bool, scope: Option<&str>) -> MovePlan {
    build_plan(&row("w1", "claude", scope), facts(dirty), "imac", "laptop").unwrap()
}

fn the_code_step_pushes_to_the_target_common_dir_then_adds_the_worktree() {
    let cmds = code::push_cmds(&plan(false, None));
    let push = cmds.first().unwrap();
    assert_eq!(push.program, "git");
    assert!(push.args.contains(&"ssh://imac/repo/.git".to_string()));
    assert!(push.args.contains(&"HEAD:refs/heads/feature/x".to_string()));
    let add = cmds.get(1).unwrap().args.last().unwrap().clone();
    assert!(
        add.contains("worktree add /work/tree ") && add.contains("feature/x"),
        "{add}"
    );
}

fn a_dirty_tree_syncs_with_rsync_and_leaves_its_pointer_and_target_alone() {
    let cmd = code::sync_cmd(&plan(true, None));
    assert_eq!(cmd.program, "rsync");
    assert!(cmd.args.windows(2).any(|w| w == ["--exclude", "/.git"]));
    assert!(cmd.args.windows(2).any(|w| w == ["--exclude", "/target/"]));
    assert_eq!(
        cmd.args.last().map(String::as_str),
        Some("imac:/work/tree/")
    );
}

fn the_transcript_copy_carries_the_sidecar_and_makes_the_parent() {
    let cmds = carry::transcript_cmds(&plan(false, None));
    let ssh = cmds.first().unwrap().args.last().unwrap().clone();
    assert!(ssh.contains("mkdir -p /store/projects/p"), "{ssh}");
    assert!(cmds.iter().any(|c| c
        .args
        .last()
        .is_some_and(|a| a == "imac:/store/projects/p/sid-1/")));
    assert!(carry::canon_cmds(&plan(false, None)).is_empty());
}

fn the_cutover_registers_resumes_and_retires_by_name() {
    let p = plan(false, None);
    let rows = cutover::target_row_cmds(&p);
    let rename = rows.get(1).unwrap().args.last().unwrap().clone();
    assert!(
        rename.contains("rename cc-sid-1") && rename.contains("'w1'"),
        "{rename}"
    );
    let claims = cutover::claims_cmds(&p);
    let take = claims.first().unwrap().args.last().unwrap().clone();
    assert!(take.contains("node:x-1") && take.contains("--handover-from target-session:sid-1"));
    let resume = cutover::resume_cmd(&p).args.last().unwrap().clone();
    assert!(
        resume.contains("session resume ")
            && resume.contains("w1")
            && resume.contains("moved from laptop")
    );
    assert_eq!(
        cutover::retire_cmd(&p).args,
        ["agents", "session", "rm", "w1", "--force"]
    );
}

fn a_verify_that_cannot_read_the_source_fails_at_verify() {
    // The fixture transcript path does not exist here, so verify fails at
    // the length read, before any poll: the same exit 1 path.
    let shell = Fake::new(ROWS);
    let err = cutover::verify(&plan(false, None), &shell, 3, &|| {}).unwrap_err();
    assert!(matches!(
        err,
        MoveError::Failed {
            step: Step::Verify,
            ..
        }
    ));
}

fn a_path_with_a_space_or_a_quote_is_not_plain() {
    assert!(path_is_plain(Path::new("/work/tree-1/a.b")));
    for bad in ["/work/my tree", "/work/it's", "relative/path", "/a;rm"] {
        assert!(!path_is_plain(Path::new(bad)), "{bad}");
    }
    assert_eq!(sh_quote("it's"), "'it'\\''s'");
}

fn all_sorts_workers_leads_and_skips() {
    let cx = row("cx", "codex", None);
    let mut dead = row("dead", "claude", None);
    dead.liveness = Some("dead".into());
    let rows = [
        row("a", "claude", None),
        row("b", "claude", None),
        row("lead", "claude", Some("x-epic")),
        cx,
        dead,
    ];
    let plan = all::partition(&rows);
    assert_eq!(plan.workers, ["a", "b"]);
    assert_eq!(plan.leads, [("lead".to_string(), "x-epic".to_string())]);
    assert_eq!(plan.skipped.len(), 2);
    let text = all::render_dry_run(&plan, "imac");
    assert_eq!(text.matches("  move ").count(), 2);
    assert!(text.contains("carry canon doc for lead") && text.contains("hand off:"));
    assert!(
        text.contains("no resume") && !text.lines().any(|l| l.trim_start().starts_with("resume"))
    );
}

fn a_failed_all_names_the_workers_after_the_failure() {
    let workers: Vec<String> = ["one", "two", "three"].map(String::from).into();
    assert_eq!(all::left_after(&workers, 1), ["three"]);
    assert!(all::left_after(&workers, 2).is_empty());
}

#[test]
fn the_command_line_parses_or_refuses() {
    parse_args_reads_name_host_and_flags();
    parse_args_refuses_what_it_cannot_run();
}

#[test]
fn the_plan_orders_its_steps_and_refuses_a_missing_transcript() {
    a_dirty_claude_row_plans_every_step_in_order();
    a_clean_tree_skips_only_the_worktree_sync();
    the_source_stops_before_any_copy_and_retires_last();
    a_missing_transcript_or_session_id_refuses();
}

#[test]
fn every_refusal_happens_before_a_step_runs() {
    hosts_that_name_this_machine_or_no_machine_refuse();
    a_codex_row_exits_2_naming_codex_and_runs_no_ssh();
    an_unknown_name_refuses_before_any_ssh();
    a_refused_preflight_stops_nothing();
    the_daemon_probe_tells_gui_born_from_ssh_and_job_born();
    a_path_with_a_space_or_a_quote_is_not_plain();
}

#[test]
fn a_dry_run_prints_the_steps_and_ssh_options_fence_the_host() {
    dry_run_prints_the_steps_and_runs_no_ssh();
    ssh_argv_puts_the_host_after_the_option_fence();
}

#[test]
fn each_step_family_builds_its_commands() {
    the_code_step_pushes_to_the_target_common_dir_then_adds_the_worktree();
    a_dirty_tree_syncs_with_rsync_and_leaves_its_pointer_and_target_alone();
    the_transcript_copy_carries_the_sidecar_and_makes_the_parent();
    the_cutover_registers_resumes_and_retires_by_name();
    a_verify_that_cannot_read_the_source_fails_at_verify();
}

#[test]
fn the_all_form_sorts_rows_and_names_what_a_failure_left() {
    all_sorts_workers_leads_and_skips();
    a_failed_all_names_the_workers_after_the_failure();
}
