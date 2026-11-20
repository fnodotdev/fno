//! The `fno doctor event` commands and the rows the front door writes.

#[cfg(feature = "app")]
pub mod attest;
#[cfg(feature = "app")]
pub mod chokepoint_events;
#[cfg(feature = "app")]
pub mod cli_usage;
#[cfg(feature = "app")]
pub mod event_cli;
#[cfg(feature = "app")]
pub(crate) mod event_corrections;
#[cfg(feature = "app")]
pub(crate) mod event_signals;
#[cfg(feature = "app")]
pub(crate) mod first_check;
