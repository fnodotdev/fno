//! The mail protocol core: nothing in this crate names a fleet crate. Cargo
//! holds that rule: the manifest names only serde, serde_json and chrono.

pub mod address;
pub mod directory;
pub mod envelope;
pub mod frame;
pub mod hold;
pub mod lane;
pub mod log;
pub mod reserve;
pub mod session;
pub mod state;
pub mod turn;
