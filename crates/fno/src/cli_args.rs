//! Typed classification for the `fno` front door.
//!
//! One clap declaration set owns the native-vs-forward boundary and the
//! already-typed native commands; the carry-verbatim mux families (pane, block,
//! tab, layout, rows, where, thread, reseat, retire-session, view, workspace,
//! serve, web) still parse inside `mux_cli` until their cutover wave, so
//! `MuxCmd::Other` carries their argv byte-verbatim. The classifier keeps
//! Footnote's explicit compatibility rules at the command edge: the Locked-7
//! leading `--server`/`--session` pair, the socket-path role for a `--server`
//! value containing `/`, and the deprecation note that names the flag.

use std::ffi::OsString;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

/// What the front door decided to do with one invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum FrontDoor {
    /// Attach (spawn the server if absent). `Some(name)` pins the session;
    /// `explicit_socket` marks a `--server` value containing `/` (the
    /// internal role `client.rs` spawns with an absolute path).
    Attach {
        name: Option<String>,
        explicit_socket: bool,
    },
    /// `fno version [--json]`: the mux self-report.
    Version { json: bool },
    /// `fno uninstall [--yes] [--dry-run] [--purge]`.
    Uninstall(crate::uninstall::Opts),
    /// A native mux invocation, typed as far as this wave cutover reaches.
    Mux(MuxParsed),
    /// Native-shaped but malformed: `message` prints on stderr, exit 2. The
    /// text is either clap's rendered help (an explicit help request on a
    /// group) or one command-qualified refusal line naming the bad token.
    Usage { message: String },
    /// An explicit `--help`/`-h` the typed tree resolved: the rendered help
    /// prints on stdout, exit 0.
    Help { message: String },
    /// `fno --help` / `fno -h`: the root menu, read from command-names.tsv.
    /// `all` adds the retired names with their new spellings.
    RootHelp { all: bool },
    /// `fno --version` / `fno -V`: `fno <crate version>`.
    RootVersion,
}

/// The `mux` subcommand's typed parse.
#[derive(Debug, PartialEq, Eq)]
pub struct MuxParsed {
    pub cmd: MuxCmd,
}

/// The mux commands typed in this wave, plus the carry-verbatim `Other` arm the
/// later mux wave consumes.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum MuxCmd {
    /// `mux server [--server <n>]`: run the mux server for a session; `--session`
    /// is the deprecated `--server` spelling and warns.
    Server(ServerArgs),
    /// `mux attach <name>`
    Attach { name: String },
    /// `mux ls [--json]`
    Ls {
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux doctor [--json]`
    Doctor {
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux diag stats [--json]` (hidden, read-only)
    #[command(hide = true)]
    Diag {
        #[command(subcommand)]
        op: DiagOp,
    },
    /// `mux notify <title> <message> [--json]` (hidden): one desktop
    /// notification through every attached client's tty; the receipt's
    /// `clients` count is the delivery verdict (0 = none took it, so the
    /// caller - `fno notify` - falls back to osascript).
    #[command(hide = true)]
    Notify {
        title: String,
        message: String,
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux kill-server [<name>] [--end-unkept [--yes] | --stale-idle | --all] [--json]`
    KillServer {
        /// Optional server name (default resolution: flag > env > default)
        name: Option<String>,
        #[command(flatten)]
        json: JsonOnly,
        /// End live panes no keeper holds (otherwise a kill refuses)
        #[arg(long)]
        end_unkept: bool,
        /// Confirm the portals and attached clients an --end-unkept kill closes
        #[arg(long, requires = "end_unkept")]
        yes: bool,
        /// Kill every live stale-wire session hosting no live pane
        #[arg(long)]
        stale_idle: bool,
        /// Kill every live session
        #[arg(long)]
        all: bool,
    },
    /// `mux shell-init [<shell>] [--json]`
    ShellInit {
        /// Shell to print the snippet for (the command validates the spelling)
        shell: Option<String>,
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux command <selector> --text <native-command> --proof <kind>`
    Command(MuxCommandArgs),
    /// `mux serve --web` opens one session read-only in a browser; `--stop`
    /// ends it, `--status` reads its state.
    /// `mux serve --share --pane <id> [--steer] [--tunnel]` shares one masked
    /// pane with viewers you admit.
    /// `mux serve --observe --pane <id> [--name <label>]` watches one masked
    /// pane read-only in this terminal.
    /// `mux serve --link <key> [--host <host>]` opens one session live, with
    /// input, in this terminal; `--host` dials its machine.
    #[command(disable_help_flag = true)]
    Serve(MuxTail),
    /// `mux web reap [--json]` removes the web-serve records of servers that
    /// are gone. A bare `mux web` refuses: name the operation.
    Web {
        #[command(subcommand)]
        op: WebOp,
    },
    /// `mux pane <command> ...`: the pane commands for scripts and agents:
    /// list, read, run, send, wait, kill, claim, release, split, break, focus.
    Pane {
        #[command(subcommand)]
        op: PaneOp,
    },
    /// `mux block pipe|annotate ...`: the block commands. A bare
    /// `mux block` refuses: name the operation.
    Block {
        #[command(subcommand)]
        op: BlockOp,
    },
    /// `mux tab ls|create|rename|join|move|close ...`: list and arrange tabs
    /// in the session.
    Tab {
        #[command(subcommand)]
        op: TabOp,
    },
    /// `mux layout get|apply|graft ...`: read, apply, and graft nested layout
    /// trees. The shared flags (`--server`, `--json`) may sit before the
    /// operation word (`mux layout --json apply ...`).
    Layout {
        #[command(flatten)]
        common: MuxCommon,
        #[command(subcommand)]
        op: LayoutOp,
    },
    /// `mux where <fno_id-or-tab>`: resolve an fno session id or tab to its
    /// location. The selector is required; a bare `mux where`
    /// refuses.
    #[command(disable_help_flag = true)]
    Where(MuxTailReq),
    /// `mux thread <name> [--portal N]` (hidden): show a thread
    /// row through a portal; `mux thread reseat` (v72) moves a live
    /// pane-hosted worker into a portal seat instead.
    #[command(hide = true)]
    Thread {
        #[command(subcommand)]
        op: ThreadOp,
    },
    /// `mux retire-session ...`: the exact-session retirement door
    /// (v75, hidden). The selector is required.
    #[command(hide = true, disable_help_flag = true)]
    RetireSession(MuxTailReq),
    /// `mux view <selector> [--url] [--fzf] [--json]`: point your view at
    /// the pane hosting an agent. The selector is required.
    #[command(disable_help_flag = true)]
    View(MuxTailReq),
    /// `mux workspace prune|restore|origin ...`: prune, restore, or read the
    /// workspace store. A bare `mux workspace` refuses: name the
    /// operation.
    Workspace {
        #[command(subcommand)]
        op: WorkspaceOp,
    },
    /// `mux app open|show|focus|answer|shot|state|close ...`: drive the fno-app
    /// desktop app from scripts and agents. A bare `mux app` refuses with
    /// usage; the full guide prints at `fno whoami guide app`.
    App {
        #[command(subcommand)]
        op: AppOp,
    },
}

/// The identity-pinned native action door. Keep this declaration typed so the
/// flag registry, help, and front-door classifier all share one contract.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct MuxCommandArgs {
    /// Agent name, node id, or full harness session id.
    pub selector: String,
    /// Native command to execute, for example `/compact` or `/rc`.
    #[arg(long)]
    pub text: String,
    /// Postcondition recipe: compact, goal-active, or screen.
    #[arg(long, value_parser = ["compact", "goal-active", "screen"])]
    pub proof: String,
    /// Optional bounded screen assertion for the screen recipe.
    #[arg(long)]
    pub expect: Option<String>,
    /// Regex that must positively identify an empty composer before typing.
    #[arg(long)]
    pub empty_composer: Option<String>,
    /// Maximum seconds spent waiting for the command-specific proof.
    #[arg(long, default_value_t = 30)]
    pub timeout_seconds: u64,
    /// Stable id used to make retries idempotent.
    #[arg(long)]
    pub request_id: Option<String>,
}

