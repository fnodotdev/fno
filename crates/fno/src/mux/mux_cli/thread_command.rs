//! The `fno mux thread` command: the outside-the-TUI portal reach. A child
//! module of `mux_cli` on purpose: the file-budget gate keeps the parent
//! shrink-only, and `use super::*` keeps every helper (`take_common_flags`,
//! `resolve_session`, the timeout constants, the exit codes, the proto
//! types, the selector parsers) in exactly one place.
use super::*;

/// The refusal of a thread split that names neither a tab nor a cell.
const THREAD_SPLIT_HELP: &str = "a --split needs an anchor: name its tab with --tab N, or \
its cell with --from <portal N|worker name|current>";

/// `fno mux thread <name> [--portal N|new] [--tab SEL] [--split DIR]
/// [--from CELL] [--workspace NAME] [--at PANE]` (hidden): the outside-the-TUI
/// reach behind `fno agent attach <name>`. Sends the ThreadPane control command,
/// which runs the exact command a TUI reach runs, and prints where it landed.
/// A missing server is its own exit code so the CLI caller can fall through to
/// the inline attach instead of reading a generic failure as one.
///
/// An explicit placement flag CREATES a portal, human or agent:
/// `--portal new`, or `--portal N` when N is not open. No flag tunes: the
/// reach focuses the row's open portal, and with none open the door's own
/// portal 0 opens for it (the TV rule; the operator's own portals never
/// multiply from a reach).
///
/// The placement flags reuse the pane path's spellings and ride the
/// command's `placement` field. A `--split DIR` halves, in this order: the
/// cell `--from` or `--at` names, the focused pane of the `--tab` named, the
/// caller's own pane (FNO_PANE). With none of them it is refused with both
/// flags named. They steer a FRESH open. A split beside an open `--portal N`
/// halves N and opens a new portal, so N keeps showing. With no split, a
/// portal that already has a live seat keeps its geometry (the server says so).
///
/// The hidden `--check` asks the server whether the reach would land and
/// changes nothing. A placement refusal exits 2. A server that cannot answer
/// exits 1, so a caller can tell the two apart.
pub fn thread(args: &[OsString], env_session: Option<&str>) -> i32 {
    let (session_flag, parsed) = match parse_thread_args(args) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_USAGE;
        }
    };
    let mut placement = PanePlacement::default();
    let mut portal: Option<u8> = None;
    if let Some(value) = &parsed.portal {
        // if/else, not a match: an inner `"word" =>` arm reads as a phantom
        // command to the Python parity parser's arm scan.
        let is_new = value == "new";
        if let Ok(n) = value.parse::<u8>() {
            portal = Some(n);
            // Last --portal flag wins, so an explicit index clears a `new`
            // spelled earlier.
            placement.portal_new = false;
        } else if is_new {
            placement.portal_new = true;
            portal = None;
        } else {
            eprintln!("fno mux thread: --portal takes an index 0-255 or new");
            return EXIT_USAGE;
        }
    }
    if let Some(ws) = &parsed.workspace {
        if ws.trim().is_empty() {
            eprintln!("fno mux thread: --workspace/-s needs a nonblank workspace name");
            return EXIT_USAGE;
        }
        placement.target = PaneTarget::SquadName(ws.clone());
    }
    if let Some(v) = &parsed.split {
        match parse_dir(v, "split/-x") {
            Ok(dir) => placement.split = Some(dir),
            Err(e) => {
                eprintln!("fno mux thread: {e}");
                return EXIT_USAGE;
            }
        }
    }
    if let Some(v) = &parsed.tab {
        match parse_tab_sel(v) {
            Ok(sel) => placement.tab = Some(sel),
            Err(e) => {
                eprintln!("fno mux thread: {e}");
                return EXIT_USAGE;
            }
        }
    }
    if let Some(v) = &parsed.at {
        if v == "current" {
            // `current` resolves a calling pane from FNO_PANE; this command's
            // caller is a control client with no pane of its own.
            eprintln!(
                "fno mux thread: --at takes a pane id; there is no calling \
                 pane to resolve `current` from"
            );
            return EXIT_USAGE;
        }
        match parse_u64(v, "--at") {
            Ok(at) => placement.at = Some(at),
            Err(e) => {
                eprintln!("fno mux thread: {e}");
                return EXIT_USAGE;
            }
        }
    }
    if let Some(v) = &parsed.from {
        if v == "current" {
            // `current` names the calling pane: resolve it HERE from
            // FNO_PANE, the way the spawn placement layer and pane_args
            // do. The literal never rides the wire - the server has no
            // calling pane to resolve it with.
            let fno_pane = std::env::var("FNO_PANE")
                .ok()
                .map(|s| s.trim().trim_start_matches('%').to_string())
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<u64>().ok());
            match fno_pane {
                Some(pane) => placement.at = Some(pane),
                None => {
                    eprintln!(
                        "fno mux thread: --from current needs a numeric \
                         FNO_PANE (run it inside a mux pane)"
                    );
                    return EXIT_USAGE;
                }
            }
        } else {
            placement.from = Some(v.clone());
        }
    }
    let tab_is_new = matches!(placement.tab, Some(proto::TabSel::New));
    if tab_is_new && placement.split.is_some() {
        eprintln!("fno mux thread: --tab new opens one pane, so drop --tab new or --split");
        return EXIT_USAGE;
    }
    // A named tab brings its focused pane as the anchor. Only a split with
    // no tab and no anchor falls back to the caller's own pane.
    match super::pane_args::anchor_or_refuse(
        placement.split.is_some() && placement.tab.is_none(),
        placement.at,
        placement.from.as_deref(),
        super::pane_args::pane_from_env(),
    ) {
        Ok(Some(pane)) => placement.at = Some(pane),
        Ok(None) => {}
        // An open portal N is its own anchor, and only the server knows.
        Err(_) if portal.is_some() => {}
        Err(_) => {
            eprintln!("fno mux thread: {THREAD_SPLIT_HELP}");
            return EXIT_USAGE;
        }
    }
    // A typed index rides the placement too. An absent one reads as portal 0
    // on the wire, and that default never asks for a split of portal 0.
    placement.portal = portal;
    let check = parsed.check;
    let name = parsed
        .name
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    // A check can run before the thread has a name.
    if name.is_empty() && !check {
        eprintln!("fno mux thread: needs an agent name or attach id");
        return EXIT_USAGE;
    }
    let origin = thread_origin(&name, check, parsed.cwd, parsed.parent);
    // A paneless row owns no session routing: the operator's ambient server
    // (the flag, FNO_SERVER / FNO_SESSION, or the default) is the one whose
    // portal this drives. Flag and env stay separate so resolve_session can
    // tell an env-decided server from a flag-decided one.
    let session = resolve_session(
        session_flag.as_deref().filter(|s| !s.is_empty()),
        env_session,
    );
    let sock = match proto::socket_path(&session) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fno mux thread: {e}");
            // Exit 2 under a check is the server's refusal, and none answered.
            return if check { EXIT_ERROR } else { EXIT_USAGE };
        }
    };
    let stream = match proto::connect_unix_timeout(&sock, PROBE_TIMEOUT) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fno mux thread: no live mux server ({e})");
            return EXIT_NO_SERVER;
        }
    };
    // An older server drops the origin and would act on a check.
    if check && !crate::mux_rows::wire_at_least(&session, proto::LAUNCH_PLACEMENT_PROTO) {
        eprintln!(
            "fno mux thread: server {session} is too old to check a placement; restart it \
             to get the check"
        );
        return EXIT_ERROR;
    }
    match send_control(
        stream,
        ControlCommand::ThreadPane {
            name,
            portal,
            placement,
            origin,
        },
        CONTROL_TIMEOUT,
        CONTROL_REPLY_DEADLINE,
        &session,
    ) {
        Ok(ServerMsg::Notice { text }) => {
            println!("{text}");
            EXIT_OK
        }
        Ok(ServerMsg::Err { code, msg }) => {
            eprintln!("fno mux thread: {msg}");
            if check && code == proto::err_code::BAD_REQUEST {
                EXIT_USAGE
            } else {
                EXIT_ERROR
            }
        }
        Ok(other) => {
            eprintln!("fno mux thread: unexpected reply: {other:?}");
            EXIT_ERROR
        }
        Err(ControlError::Unanswered(e)) => {
            eprintln!("fno mux thread: {e}");
            EXIT_CONTROL_UNANSWERED
        }
        Err(e) => {
            eprintln!("fno mux thread: {e}");
            EXIT_ERROR
        }
    }
}

