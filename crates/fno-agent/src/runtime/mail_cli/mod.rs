//! The `mail` front commands on the Rust leg: the do-not-disturb hold, its
//! release timer, the pane-send renderer, the session seed-provenance leaf,
//! the `team` broadcast, the outbound sent view, the withdraw leaf, the
//! unread listing, the ack leaf, the raw prompt-line send, the reply leaf,
//! the drain-self leaf, and the transcript backfill (the engine lives in
//! [crate::mail_backfill]). Dispatch only; the commands live in [hold],
//! [pane_prepare], [seed_provenance], [team], [sent], [withdraw], [unread],
//! [ack], [send_raw], [reply], and [drain_self].
pub mod ack;
pub mod drain_self;
pub mod hold;
pub mod pane_prepare;
pub mod reply;
pub mod seed_provenance;
pub mod send;
pub mod send_raw;
pub mod team;
pub mod unread;
pub mod withdraw;
pub mod you_pairs;

pub fn run(rest: &[String]) -> i32 {
    let first = rest.first().map(String::as_str);
    if rest.is_empty() || first == Some("--help") || first == Some("-h") {
        print_help();
        return if rest.is_empty() { 2 } else { 0 };
    }
    let help_asked = rest[1..]
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h");
    match rest[0].as_str() {
        "send" if help_asked => {
            print!("{SEND_HELP}");
            print_style_rules();
            0
        }
        "reply" if help_asked => {
            print!("{REPLY_HELP}");
            print_style_rules();
            0
        }
        "backfill" if help_asked => {
            print!("{BACKFILL_HELP}");
            0
        }
        "backfill" => {
            let mut argv: Vec<String> = Vec::with_capacity(rest.len());
            argv.push("run".to_string());
            argv.extend(rest.iter().skip(1).cloned());
            crate::mail_backfill::run_mail_backfill(&argv)
        }
        // Leaf-level help: the family listing answers for every leaf
        // without its own body; a hold or an ack never runs on a request.
        cmd if help_asked
            && matches!(
                cmd,
                "hold"
                    | "dnd"
                    | "hold-release"
                    | "seed-provenance"
                    | "team"
                    | "withdraw"
                    | "sent"
                    | "pane-prepare"
                    | "unread"
                    | "ack"
                    | "drain-self"
            ) =>
        {
            print_help();
            0
        }
        "hold" | "dnd" => hold::run_hold(&rest[1..]),
        "hold-release" => hold::run_hold_release(&rest[1..]),
        "seed-provenance" => seed_provenance::run(&rest[1..]),
        "team" => team::run_team(&rest[1..]),
        "withdraw" => withdraw::run(&rest[1..]),
        "sent" => crate::mail_sent_env::run_sent(rest.get(1..).unwrap_or_default()),
        "send" => send::run(&rest[1..]),
        "reply" => reply::run(&rest[1..]),
        "pane-prepare" => pane_prepare::run(&rest[1..]),
        "unread" => unread::run(&rest[1..]),
        "ack" => ack::run_ack(&rest[1..]),
        "drain-self" => drain_self::run_drain_self(&rest[1..]),
        other => {
            eprintln!(
                "fno-agent mail: unknown command {other:?} (hold | dnd | hold-release | seed-provenance | team | withdraw | backfill | sent | send | reply | pane-prepare | unread | ack | drain-self)"
            );

            2
        }
    }
}

/// The style rules the send gate enforces, stated where senders read: one
/// copy of the paragraph (STYLE_RULES), so help and lint cannot drift.
fn print_style_rules() {
    println!("\nStyle (checked before send):");
    println!("{}", crate::backlog::style_check::STYLE_RULES);
    println!("Preview a body: fno doctor lint style --surface mail --stdin");
}

const SEND_HELP: &str = "\
Usage: fno agent mail send [OPTIONS] [session-id] [message]

  Send a message to a registered agent or a project.

Arguments:
  session-id  Short id or full session id; a name still resolves. A
              codex head-8 is a 65.536-second UUIDv7 bucket, not random, and
              is refused against a codex row; claude ids are UUIDv4, so either
              form works.
  message     Message to send (async, fire-and-forget).

