//! The mail protocol core: nothing in this crate names a fleet crate. Cargo
//! holds that rule: the manifest names only serde and serde_json.

pub mod address;
pub mod envelope;
pub mod frame;
pub mod lane;
pub mod log;
pub mod session;
pub mod state;
