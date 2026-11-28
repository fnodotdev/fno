//! The composer's one-shot `!` run (v135): one command on its own pty, drawn
//! in the card of the client that started it.

use serde::{Deserialize, Serialize};

/// What a client asks of its run. `run` is client-minted, and it names the
/// run in every later message of that client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ShellRunRequest {
    /// Run `line` with `shell -lc`. The line is one argv element, never
    /// script text. `squad` is the workspace its tab opens in when it moves.
    Start {
        run: u64,
        squad: u64,
        cwd: String,
        shell: String,
        line: String,
        rows: u16,
        cols: u16,
    },
    /// Keys for the running command, as typed.
    Input { run: u64, bytes: Vec<u8> },
    /// Move the running command to a new tab. It is the same process.
    ToTab { run: u64 },
}

/// Where a run stands. Its live screen rides [`crate::ServerMsg::Frame`]
/// under the pane that `Running` names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ShellRunState {
    Running {
        pane: u64,
    },
    /// The run is a pane in a tab now, and the card lets it go.
    Moved,
    /// `exit` is absent when the run was killed before it could record one.
    Done {
        exit: Option<i32>,
        output: String,
    },
    Refused {
        reason: String,
    },
}

/// One run's progress, sent only to the client that started it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ShellRunUpdate {
    pub run: u64,
    pub state: ShellRunState,
}