/// Who asks, sent for a check and for a worker on another machine. Neither
/// has a row the server could read a workspace from.
fn thread_origin(
    name: &str,
    check: bool,
    cwd: Option<String>,
    parent: Option<String>,
) -> Option<proto::ThreadOrigin> {
    if !check && !proto::is_remote_address(name) {
        return None;
    }
    let cwd = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Some(proto::ThreadOrigin { cwd, parent, check })
}

/// The parse `thread` runs: the shared common flags ride [`MuxCommon::take`],
/// and the command's own grammar parses the REMAINDER. Parsing the original argv
/// instead would refuse `--server`/`--session`/`--json` - tokens ThreadArgs
/// does not declare - so the server-axis override every command shares would die
/// on this command. The returned error is the line the caller prints.
fn parse_thread_args(
    args: &[OsString],
) -> Result<(Option<String>, crate::cli_args::ThreadArgs), String> {
    let (common, rest) = MuxCommon::take(args).map_err(|e| format!("fno mux thread: {e}"))?;
    let parsed = crate::cli_args::ThreadArgs::try_parse_from(&rest)
        .map_err(|e| crate::cli_args::refusal_line("fno mux thread", &e))?;
    Ok((common.server.or(common.session), parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn thread_takes_the_shared_server_flags() {
        // The common flags are take()'s and the command grammar parses the
        // remainder; parsing the original argv instead refused every
        // --server/--session/--json invocation (they are not ThreadArgs').
        let (session_flag, parsed) =
            parse_thread_args(&os(&["--server", "work", "--json", "myagent"]))
                .expect("common flags parse");
        assert_eq!(session_flag.as_deref(), Some("work"));
        assert_eq!(parsed.name.as_deref(), Some("myagent"));
        let (session_flag, parsed) =
            parse_thread_args(&os(&["--session", "legacy", "myagent"])).expect("alias parses");
        assert_eq!(session_flag.as_deref(), Some("legacy"));
        assert_eq!(parsed.name.as_deref(), Some("myagent"));
    }
    #[test]
    fn a_split_with_no_anchor_halves_the_callers_pane_or_refuses() {
        use super::super::pane_args::anchor_or_refuse;
        // The default: split + no anchor + a calling pane = the caller's cell.
        assert_eq!(
            anchor_or_refuse(true, None, None, Some(7)).ok(),
            Some(Some(7))
        );
        // A caller with no pane names --from or is refused with the flag named.
        let err = anchor_or_refuse(true, None, None, None).expect_err("pane-less refuses");
        assert!(err.contains("--from"), "{err}");
        // No split, or an anchor already named: no default, no refusal.
        assert_eq!(anchor_or_refuse(false, None, None, None).ok(), Some(None));
        assert_eq!(anchor_or_refuse(true, Some(3), None, None).ok(), Some(None));
        assert_eq!(
            anchor_or_refuse(true, None, Some("portal 0"), None).ok(),
            Some(None)
        );
    }
}
