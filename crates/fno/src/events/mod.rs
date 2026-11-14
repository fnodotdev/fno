//! The `fno doctor event` commands and the rows the front door writes.

#[cfg(feature = "full")]
pub mod attest;
#[cfg(feature = "full")]
pub mod chokepoint_events;
#[cfg(feature = "full")]
pub mod cli_usage;
#[cfg(feature = "full")]
pub mod event_cli;
#[cfg(feature = "full")]
pub(crate) mod event_corrections;
#[cfg(feature = "full")]
pub(crate) mod event_signals;
#[cfg(feature = "full")]
pub(crate) mod first_check;
