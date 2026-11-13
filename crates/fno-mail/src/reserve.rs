//! The fleet reserve vocabulary both ends read: the daemon that arms a hold
//! while a listed app runs, and the mux settings row that edits the list.

/// The apps watched when `agents.reserve_apps` is unset. An entry is an app
/// name as its bundle spells it; `Name=stream` arms the stream preset (spawns
/// and tests held), any other entry arms edit (tests held).
pub const DEFAULT_APPS: &[&str] = &[
    "OBS=stream",
    "Final Cut Pro",
    "Premiere Pro",
    "DaVinci Resolve",
];