/// The kill-server request the role carries: a NAME with the break-glass
/// flags, or one selector over the whole session table.
#[derive(Debug, PartialEq, Eq)]
pub struct KillRequest {
    pub name: Option<String>,
    pub json: bool,
    pub end_unkept: bool,
    pub yes: bool,
    pub stale_idle: bool,
    pub all: bool,
}

impl KillRequest {
    /// The named form with every selector off.
    pub fn simple(name: Option<&str>, json: bool) -> Self {
        KillRequest {
            name: name.map(str::to_string),
            json,
            end_unkept: false,
            yes: false,
            stale_idle: false,
            all: false,
        }
    }
}

/// `mux block`'s operation set.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum BlockOp {
    /// `mux block pipe`: pipe a completed block into another pane's input
    #[command(disable_help_flag = true)]
    Pipe(MuxTail),
    /// `mux block annotate`: record a completed block as an operator finding
    #[command(disable_help_flag = true)]
    Annotate(MuxTail),
}

/// `mux tab`'s operation set.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum TabOp {
    /// List tabs
    #[command(disable_help_flag = true)]
    Ls(MuxTail),
    /// Create a tab
    #[command(disable_help_flag = true)]
    Create(MuxTail),
    /// Rename a tab
    #[command(disable_help_flag = true)]
    Rename(MuxTail),
    /// Join a tab to another tab
    #[command(disable_help_flag = true)]
    Join(MuxTail),
    /// Move a tab to a new position
    #[command(disable_help_flag = true)]
    Move(MuxTail),
    /// Close a tab
    #[command(disable_help_flag = true)]
    Close(MuxTail),
}

/// `mux layout`'s operation set. The re-sliced tail keeps the operation
/// word in place (`mux layout --json apply spec.toml` slices to
/// `["--json","apply","spec.toml"]`), because a common flag may sit
/// before the command and [`MuxCommon::take`] strips it from any position.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum LayoutOp {
    /// Dump the nested layout tree + geometry (the default read)
    #[command(disable_help_flag = true)]
    Get(MuxTail),
    /// Apply a layout spec (--template/--slot or a .toml spec file)
    #[command(disable_help_flag = true)]
    Apply(MuxTail),
    /// Graft a spec onto a fresh tab anchored at a pane
    #[command(disable_help_flag = true)]
    Graft(MuxTail),
}

/// `mux workspace`'s operation set.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum WorkspaceOp {
    /// Reap dead-origin residue from the workspace store
    #[command(disable_help_flag = true)]
    Prune(MuxTail),
    /// Bring the stored workspaces' worker members back
    #[command(disable_help_flag = true)]
    Restore(MuxTail),
    /// Add or drop a workspace's folder: `origin add|remove <name> <path>`
    #[command(disable_help_flag = true)]
    Origin(MuxTailReq),
}