Options:
  -H, --harness <str>      claude | codex | gemini (optional; used for
                           mismatch check).
  -c, --cwd <str>          Working directory context.
  --from-name <str>        XML-safe sender. Unset: session handle or 'fno' for
                           agents; project for project sends.
  --from-self              Stamp the sender with this session's own canonical
                           mail handle (the reply handle `fno whoami` shows).
                           Fails loud (exit 2) with no ambient harness
                           identity.
  --to-project <str>       Anycast: deliver to whoever works on this project
                           (live if exactly one peer; refuses if none).
                           Use instead of <name>.
  --to-lead <str>          Anycast over the role: deliver to whoever holds
                           this scope RIGHT NOW, resolved at send time; no
                           live holder queues nothing.
  --any                    With --to-project, break a multi-live-peer tie
                           (most recent activity wins).
  -k, --kind <str>         Retired: the note lane is gone. A plain send reaches
                           a session; `fno inbox outstanding ask` reaches a
                           human. The flag exits 2 and names both.
  -b, --body <str>         Message body in ANY mode (alternative to the
                           positional arg).
  --body-file <path>       Read the body from a file ('-' = stdin); binds in
                           every mode.
  --ref-pr <int>           With --kind: PR number reference for triage.
  --ref-node <str>         With --kind: graph node id reference.
  --ref-gate <str>         With --kind: named gate/milestone reference.
  -J, --json               With --kind: print {msg_id, thread_path, appended}
                           as JSON.
  --raw                    Inject the payload UNWRAPPED at the recipient's
                           prompt line: one line, typed verbatim - a command
                           only: it must start with / or $. A status report or
                           any authored message goes WRAPPED (drop --raw) so
                           its sender stays visible. A payload starting /fno:
                           or $fno: is an fno command, and the lane rewrites
                           the marker to the form of the receiving harness. A
                           showing prompt is answered with `fno agent ask`.
                           Never queues durable. An actor OTHER than the model
                           must supply the trigger; self-injection is barred
                           unless --to-self. Mechanics and the reviewer-off-
                           the-author rationale: docs/architecture/review-
                           lanes.md.
  --check                  With --raw: report whether an injection path EXISTS
                           and inject nothing: 'injectable: <lane>' (exit 0),
                           'not-injectable' (1), 'unmeasurable' (3), malformed
                           payload (2). Reports a PATH only.
  --to-self                Address this session as the recipient (no <id>
                           needed); with --raw the envelope is stripped so a
                           slash command parses at your own prompt line.
  -F, --force              Deliver over the PANE transport: type the wrapped
                           body at the recipient's prompt, keeping every mail
                           semantic. The receipt says `typed (pane <id>)`,
                           NEVER `delivered` - bytes written to a PTY is not
                           action. It reads the pane first and refuses a
                           showing prompt.
  --style-exception <str>  Bypass the style check for this body with a stated
                           reason.
  --steer                  Answer posture: the claude inject types the
                           body as a TRUE single line, which a mid-turn
                           recipient absorbs beside its next tool result
                           (absorbed_mid_turn), instead of the multi-line form
                           the harness queue parks until a turn boundary. The
                           questions lane passes it on every answer delivery.
  --help                   Show this message and exit.
";

const REPLY_HELP: &str = "\
Usage: fno agent mail reply --to MSG_ID [OPTIONS] [BODY]

  Reply to a message, routed by the answered message's lane.

Options:
  --to <id>                The message being answered (a leading positional
                           msg-id works too).
  --body <str>             Reply body (alternative to the positional arg).
  --body-file <path>       Read the body from a file ('-' = stdin).
  --kind <str>             Inbox kind of the reply.
  --from <project>         Sender project.
  --sender-session <id>    Sender session.
  --style-exception <str>  Bypass the style check for this body with a stated
                           reason.
  -J, --json               Print the receipt as JSON.
  --help                   Show this message and exit.
";

const BACKFILL_HELP: &str = "\
Usage: fno agent mail backfill [--all | --since T --until T] [--apply]
                               [--root DIR]... [--live PATH]

  Rebuild past cross-session mail: scan Claude Code transcripts for
  SendMessage calls and their receiver-side blocks, join each send to its
  landing by body attribution, and (with --apply) append the pair to the
  bus log as a durable audit-only row. Without --apply nothing is written.

  The default window is the one mail-send outage. --all scans every
  transcript, no window; it refuses --since/--until.

Options:
  --all            Scan all history; refuses --since/--until.
  --since <ts>     Window start (default: the outage start).
  --until <ts>     Window end (default: the outage end).
  --apply          Write the joined pairs; default is a dry run.
  --root <dir>     A transcript tree to scan (repeatable; default: the
                   Claude projects home).
  --live <path>    The bus log to write (default: the resolved live path).
  --help           Show this message and exit.
";

fn print_help() {
    println!("fno-agent mail - the do-not-disturb hold, team broadcast, sent view, withdraw, backfill, unread listing, ack, raw prompt-line send, reply, and drain-self for agent mail");
    println!("  hold [--minutes N | --for N] [--status] [--off]   alias: dnd");
    println!("  hold-release --handle KEY [--poll-s S]");
    println!("  backfill [--all | --since T --until T] [--apply] [--root DIR]... [--live PATH]");
    println!("           every past SendMessage its receiver block joins, dry run by default");
    println!("  seed-provenance                                 payload from FNO_SEED_PROV_* env");
    println!("  team --scope <all|leads|<role>|project:<p>> <message>");
    println!("  withdraw MSG_ID [--from-name NAME]");
    println!("  sent [--unclaimed] [--json] [--from-name LABEL]");
    println!("  pane-prepare --session-id S --pane N [--harness H] [--style-exception R]");
    println!("  send [--check] [--self-ok] [--review-request] [--style-exception <r>] [--origin <o>] <name> <payload>   raw prompt-line lane");
    println!(
        "  reply --to MSG_ID [BODY]                        routed by the answered message's lane"
    );
    println!("  unread [--name NAME | -n NAME] [--json | -J]");
    println!("  ack MSG_ID [--name NAME]                        alias front: fno mail ack");
    println!("  drain-self [--json]");
}
