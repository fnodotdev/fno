//! Agent sessions: history, liveness, restore, transfer and spawn admission.

#[cfg(feature = "app")]
pub mod agents_history;
#[cfg(feature = "app")]
pub mod argv_witness;
#[cfg(feature = "app")]
pub mod process_admission;
#[cfg(feature = "app")]
pub mod restore_gate;
#[cfg(feature = "app")]
pub mod restore_liveness;
#[cfg(feature = "app")]
pub mod served_liveness;
#[cfg(feature = "transfer")]
pub mod session_move;
#[cfg(feature = "app")]
pub mod session_origin;
#[cfg(feature = "app")]
pub mod spawn_floor;
#[cfg(feature = "app")]
pub mod spawn_journal;
#[cfg(feature = "transfer")]
pub mod transcript_transfer;
