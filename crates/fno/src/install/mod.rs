//! Update, uninstall and the signed binaries an install places.

#[cfg(feature = "app")]
pub mod build_drift;
#[cfg(feature = "app")]
pub mod codesign;
#[cfg(feature = "app")]
pub mod doctor_update;
#[cfg(all(test, feature = "app"))]
#[path = "doctor_update_tests.rs"]
mod doctor_update_tests;
#[cfg(feature = "app")]
pub(crate) mod downgrade_guard;
#[cfg(all(test, feature = "app"))]
#[path = "downgrade_guard_tests.rs"]
mod downgrade_guard_tests;
#[cfg(feature = "app")]
pub(crate) mod legacy_links;
#[cfg(feature = "app")]
pub(crate) mod release_install;
#[cfg(feature = "app")]
pub mod shim_check;
#[cfg(feature = "app")]
pub mod uninstall;
#[cfg(feature = "app")]
pub(crate) mod update_prebuilt;
