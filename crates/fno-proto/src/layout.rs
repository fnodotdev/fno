//! Plain data the mux wire carries: pane and tab ids, the split axis, directions,
//! rectangles, the pane tree, and the shell-activity reading. No behaviour lives here;
//! the layout engine that edits a [`Node`] stays in the `fno` crate.

use serde::{Deserialize, Serialize};

/// Monotonic allocation is the SERVER's job (task 2.3, an `AtomicU64`); the
/// tree never mints ids, callers pass a new id into [`split`].
pub type PaneId = u64;

/// Split direction: `Horizontal` children sit side by side (left -> right,
/// i.e. a "split H" / vertical divider line); `Vertical` children stack top
/// -> bottom (a "split V" / horizontal divider line). Named for the axis
/// panes are arranged ALONG, matching tmux/i3 convention, not the divider's
/// own orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A navigate/resize direction. Rides the wire in `Command::FocusDir` /
/// `Command::ResizeDir` (task 2.2), hence `Serialize`/`Deserialize` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A tiled rectangle: `(x, y)` is the top-left corner within the content
/// area, `rows`/`cols` the size in cells. u16 matches terminal dimensions
/// (`ClientMsg::Attach`/`Resize` already use u16 rows/cols).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub rows: u16,
    pub cols: u16,
}

/// A node in the pane tree. `Branch` ratios sum to 1.0 (checked by
/// [`check_invariants`]); a `Branch` always has >= 2 children - a
/// single-child branch is collapsed away by [`close`], and a same-axis
/// nested branch is normalized (merged) away, never left standing.
///
/// ponytail: the shared-type contract asks for `Copy, Eq` on every type in
/// this module, but neither derives on `Node`/`Tab`: `Vec` is never `Copy`,
/// and `f32` never implements `Eq` (NaN), so `#[derive(Eq)]` on a type
/// containing one is a compile error, not a style choice. `Axis`, `Dir`,
/// `Rect` (below) get the full derive set as specified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Node {
    Leaf(PaneId),
    Branch {
        axis: Axis,
        children: Vec<(f32, Node)>,
    },
}

/// Stable tab identity: session-scoped, monotonic, never reused (Locked
/// Decision 6 extended to tabs in Phase 3). Minted by `squad::Session`, like
/// `PaneId` the tree itself never allocates one.
pub type TabId = u64;

/// What the pane's own OSC 133 evidence says about its shell: a
/// four-way reading, never a blind default. `Unmeasured` when no marker was
/// ever seen (nothing here is knowable - the reading the old render folded
/// into `Idle`), `Running` when a command block is open, `Empty` when the
/// shell drew a prompt and ran nothing, `Idle` when a block completed and the
/// prompt is back. A rendered value must be a function of this signal, never
/// of which builder produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ShellActivity {
    Unmeasured,
    Running,
    Empty,
    Idle,
}