/// `mux app`'s operation set over the fno-app desktop app: an agent opens,
/// drives, screenshots, inspects and closes the app. A bare `mux app`
/// refuses with usage naming the commands; the full guide prints at
/// `fno whoami guide app`.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum AppOp {
    /// `mux app open [--mode <view>] [--focus <agent>] [--server <name>]`:
    /// launch fno-app attached to the session and print its pid
    Open(AppOpenArgs),
    /// `mux app show <view> [--server <name>]`: point the running app at a
    /// view, or open it there when none runs
    Show(AppShowArgs),
    /// `mux app focus <agent> [--server <name>]`: focus one agent row in
    /// the running app; refuses naming the session when none runs
    Focus(AppFocusArgs),
    /// `mux app answer <option> [--server <name>]`: pick option N (1 is
    /// the first) on the question the focused chat waits on; refuses
    /// naming the session when none runs
    Answer(AppAnswerArgs),
    /// `mux app approve <pr> [--repo <slug>]`: post the person's GitHub
    /// APPROVED review from the terminal; the satisfier of
    /// config.review.human = ui. Works with the app closed.
    Approve(AppApproveArgs),
    /// `mux app shot --out <path> [--theme dark|light] [--mode <view>]
    /// [--find bar|chords] [--ask rows|overflow] [--notice]`: render one offscreen
    /// demo frame and write a
    /// png; never opens a window on any display
    Shot(AppShotArgs),
    /// `mux app state [--json]`: read the app's saved window state
    State {
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux app close [--server <name>]`: ask the running app to quit;
    /// exits 0 when none runs
    Close {
        #[command(flatten)]
        common: MuxCommon,
    },
}

/// The views `mux app open --mode` and `mux app show` name. Kebab-case
/// spellings mirror `fno_app::MainView`, which the app parses on launch.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppView {
    /// The focused agent's conversation
    Chat,
    /// The server's panes
    Terminal,
    /// The command center board
    Orchestration,
    /// The mods and layouts feed
    Mods,
    /// The questions and decisions that wait on the user
    Inbox,
}

impl AppView {
    /// The kebab-case spelling the app's `--mode` flag parses.
    pub fn as_spelling(&self) -> &'static str {
        match self {
            AppView::Chat => "chat",
            AppView::Terminal => "terminal",
            AppView::Orchestration => "orchestration",
            AppView::Mods => "mods",
            AppView::Inbox => "inbox",
        }
    }
}

/// `mux diag`'s operation set: the hidden machine-read doors, folded
/// under one group so the mux level-3 menu stays at its seeded size.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum DiagOp {
    /// `mux diag stats [--json]`: the mux counters (hidden, read-only)
    #[command(hide = true)]
    Stats {
        #[command(flatten)]
        json: JsonOnly,
    },
    /// `mux diag rows [--json] ...`: the one row-set receipt (hidden)
    #[command(disable_help_flag = true, hide = true)]
    Rows(MuxTail),
}

/// `mux app open`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppOpenArgs {
    /// View the window opens on (default: the saved view, else chat)
    #[arg(long, value_enum)]
    pub mode: Option<AppView>,
    /// Agent row the window focuses at launch
    #[arg(long)]
    pub focus: Option<String>,
    #[command(flatten)]
    pub common: MuxCommon,
}

/// `mux app show`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppShowArgs {
    /// The view to switch to
    pub view: AppView,
    #[command(flatten)]
    pub common: MuxCommon,
}

/// `mux app answer`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppAnswerArgs {
    /// The option to pick, counted from 1
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub option: u32,
    #[command(flatten)]
    pub common: MuxCommon,
}

/// `mux app approve`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppApproveArgs {
    /// The PR number to approve
    pub pr: u64,
    /// The owner/name slug when the cwd is not the repo checkout
    #[arg(long)]
    pub repo: Option<String>,
}

/// `mux app focus`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppFocusArgs {
    /// The agent row to focus (name, node id or session id)
    pub agent: String,
    #[command(flatten)]
    pub common: MuxCommon,
}

/// `mux app shot`'s flags.
#[derive(Args, Debug, PartialEq, Eq, Clone)]
pub struct AppShotArgs {
    /// png path to write
    #[arg(long)]
    pub out: String,
    /// Palette the frame paints: dark (default) or light
    #[arg(long, value_parser = ["dark", "light"], default_value = "dark")]
    pub theme: String,
    /// View to render (default: the command center board)
    #[arg(long, value_enum)]
    pub mode: Option<AppView>,
    /// Open the answer sheet on the newest question
    #[arg(long)]
    pub sheet: bool,
    /// Paint with cmd held: rows and tabs show their digit badges
    #[arg(long)]
    pub cmd: bool,
    /// Find-bar state the frame paints over the chat view: `bar` opens
    /// the bar with a query typed; `chords` also holds the cmd hint
    #[arg(long, value_parser = ["bar", "chords"])]
    pub find: Option<String>,
    /// Window-relative X,Y in logical points the frame clicks through a
    /// posted mouse event before the capture
    #[arg(long, value_name = "X,Y")]
    pub tap: Option<String>,
    /// Seed an open ask-the-user question in the chat: `rows` is the short
    /// options frame, `overflow` makes the first option wrap
    #[arg(long, value_parser = ["rows", "overflow"])]
    pub ask: Option<String>,
    /// End the seeded chat on a raw task-notification row, which the chat
    /// folds to one caret line
    #[arg(long)]
    pub notice: bool,
    /// Open one overlay through its real code path before the capture:
    /// the row menu (`row-menu-node` gives the agent a node first), the
    /// in-tab rename field, the scrolled-back pill, or the demo row's
    /// read-only PR sheet
    #[arg(long, value_parser = ["row-menu", "row-menu-node", "tab-rename", "scrolled", "pr-sheet"])]
    pub ui: Option<String>,
}

/// `mux thread`'s operation set: `reseat` is the one declared operation;
/// anything else is the row's name or attach id (external arm).
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum ThreadOp {
    /// `mux thread reseat <agent-name|pane-id> [--portal N]`: move a live
    /// pane-hosted worker into a portal seat, keeping its PTY
    #[command(hide = true, disable_help_flag = true)]
    Reseat(MuxTailReq),
    /// A thread's name or attach id: `mux thread <name> [--portal N]`
    #[command(external_subcommand, hide = true)]
    Name(Vec<OsString>),
}

impl ThreadOp {
    /// The re-sliced family argv (see `mux_with_tail`).
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            ThreadOp::Reseat(MuxTailReq { tail }) => tail.clone(),
            ThreadOp::Name(words) => words.clone(),
        }
    }
}

/// The untyped family tail: declared ONLY so clap swallows the family's
/// own argv without parsing it. The dispatch tail is re-sliced from the
/// original argv (`mux_with_tail`); this captured copy is never read.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct MuxTail {
    /// The family's own argv, byte-verbatim
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub tail: Vec<OsString>,
}

/// [`MuxTail`] with at least one token: the leaves whose bare form is the
/// usage refusal (`mux where`, `mux view`, `mux retire-session`,
/// `mux thread reseat`).
#[derive(Args, Debug, PartialEq, Eq)]
pub struct MuxTailReq {
    /// The family's own argv, byte-verbatim (at least one token)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub tail: Vec<OsString>,
}

impl PaneOp {
    /// The re-sliced family argv (see `mux_with_tail`).
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            PaneOp::Ls(t)
            | PaneOp::Read(t)
            | PaneOp::Run(t)
            | PaneOp::Send(t)
            | PaneOp::Wait(t)
            | PaneOp::Kill(t)
            | PaneOp::Claim(t)
            | PaneOp::Release(t)
            | PaneOp::Split(t)
            | PaneOp::Break(t)
            | PaneOp::Focus(t)
            | PaneOp::Keeper {
                op: KeeperOp::List(t),
            } => t.tail.clone(),
        }
    }

    /// Same operation, dispatch tail re-sliced (see `mux_with_tail`).
    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            PaneOp::Ls(_) => PaneOp::Ls(t),
            PaneOp::Read(_) => PaneOp::Read(t),
            PaneOp::Run(_) => PaneOp::Run(t),
            PaneOp::Send(_) => PaneOp::Send(t),
            PaneOp::Wait(_) => PaneOp::Wait(t),
            PaneOp::Kill(_) => PaneOp::Kill(t),
            PaneOp::Claim(_) => PaneOp::Claim(t),
            PaneOp::Release(_) => PaneOp::Release(t),
            PaneOp::Split(_) => PaneOp::Split(t),
            PaneOp::Break(_) => PaneOp::Break(t),
            PaneOp::Focus(_) => PaneOp::Focus(t),
            PaneOp::Keeper { .. } => unreachable!(),
        }
    }
}

impl BlockOp {
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            BlockOp::Pipe(t) | BlockOp::Annotate(t) => t.tail.clone(),
        }
    }

    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            BlockOp::Pipe(_) => BlockOp::Pipe(t),
            BlockOp::Annotate(_) => BlockOp::Annotate(t),
        }
    }
}

impl TabOp {
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            TabOp::Ls(t)
            | TabOp::Create(t)
            | TabOp::Rename(t)
            | TabOp::Join(t)
            | TabOp::Move(t)
            | TabOp::Close(t) => t.tail.clone(),
        }
    }

    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            TabOp::Ls(_) => TabOp::Ls(t),
            TabOp::Create(_) => TabOp::Create(t),
            TabOp::Rename(_) => TabOp::Rename(t),
            TabOp::Join(_) => TabOp::Join(t),
            TabOp::Move(_) => TabOp::Move(t),
            TabOp::Close(_) => TabOp::Close(t),
        }
    }
}

impl LayoutOp {
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            LayoutOp::Get(t) | LayoutOp::Apply(t) | LayoutOp::Graft(t) => t.tail.clone(),
        }
    }

    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            LayoutOp::Get(_) => LayoutOp::Get(t),
            LayoutOp::Apply(_) => LayoutOp::Apply(t),
            LayoutOp::Graft(_) => LayoutOp::Graft(t),
        }
    }
}

impl WorkspaceOp {
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            WorkspaceOp::Prune(t) | WorkspaceOp::Restore(t) => t.tail.clone(),
            WorkspaceOp::Origin(t) => t.tail.clone(),
        }
    }

    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            WorkspaceOp::Prune(_) => WorkspaceOp::Prune(t),
            WorkspaceOp::Restore(_) => WorkspaceOp::Restore(t),
            WorkspaceOp::Origin(_) => WorkspaceOp::Origin(MuxTailReq { tail: t.tail }),
        }
    }
}

impl WebOp {
    pub fn tail(&self) -> Vec<OsString> {
        match self {
            WebOp::Reap(t) | WebOp::InstallAgent(t) | WebOp::UninstallAgent(t) => t.tail.clone(),
        }
    }

    fn with_tail(self, t: MuxTail) -> Self {
        match self {
            WebOp::Reap(_) => WebOp::Reap(t),
            WebOp::InstallAgent(_) => WebOp::InstallAgent(t),
            WebOp::UninstallAgent(_) => WebOp::UninstallAgent(t),
        }
    }
}

/// The root declaration set: the leading attach pair plus the subcommands.
#[derive(Parser, Debug)]
#[command(name = "fno", disable_version_flag = true)]
pub struct FnoRoot {
    /// Attach to (or spawn) the named server; a value containing `/` names an explicit socket path (internal role)
    #[arg(long, value_name = "NAME", conflicts_with = "session")]
    pub server: Option<String>,
    /// Deprecated spelling of --server (warns; alias removed in a future release)
    #[arg(long, hide = true, value_name = "NAME")]
    pub session: Option<String>,
    #[command(subcommand)]
    pub cmd: Option<RootCmd>,
}

/// The root subcommand set.
#[derive(Subcommand, Debug)]
pub enum RootCmd {
    /// The native terminal multiplexer surface
    Mux(MuxRoot),
    /// Print the mux binary's baked-in build rev
    Version {
        /// Emit one JSON object on stdout
        #[arg(long)]
        json: bool,
    },
    /// Remove the fno plugins, binaries, launchd agents and harness hooks
    #[command(disable_help_flag = true)]
    Uninstall(crate::uninstall::Opts),
    /// Unclaimed argv: the forwarded Python surface, byte-verbatim.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

/// The mux subcommand's arg set.
#[derive(Args, Debug)]
pub struct MuxRoot {
    #[command(subcommand)]
    pub cmd: MuxCmd,
}

/// `mux web`'s operation set.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum WebOp {
    /// `mux web reap [--json]`: remove every bridge marker whose recorded pid is gone or whose port refuses
    #[command(disable_help_flag = true)]
    Reap(MuxTail),
    /// `mux web install-agent [--bin <path>] [--port N] [--attention-port N] [--json]`: install the launchd agent that keeps the web bridge and answer endpoint up across reboots
    #[command(disable_help_flag = true)]
    InstallAgent(MuxTail),
    /// `mux web uninstall-agent [--json]`: unload the web-bridge launchd agent and remove its plist
    #[command(disable_help_flag = true)]
    UninstallAgent(MuxTail),
}

/// `mux pane`'s operation set (the order today's dispatcher taught, `keeper`
/// last). Every leaf carries its family argv untyped.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum PaneOp {
    /// List panes (`--fno-id <id>` filters by session id)
    #[command(disable_help_flag = true)]
    Ls(MuxTail),
    /// Print a pane's screen (all of it, or --lines / --block)
    #[command(disable_help_flag = true)]
    Read(MuxTail),
    /// Run a command in a new pane (the payload after -- is the command's own)
    #[command(disable_help_flag = true)]
    Run(MuxTail),
    /// Send text or keys to a pane
    #[command(disable_help_flag = true)]
    Send(MuxTail),
    /// Wait until a pane is idle, matches a pattern, or times out
    #[command(disable_help_flag = true)]
    Wait(MuxTail),
    /// Kill a pane (use --expect-child-pid <pid> to guard against replacement)
    #[command(disable_help_flag = true)]
    Kill(MuxTail),
    /// Claim a pane's writer lock
    #[command(disable_help_flag = true)]
    Claim(MuxTail),
    /// Release a pane's writer lock
    #[command(disable_help_flag = true)]
    Release(MuxTail),
    /// Split a pane (--direction is required)
    #[command(disable_help_flag = true)]
    Split(MuxTail),
    /// Break a pane out into its own named tab
    #[command(disable_help_flag = true)]
    Break(MuxTail),
    /// Focus a pane, a registry selector, or --fzf for the picker
    #[command(disable_help_flag = true)]
    Focus(MuxTail),
    /// Keeper-survival reads (direct socket scan, no server)
    #[command(hide = true)]
    Keeper {
        #[command(subcommand)]
        op: KeeperOp,
    },
}

/// `mux pane keeper`'s operation set.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum KeeperOp {
    /// List the keeper sockets directly (survives the server's death)
    #[command(disable_help_flag = true)]
    List(MuxTail),
}

/// Shared machine-output flag group for the scriptable no-positional commands
/// (contracts build on this): one declaration, `-J` as the short alias.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct JsonOnly {
    /// Emit machine-readable JSON on stdout
    #[arg(short = 'J', long)]
    pub json: bool,
}

/// `mux server`'s flags.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct ServerArgs {
    /// Session name to serve
    #[arg(long, value_name = "NAME", conflicts_with = "session")]
    pub server: Option<String>,
    /// Deprecated spelling of --server (warns)
    #[arg(long, hide = true, value_name = "NAME")]
    pub session: Option<String>,
}

/// Classify a full argv (post-argv[0]) into a front-door role.
///
/// Only a claimed first token (`mux`, `version`, `--server`, `--session`)
/// enters the parser; everything else IS the forwarded Python surface,
/// byte-verbatim (AC1-FORWARD). A claimed shape that fails to parse is
/// usage, exit 2. The mapping reads the raw `ArgMatches` (not the derive's
/// typed layer) on purpose: the typed layer misroutes subcommand capture
/// once the enum also carries a non-subcommand arm.
/// A help request that rides the root's own flags (`--server x --help`):
/// every word is a root flag, a flag value or the help request itself, and
/// the help flag is present. `Some(all)` when the argv asks the root menu.
fn root_flag_help(args: &[OsString]) -> Option<bool> {
    let mut all = false;
    let mut help = false;
    let words = args
        .iter()
        .map(|a| a.to_str())
        .collect::<Option<Vec<_>>>()?;
    let mut it = words.iter();
    while let Some(&w) = it.next() {
        match w {
            "--server" | "--session" => {
                it.next();
            }
            "--help" | "-h" => {
                help = true;
            }
            "--all" | "-A" => {
                all = true;
            }
            w if w.starts_with("--server=") || w.starts_with("--session=") => {}
            "-J" | "--json" => {}
            _ => return None,
        }
    }
    help.then_some(all)
}

pub fn classify(args: &[OsString]) -> FrontDoor {
    match args.first().and_then(|a| a.to_str()) {
        Some("--help" | "-h") => {
            return FrontDoor::RootHelp {
                all: args.iter().skip(1).any(|a| a == "--all" || a == "-A"),
            }
        }
        Some("--version" | "-V") if args.len() == 1 => return FrontDoor::RootVersion,
        Some(first) if first.starts_with('-') => {
            // Root flags before a help request (`fno --server x --help`) ask
            // the root menu too: the root carries no clap help flag, because
            // a derive disable there propagates into every subcommand.
            if let Some(all) = root_flag_help(args) {
                return FrontDoor::RootHelp { all };
            }
        }
        _ => {}
    }
    match front_command()
        .no_binary_name(true)
        .try_get_matches_from(args)
    {
        Ok(m) => match FnoRoot::from_arg_matches(&m) {
            Ok(root) => front_door_from(root, args),
            Err(_) => FrontDoor::Usage {
                message: "fno: unclassifiable native invocation".into(),
            },
        },
        Err(e) => {
            if !native_first(args) {
                return unclaimed(args);
            }
            // `fno mux app` with no operation word: clap's missing-subcommand
            // error names none of the commands, and the group's guide lives
            // behind the `fno whoami guide app` topic, so this refusal is
            // the group's one self-teaching surface. The derive arms every
            // required subcommand field with arg_required_else_help, so the
            // bare group arrives as the help-shaped kind, not MissingSubcommand.
            if path_words(args).as_slice() == ["mux", "app"]
                && matches!(
                    e.kind(),
                    clap::error::ErrorKind::MissingSubcommand
                        | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                )
            {
                return FrontDoor::Usage {
                    message: crate::mux::app_cli::bare_usage(),
                };
            }
            // An explicit help request the tree resolved (mux groups and
            // leaves, version): the rendered help answers on stdout, exit 0.
            // The mux thread group keeps its flag off, so its `-h` arrives as
            // a parse error and its arm above keeps the authored body.
            if e.kind() == clap::error::ErrorKind::DisplayHelp {
                return FrontDoor::Help {
                    message: e.render().to_string(),
                };
            }
            // One command-qualified line naming the bad token; the
            // deepest declared path becomes the prefix.
            let prefix = format!(
                "fno{}",
                path_words(args)
                    .iter()
                    .fold(String::new(), |a, w| format!("{a} {w}"))
            );
            FrontDoor::Usage {
                message: refusal_line(&prefix, &e),
            }
        }
    }
}

/// The refusal for an argv no classifier claims: name the words and the help
/// that lists what does answer.
fn unclaimed(args: &[OsString]) -> FrontDoor {
    let words: Vec<String> = args
        .iter()
        .take(2)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let message = match words.as_slice() {
        // Only a live root is a group worth pointing at; an unknown first
        // word would send the caller to `fno <typo> --help`, which refuses too.
        [root, rest, ..] if crate::command_names::live_roots().contains(&root.as_str()) => {
            format!("fno {root}: no command `{rest}`. Run fno {root} --help.")
        }
        [] => "fno: no command given. Run fno --help.".to_string(),
        [word, ..] => format!("fno: no command `{word}`. Run fno --help."),
    };
    FrontDoor::Usage { message }
}

/// The front-door command, assembled once: the derive-built tree with the
/// pane and thread groups' after_help injected. classify, the inventory
/// and completion all read this one command, so the surfaces cannot drift.
///
/// The root carries no help flag: its derive disable would be a clap
/// GLOBAL setting, and clap ORs a parent's global settings into every
/// subcommand at build time, drowning any child-level override. The
/// curated `fno --help` menu owns the root spelling through the
/// intercept in classify; the typed tree answers `--help` everywhere
/// else (stdout, exit 0), and the tail carriers' own disables keep a
/// pane payload's `--help`.
pub fn front_command() -> clap::Command {
    FnoRoot::command().mut_subcommand("mux", |mux| {
        mux.mut_subcommand("pane", |pane| pane.after_help(pane_group_help()))
            .mut_subcommand("thread", |thread| thread.after_help(thread_group_help()))
    })
}

/// The pane command words, kept in sync with [`PaneOp`] by the tree test.
pub const PANE_COMMANDS_LIST: &str =
    "ls|read|run|send|wait|kill|claim|release|split|break|focus|keeper";

/// The pane group's self-teaching help: the reference line plus the three
/// long help texts. clap renders it on `fno mux pane -h`; the run command
/// reuses it for the `-h` inside its tail.
pub fn pane_group_help() -> String {
    format!(
        "{}; commands: {}
{}
{}
{}",
        crate::mux_cli::PANE_REFERENCE_USAGE,
        PANE_COMMANDS_LIST,
        crate::mux_cli::PANE_SEND_RAW_HELP,
        crate::mux_cli::PANE_RUN_WORKER_HELP,
        crate::mux_cli::PANE_LS_IDENTITY_HELP
    )
}

/// The thread group's self-teaching help: the addressing contract the two
/// portal documents quote. It rides the group's clap help as `after_help`
/// (the group re-enabled its help flag), and
/// `render_path_help(["mux", "thread"])` reads it.
pub fn thread_group_help() -> String {
    "\
The addressing door onto a live thread row. An explicit placement flag creates a portal from any command, human or agent: `--portal new`, or `--portal N` when N is not open, and a `--split DIR` halves the caller's own pane unless `--from portal N|worker|current` names another cell. No flag tunes: the reach focuses the row's open portal, and with none open the door's own portal 0 opens for it. A default never creates: a bare `fno agent spawn` is a paneless thread. The key matches one live row exactly, not by the prefix or substring tiers `fno mux view` uses; zero matches, or several rows answering the same key, refuse and spawn no worker.

  fno mux thread w2
  fno mux thread w2 --portal new --split right --from portal 0
  fno agent whoami prints both values: `name` for either harness, the full `session` id for Codex, and the `short_id` job id for Claude - a full Claude transcript UUID is not accepted here.

Move a live pane-hosted worker into a portal, keeping its terminal:
  fno mux thread reseat <name> --portal N"
        .to_string()
}

/// The deepest declared command path in `args` (e.g. `["mux", "pane"]` for
/// `fno mux pane bogus`), used to qualify parse-error refusals. Walks the
/// declaration, stopping at the first token no level names.
fn path_words(args: &[OsString]) -> Vec<&str> {
    let mut words = Vec::new();
    let mut cmd = front_command();
    for a in args {
        let Some(w) = a.to_str() else { break };
        if w.starts_with('-') {
            break;
        }
        let Some(sub) = cmd.find_subcommand(w) else {
            break;
        };
        words.push(w);
        cmd = sub.clone();
    }
    words
}

/// clap's rendered help for a resolved path (e.g. `["mux", "view"]`), for
/// the shapes that ask for help on a leaf that keeps its tail (`mux view -h`).
pub fn render_path_help(words: &[&str]) -> String {
    let mut cmd = front_command();
    for w in words {
        cmd = match cmd.find_subcommand(w) {
            Some(sub) => sub.clone(),
            None => return String::new(),
        };
    }
    cmd.render_help().to_string()
}

/// The typed front door: map the derive's own parse to a role. The family
/// tails re-slice from the ORIGINAL argv (`mux_with_tail`), never from
/// clap's captured values.
fn front_door_from(root: FnoRoot, args: &[OsString]) -> FrontDoor {
    // The one side effect classification keeps from the old cursor parser:
    // the `--session` deprecation note, which names the flag.
    if root.session.is_some() {
        crate::mux_cli::note_server_flag("--session");
    }
    let FnoRoot {
        server,
        session,
        cmd,
    } = root;
    if server.is_some() || session.is_some() {
        // Locked-7: only the exact pair attaches. A server/session flag
        // beside a subcommand is usage - never a forward, and never the
        // socket role, which would silently drop the trailing argv (AC3-ERR).
        return match cmd {
            None => {
                let from_server = server.is_some();
                let name = server.or(session).unwrap_or_default();
                let explicit_socket = from_server && name.contains('/');
                FrontDoor::Attach {
                    name: Some(name),
                    explicit_socket,
                }
            }
            Some(_) => FrontDoor::Usage {
                message: "fno: a leading --server/--session pair never combines with a subcommand"
                    .into(),
            },
        };
    }
    match cmd {
        // Bare `fno` (no flags, no subcommand) is the attach the client runs:
        // the picker when nothing is pinned, the nested guard otherwise.
        None if args.is_empty() => FrontDoor::Attach {
            name: None,
            explicit_socket: false,
        },
        // `fno --` (an escape with nothing after it) names no command.
        None => unclaimed(args),
        Some(RootCmd::Version { json }) => FrontDoor::Version { json },
        Some(RootCmd::Uninstall(opts)) => FrontDoor::Uninstall(opts),
        // No classifier claimed it, and no Python CLI is left to forward to.
        Some(RootCmd::External(_)) => unclaimed(args),
        Some(RootCmd::Mux(mux)) => {
            // The server subcommand carries its own deprecated `--session`
            // alias; it warns here, exactly where the root-level spelling
            // warned above (main.rs routes on the parsed fields only).
            if let MuxCmd::Server(s) = &mux.cmd {
                if s.session.is_some() {
                    crate::mux_cli::note_server_flag("--session");
                }
            }
            FrontDoor::Mux(MuxParsed {
                cmd: mux_with_tail(mux.cmd, args),
            })
        }
    }
}

/// Re-slice each family's dispatch tail from the ORIGINAL argv, after the
/// family's leading words. The captured copies ride the typed parse unused.
fn mux_with_tail(cmd: MuxCmd, args: &[OsString]) -> MuxCmd {
    match cmd {
        MuxCmd::Pane { op } => MuxCmd::Pane {
            op: match op {
                PaneOp::Keeper { .. } => PaneOp::Keeper {
                    op: KeeperOp::List(tail_from(args, 4)),
                },
                other => other.with_tail(tail_from(args, 3)),
            },
        },
        MuxCmd::Block { op } => MuxCmd::Block {
            op: op.with_tail(tail_from(args, 3)),
        },
        MuxCmd::Tab { op } => MuxCmd::Tab {
            op: op.with_tail(tail_from(args, 3)),
        },
        // layout keeps the operation word in its tail: a common flag may sit
        // before it, and MuxCommon::take strips flags from any position.
        MuxCmd::Layout { common, op } => MuxCmd::Layout {
            common,
            op: op.with_tail(tail_from(args, 2)),
        },
        MuxCmd::Web { op } => MuxCmd::Web {
            op: op.with_tail(tail_from(args, 3)),
        },
        MuxCmd::Workspace { op } => MuxCmd::Workspace {
            op: op.with_tail(tail_from(args, 3)),
        },
        MuxCmd::Serve(_) => MuxCmd::Serve(tail_from(args, 2)),
        MuxCmd::Diag { op } => MuxCmd::Diag { op },
        MuxCmd::Where(_) => MuxCmd::Where(tail_req_from(args, 2)),
        MuxCmd::RetireSession(_) => MuxCmd::RetireSession(tail_req_from(args, 2)),
        MuxCmd::View(_) => MuxCmd::View(tail_req_from(args, 2)),
        MuxCmd::Thread { op } => MuxCmd::Thread {
            op: match op {
                ThreadOp::Reseat(_) => ThreadOp::Reseat(tail_req_from(args, 3)),
                ThreadOp::Name(words) => ThreadOp::Name(words),
            },
        },
        // The seven already-typed commands carry no tail.
        typed => typed,
    }
}

/// Slice the ORIGINAL argv from `start` on, byte-exact.
fn tail_from(args: &[OsString], start: usize) -> MuxTail {
    MuxTail {
        tail: args[start.min(args.len())..].to_vec(),
    }
}

/// Same, for the leaves whose bare form is a usage refusal.
fn tail_req_from(args: &[OsString], start: usize) -> MuxTailReq {
    MuxTailReq {
        tail: args[start.min(args.len())..].to_vec(),
    }
}

/// The first tokens that can begin a native invocation.
fn native_first(args: &[OsString]) -> bool {
    matches!(
        args.first().and_then(|a| a.to_str()),
        Some("mux" | "version" | "uninstall" | "--server" | "--session")
    )
}

/// The native command inventory: one tab-separated row per declared path
/// (without the `fno` word), sorted - path, kind (root|group|leaf),
/// visibility, aliases, flags. Generated artifact of record for the Python
/// ratchets (`scripts/ci/native-command-tree.txt`); the freshness test below
/// fails `cargo test` the moment the artifact lags this tree.
pub fn render_inventory() -> String {
    fn flags_of(cmd: &clap::Command) -> Vec<String> {
        let mut flags: Vec<String> = Vec::new();
        for arg in cmd.get_arguments() {
            if arg.is_positional() || arg.get_id() == "help" || arg.get_id() == "version" {
                continue;
            }
            let mut spellings: Vec<String> = Vec::new();
            if let Some(long) = arg.get_long() {
                spellings.push(format!("--{long}"));
            }
            if let Some(short) = arg.get_short() {
                spellings.push(format!("-{short}"));
            }
            if spellings.is_empty() {
                continue;
            }
            if arg.is_hide_set() {
                spellings[0] = format!("!{}", spellings[0]);
            }
            flags.push(spellings.join(","));
        }
        flags.sort();
        flags
    }

    fn walk(cmd: &clap::Command, prefix: &str, rows: &mut Vec<String>) {
        let name = cmd.get_name().to_string();
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        let kind = if cmd.has_subcommands() {
            "group"
        } else {
            "leaf"
        };
        let visibility = if cmd.is_hide_set() {
            "hidden"
        } else {
            "visible"
        };
        let aliases = cmd.get_aliases().collect::<Vec<_>>().join(",");
        let aliases = if aliases.is_empty() {
            "-".to_string()
        } else {
            aliases
        };
        let flags = flags_of(cmd).join(",");
        let flags = if flags.is_empty() {
            "-".to_string()
        } else {
            flags
        };
        rows.push(format!("{path}\t{kind}\t{visibility}\t{aliases}\t{flags}"));
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            walk(sub, &path, rows);
        }
    }

    let mut rows: Vec<String> = Vec::new();
    let mut root_flags = flags_of(&front_command()).join(",");
    if root_flags.is_empty() {
        root_flags = "-".into();
    }
    rows.push(format!("(root)\troot\tvisible\t-\t{root_flags}"));
    for sub in front_command().get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        walk(sub, "", &mut rows);
    }
    rows.sort();
    let header = concat!(
        "# Native fno command inventory, generated from the typed clap tree.\n",
        "# Regenerate with:\n",
        "#   cargo run --quiet --manifest-path crates/fno/Cargo.toml --example native_command_tree > scripts/ci/native-command-tree.txt\n",
        "# Columns: path, kind (root|group|leaf), visibility, aliases, flags\n",
        "# (hidden flags prefixed !; a hidden FLAG ALIAS is not listed).\n"
    );
    format!("{header}{}\n", rows.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn registry_root_and_mux_have_help_and_no_duplicates() {
        let cmd = FnoRoot::command();
        let mut seen = std::collections::BTreeSet::new();
        for arg in cmd.get_arguments() {
            if let Some(long) = arg.get_long() {
                assert!(seen.insert(long.to_string()), "--{long} declared twice");
                if arg.is_hide_set() {
                    continue;
                }
                let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
                assert!(!help.trim().is_empty(), "--{long}: empty help");
            }
        }
        for sub in cmd.get_subcommands() {
            assert!(
                sub.get_about()
                    .map(|a| !a.to_string().trim().is_empty())
                    .unwrap_or(false),
                "subcommand {} lacks an about line",
                sub.get_name()
            );
        }
    }

    #[test]
    fn bare_fno_is_the_attach() {
        // No argv at all is the bare attach (the picker or the nested
        // guard decides downstream); a bare `--` names no command.
        assert_eq!(
            classify(&[]),
            FrontDoor::Attach {
                name: None,
                explicit_socket: false
            }
        );
        assert!(matches!(classify(&os(&["--"])), FrontDoor::Usage { .. }));
    }

    #[test]
    fn root_help_and_version_answer_and_the_rest_refuses() {
        assert_eq!(
            classify(&os(&["--help"])),
            FrontDoor::RootHelp { all: false }
        );
        assert_eq!(
            classify(&os(&["-h", "--all"])),
            FrontDoor::RootHelp { all: true }
        );
        assert_eq!(classify(&os(&["--version"])), FrontDoor::RootVersion);
        let usage = |args: &[&str]| match classify(&os(args)) {
            FrontDoor::Usage { message } => message,
            other => panic!("{args:?}: {other:?}"),
        };
        assert_eq!(
            usage(&["--wat"]),
            "fno: no command `--wat`. Run fno --help."
        );
        assert_eq!(
            usage(&["config", "frob"]),
            "fno config: no command `frob`. Run fno config --help."
        );
        assert_eq!(
            usage(&["serach", "anything"]),
            "fno: no command `serach`. Run fno --help."
        );
        // A non-UTF-8 payload refuses by its lossy spelling, never a panic.
        let mut raw = os(&["config"]);
        raw.push(std::os::unix::ffi::OsStringExt::from_vec(vec![0xff]));
        assert!(matches!(classify(&raw), FrontDoor::Usage { .. }));
    }

    #[test]
    fn locked7_leading_pair_attaches_socket_or_usage() {
        assert_eq!(
            classify(&os(&["--server", "work"])),
            FrontDoor::Attach {
                name: Some("work".into()),
                explicit_socket: false
            }
        );
        assert_eq!(
            classify(&os(&["--server", "/tmp/x.sock"])),
            FrontDoor::Attach {
                name: Some("/tmp/x.sock".into()),
                explicit_socket: true
            }
        );
        // Only the exact pair attaches: a bare flag or trailing argv is
        // usage, never a silent forward (AC3-ERR).
        assert!(matches!(
            classify(&os(&["--server"])),
            FrontDoor::Usage { .. }
        ));
        assert!(matches!(
            classify(&os(&["--server", "work", "backlog", "list"])),
            FrontDoor::Usage { .. }
        ));
        // The socket spelling obeys the same exact-pair rule: trailing argv
        // after `--server <path>` is usage, never a socket role that drops
        // the command.
        assert!(matches!(
            classify(&os(&["--server", "/tmp/x.sock", "version"])),
            FrontDoor::Usage { .. }
        ));
        assert!(matches!(
            classify(&os(&["--server", "/tmp/x.sock", "backlog", "list"])),
            FrontDoor::Usage { .. }
        ));
        assert!(matches!(
            classify(&os(&["--session"])),
            FrontDoor::Usage { .. }
        ));
        assert!(matches!(
            classify(&os(&["--session", "work", "backlog", "list"])),
            FrontDoor::Usage { .. }
        ));
    }

    #[test]
    fn version_is_typed_and_bounded() {
        assert_eq!(
            classify(&os(&["version"])),
            FrontDoor::Version { json: false }
        );
        assert_eq!(
            classify(&os(&["version", "--json"])),
            FrontDoor::Version { json: true }
        );
        assert!(matches!(
            classify(&os(&["version", "x"])),
            FrontDoor::Usage { .. }
        ));
        assert!(matches!(
            classify(&os(&["version", "--help"])),
            FrontDoor::Help { .. }
        ));
    }
    #[test]
    fn inventory_artifact_is_fresh() {
        // The committed artifact must equal what the tree renders NOW. A new
        // operation added to FnoRoot without regenerating fails here, and the
        // message names the regenerate command (AC3-HP).
        let committed = include_str!("../../../scripts/ci/native-command-tree.txt");
        assert_eq!(
            committed,
            render_inventory(),
            "scripts/ci/native-command-tree.txt is stale - regenerate with: \
             cargo run --quiet --manifest-path crates/fno/Cargo.toml --example native_command_tree \
             > scripts/ci/native-command-tree.txt"
        );
    }

    #[test]
    fn inventory_paths_classify_as_positive_controls() {
        // Every leaf row classifies without an unrecognized-subcommand
        // refusal; every group row plus the probe command refuses BY NAME. The
        // thread group is the one exception: its external arm accepts a bare
        // name, so the probe resolves as the thread's name argument.
        for row in render_inventory().lines().filter(|l| !l.starts_with('#')) {
            let cols: Vec<&str> = row.split('\t').collect();
            let path = cols[0];
            if path == "(root)" {
                continue;
            }
            let tokens: Vec<OsString> = path.split(' ').map(OsString::from).collect();
            if cols[1] == "group" {
                let mut probe = tokens.clone();
                probe.push(OsString::from("__fno_command_probe__"));
                match classify(&probe) {
                    FrontDoor::Usage { message } => assert!(
                        message.contains("__fno_command_probe__"),
                        "group {path} must name the probe in its refusal"
                    ),
                    FrontDoor::Mux(_) if path == "mux thread" => {
                        // `thread <name>` is the row's own argument surface.
                    }
                    other => panic!("group {path} accepted the probe without naming it: {other:?}"),
                }
            } else {
                assert!(
                    !matches!(classify(&tokens), FrontDoor::Usage { message }
                        if message.contains("unrecognized subcommand")),
                    "leaf path {path} must classify without an unrecognized-subcommand refusal"
                );
            }
        }
        // Hidden paths are IN the artifact, marked hidden (AC3-EDGE).
        for (path, kind) in [
            ("mux diag stats", "leaf"),
            ("mux diag rows", "leaf"),
            ("mux thread", "group"),
            ("mux thread reseat", "leaf"),
            ("mux retire-session", "leaf"),
            ("mux pane keeper", "group"),
        ] {
            let hit = render_inventory().lines().any(|l| {
                l.starts_with(path) && l.contains('\t') && l.split('\t').nth(2) == Some("hidden")
            });
            assert!(
                hit,
                "{path} must appear in the inventory as hidden ({kind})"
            );
        }
        // The root's hidden deprecated flag is marked with `!` (AC3-EDGE).
        assert!(
            render_inventory().contains("!--session"),
            "the root row must mark --session hidden"
        );
        // Operation arguments are in the artifact (AC3-EDGE).
        for path in ["mux pane keeper list", "mux thread reseat", "mux web reap"] {
            assert!(render_inventory().contains(path), "{path} must be listed");
        }
    }

    #[test]
    fn thread_group_help_renders_the_addressing_contract() {
        // The help body is the contract the two portal documents quote:
        // the TV rule (explicit placement creates, a default never does),
        // the split-from form, the discovery command, the exact-match boundary,
        // and the no-spawn refusal. `attach_id` stays internal; the help
        // teaches `name`, `session` and `short_id` (the whoami fields).
        let help = render_path_help(&["mux", "thread"]);
        assert!(help.contains("fno mux thread w2"), "{help}");
        assert!(help.contains("explicit placement flag creates"), "{help}");
        assert!(help.contains("No flag tunes"), "{help}");
        assert!(help.contains("halves the caller's own pane"), "{help}");
        assert!(help.contains("fno agent whoami"), "{help}");
        assert!(help.contains("exact"), "{help}");
        assert!(help.contains("refuse and spawn no worker"), "{help}");
        assert!(help.contains("short_id"), "{help}");
        assert!(!help.contains("attach_id"), "{help}");
    }
    #[test]
    fn typed_app_ops_resolve_and_bare_group_refuses() {
        let r = classify(&os(&[
            "mux", "app", "open", "--mode", "chat", "--focus", "x-1",
        ]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::App { op },
            }) => assert!(matches!(op, AppOp::Open(_))),
            other => panic!("typed app open expected, got {other:?}"),
        }
        let r = classify(&os(&[
            "mux", "app", "shot", "--out", "x.png", "--theme", "light",
        ]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::App { op },
            }) => assert!(matches!(op, AppOp::Shot(_))),
            other => panic!("typed app shot expected, got {other:?}"),
        }
        // The approve command parses with its positional PR and --repo; it
        // is the satisfier of config.review.human = ui.
        let r = classify(&os(&[
            "mux",
            "app",
            "approve",
            "569",
            "--repo",
            "fnodotdev/fno",
        ]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::App { op },
            }) => match op {
                AppOp::Approve(args) => {
                    assert_eq!(args.pr, 569);
                    assert_eq!(args.repo.as_deref(), Some("fnodotdev/fno"));
                }
                other => panic!("typed app approve expected, got {other:?}"),
            },
            other => panic!("typed app approve expected, got {other:?}"),
        }
        // Bare group: the refusal is the self-teaching surface; it names
        // the commands and the guide door in one command-qualified line.
        let r = classify(&os(&["mux", "app"]));
        match r {
            FrontDoor::Usage { message } => {
                assert!(
                    message.contains("open, show, focus, answer, approve, shot, state or close")
                );
                assert!(message.contains("fno whoami guide app"));
            }
            other => panic!("bare app usage expected, got {other:?}"),
        }
    }
    #[test]
    fn mux_tree_resolves_an_explicit_help_request_as_help() {
        // The mux subtree and `version` re-enabled their help flag (AC6-HP):
        // an explicit request resolves as Help, which prints clap's rendered
        // help on stdout and exits 0. The root keeps its flag off (the
        // curated `fno --help` menu owns the spelling).
        for args in [
            &["mux", "--help"][..],
            &["mux", "-h"][..],
            &["mux", "pane", "--help"],
            &["mux", "app", "--help"],
            &["mux", "app", "open", "--help"],
            &["mux", "diag", "stats", "--help"],
            &["mux", "thread", "--help"],
            &["version", "--help"],
        ] {
            match classify(&os(args)) {
                FrontDoor::Help { message } => assert!(!message.trim().is_empty(), "{args:?}"),
                other => panic!("{args:?}: expected Help, got {other:?}"),
            }
        }
        // The root itself keeps the curated menu: RootHelp, not clap's tree.
        assert!(matches!(
            classify(&os(&["--help"])),
            FrontDoor::RootHelp { .. }
        ));
    }
    #[test]
    fn only_tail_carriers_keep_their_help_flag_off() {
        // Every command whose help flag stays off carries a passthrough
        // payload tail (or owns the spelling itself, like the root's curated
        // menu and uninstall's help field). A new leaf lands with its help
        // flag on by default; adding to this list is the named, reviewed
        // decision (AC6-HP). The help-walk integration test skips this same
        // list, so the two cannot drift without one of them failing.
        let expected: std::collections::BTreeSet<&str> = [
            "uninstall",
            "mux serve",
            "mux where",
            "mux view",
            "mux retire-session",
            "mux thread reseat",
            "mux web reap",
            "mux web install-agent",
            "mux web uninstall-agent",
            "mux block pipe",
            "mux block annotate",
            "mux tab ls",
            "mux tab create",
            "mux tab rename",
            "mux tab join",
            "mux tab move",
            "mux tab close",
            "mux layout get",
            "mux layout apply",
            "mux layout graft",
            "mux workspace prune",
            "mux workspace restore",
            "mux workspace origin",
            "mux diag rows",
            "mux pane ls",
            "mux pane read",
            "mux pane run",
            "mux pane send",
            "mux pane wait",
            "mux pane kill",
            "mux pane claim",
            "mux pane release",
            "mux pane split",
            "mux pane break",
            "mux pane focus",
            "mux pane keeper list",
        ]
        .into_iter()
        .collect();
        let cmd = front_command();
        let mut got: Vec<String> = Vec::new();
        got.push(if cmd.is_disable_help_flag_set() {
            "(root)".to_string()
        } else {
            String::new()
        });
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            walk_disabled(sub, "", &mut got);
        }
        got.retain(|p| !p.is_empty());
        got.sort();
        got.dedup();
        let expected_strings: Vec<String> = expected.iter().map(|e| e.to_string()).collect();
        assert_eq!(
            got, expected_strings,
            "help-off set must stay exactly the tail carriers"
        );
    }

    fn walk_disabled(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let name = cmd.get_name().to_string();
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        if cmd.is_disable_help_flag_set() {
            out.push(path.clone());
        }
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            walk_disabled(sub, &path, out);
        }
    }

    #[test]
    fn typed_families_resolve_with_re_sliced_tails() {
        // The carry families ride their typed op enums; the payload tail is
        // re-sliced from the ORIGINAL argv, so a leading `--` survives
        // (clap would drop the escape) and flags precede the payload.
        let r = classify(&os(&["mux", "pane", "run", "--cwd", "/x", "--", "claude"]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::Pane { op },
            }) => {
                assert!(matches!(op, PaneOp::Run(_)));
                let got: Vec<String> = op
                    .tail()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(got, ["--cwd", "/x", "--", "claude"]);
            }
            other => panic!("typed pane expected, got {other:?}"),
        }
        let r = classify(&os(&["mux", "workspace", "prune", "--dry-run"]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::Workspace { op },
            }) => {
                assert!(matches!(op, WorkspaceOp::Prune(_)));
                assert_eq!(op.tail().len(), 1, "--dry-run rides the tail");
            }
            other => panic!("typed workspace expected, got {other:?}"),
        }
        // layout: a common flag before the command still resolves the command, and
        // the tail keeps the flag for the family's own MuxCommon::take.
        let r = classify(&os(&["mux", "layout", "--json", "apply", "spec.toml"]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::Layout { common, op },
            }) => {
                assert!(common.json, "the group parsed --json");
                assert!(matches!(op, LayoutOp::Apply(_)));
                let got: Vec<String> = op
                    .tail()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(got, ["--json", "apply", "spec.toml"]);
            }
            other => panic!("typed layout expected, got {other:?}"),
        }
        // thread: the declared reseat op routes; a bare name rides the
        // external arm.
        let r = classify(&os(&["mux", "thread", "reseat", "7"]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::Thread { op },
            }) => {
                assert!(matches!(op, ThreadOp::Reseat(_)));
                assert_eq!(op.tail(), os(&["7"]));
            }
            other => panic!("typed thread expected, got {other:?}"),
        }
        let r = classify(&os(&["mux", "thread", "wk"]));
        match r {
            FrontDoor::Mux(MuxParsed {
                cmd: MuxCmd::Thread { op },
            }) => {
                assert!(matches!(op, ThreadOp::Name(_)));
                assert_eq!(op.tail(), os(&["wk"]));
            }
            other => panic!("typed thread expected, got {other:?}"),
        }
    }
}

/// The common mux command flags: the server axis plus the machine-output flag
/// every scriptable command parses. One declaration; the deprecation
/// note fires here when the legacy `--session` spelling binds. Command-specific
/// tokens ride through in `rest`, order kept, so each command's own grammar
/// still refuses its unknowns.
#[derive(Parser, Debug, Default, PartialEq, Eq, Clone)]
pub struct MuxCommon {
    /// Server (socket session) this command addresses
    #[arg(long, value_name = "NAME")]
    pub server: Option<String>,
    /// Deprecated spelling of --server (warns)
    #[arg(long, hide = true, value_name = "NAME")]
    pub session: Option<String>,
    /// Emit machine-readable JSON on stdout
    #[arg(long)]
    pub json: bool,
}

impl MuxCommon {
    /// The known flag spellings this group owns.
    pub const FLAGS: &'static [&'static str] = &["--server", "--session", "--json"];

    /// Split the common flags out of a small command's argv. Non-UTF-8 input is
    /// the old refusal; a valueless server flag is the old `{flag} needs a
    /// value` error; a repeated common flag is refused so the command's own
    /// grammar cannot disagree with this one.
    pub fn take(toks: &[OsString]) -> Result<(MuxCommon, Vec<String>), String> {
        let mut pairs: Vec<String> = Vec::new();
        let mut rest = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let tok = toks[i]
                .to_str()
                .ok_or_else(|| "non-UTF-8 argument".to_string())?;
            if Self::FLAGS.contains(&tok) {
                if tok != "--json" {
                    if i + 1 >= toks.len() {
                        return Err(format!("{tok} needs a value"));
                    }
                    let value = toks[i + 1]
                        .to_str()
                        .ok_or_else(|| "non-UTF-8 argument".to_string())?;
                    pairs.push(tok.to_string());
                    pairs.push(value.to_string());
                    i += 2;
                } else {
                    pairs.push(tok.to_string());
                    i += 1;
                }
                continue;
            }
            rest.push(tok.to_string());
            i += 1;
        }
        let cmd = <Self as clap::CommandFactory>::command()
            .no_binary_name(true)
            .disable_help_flag(true)
            .disable_version_flag(true);
        let matches = cmd
            .try_get_matches_from(pairs)
            .map_err(|e| refusal_line("fno mux", &e))?;
        let session = matches.get_one::<String>("session").cloned();
        if session.is_some() {
            crate::mux_cli::note_server_flag("--session");
        }
        Ok((
            MuxCommon {
                server: matches.get_one::<String>("server").cloned(),
                session,
                json: matches.get_flag("json"),
            },
            rest,
        ))
    }
}

