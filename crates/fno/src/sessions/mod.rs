//! Agent sessions: history, liveness, restore, transfer and spawn admission.

#[cfg(feature = "full")]
pub mod agents_history;
#[cfg(feature = "full")]
pub mod argv_witness;
#[cfg(feature = "full")]
pub mod process_admission;
#[cfg(feature = "full")]
pub mod restore_gate;
#[cfg(feature = "full")]
pub mod restore_liveness;
#[cfg(feature = "full")]
pub mod served_liveness;
#[cfg(feature = "full")]
pub mod session_move;
#[cfg(feature = "full")]
pub mod session_origin;
#[cfg(feature = "full")]
pub mod spawn_floor;
#[cfg(feature = "full")]
pub mod spawn_journal;
#[cfg(feature = "full")]
pub mod transcript_transfer;