/// `fno mux thread <name>`'s flags (/): the portal reach and the
/// placement trio, one typed declaration replacing the command's scan and its
/// flag-value macro.
#[derive(Parser, Debug, Default, PartialEq, Eq)]
#[command(
    no_binary_name = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct ThreadArgs {
    /// Which portal to reach through: an index, or "new" for a dedicated portal in a new tab
    #[arg(long)]
    pub portal: Option<String>,
    /// Workspace whose tab hosts the thread
    #[arg(long, alias = "squad", short = 's', value_name = "NAME")]
    pub workspace: Option<String>,
    /// Split direction for a fresh open
    #[arg(long, short = 'x')]
    pub split: Option<String>,
    /// Tab selector for a fresh open
    #[arg(long)]
    pub tab: Option<String>,
    /// Anchor pane id for a fresh open
    #[arg(long)]
    pub at: Option<String>,
    /// The named cell a split halves: `portal N`, a worker name, or `current`
    #[arg(long)]
    pub from: Option<String>,
    /// The agent name or attach id
    pub name: Option<String>,
}

/// `fno mux block pipe`'s flags. Values stay strings here; the pure
/// validators (`parse_u64`, `parse_block_sel`) keep their exact refusals
/// when the parser converts.
#[derive(Parser, Debug, Default, PartialEq, Eq)]
#[command(
    no_binary_name = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct BlockPipeArgs {
    /// Server (socket session) this command addresses
    #[arg(long, value_name = "NAME")]
    pub server: Option<String>,
    /// Deprecated spelling of --server (warns)
    #[arg(long, hide = true, value_name = "NAME")]
    pub session: Option<String>,
    /// Emit machine-readable JSON on stdout
    #[arg(long)]
    pub json: bool,
    /// Force when the target pane is not idle
    #[arg(long)]
    pub force: bool,
    /// Source pane id to read the completed block from
    #[arg(long)]
    pub from: Option<String>,
    /// Target pane id to pipe into
    #[arg(long)]
    pub to: Option<String>,
    /// Which block to read (last | <seq>)
    #[arg(long)]
    pub block: Option<String>,
}

/// `fno mux block annotate`'s flags; same string-then-validate shape as
/// [`BlockPipeArgs`].
#[derive(Parser, Debug, Default, PartialEq, Eq)]
#[command(
    no_binary_name = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct BlockAnnotateArgs {
    /// Server (socket session) this command addresses
    #[arg(long, value_name = "NAME")]
    pub server: Option<String>,
    /// Deprecated spelling of --server (warns)
    #[arg(long, hide = true, value_name = "NAME")]
    pub session: Option<String>,
    /// Source pane id to read the block from
    #[arg(long)]
    pub from: Option<String>,
    /// Which block to read (last | <seq>)
    #[arg(long)]
    pub block: Option<String>,
    /// The node the finding is recorded against
    #[arg(long)]
    pub node: Option<String>,
    /// The finding text
    #[arg(short = 'm', long)]
    pub message: Option<String>,
}

/// One command-qualified refusal line for a parse failure. The caller prints
/// it to stderr and exits 2; clap's own multi-line usage block never reaches
/// the operator.
pub fn refusal_line(cmd: &str, err: &clap::Error) -> String {
    let rendered = err.render().to_string();
    let first = rendered
        .lines()
        .next()
        .unwrap_or("invalid arguments")
        .trim()
        .trim_start_matches("error: ")
        .trim();
    format!("{cmd}: {first}")
}
