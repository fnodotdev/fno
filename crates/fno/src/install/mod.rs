//! Update, uninstall and the signed binaries an install places.

#[cfg(feature = "full")]
pub mod build_drift;
#[cfg(feature = "full")]
pub mod codesign;
#[cfg(feature = "full")]
pub mod doctor_update;
#[cfg(all(test, feature = "full"))]
#[path = "doctor_update_tests.rs"]
mod doctor_update_tests;
#[cfg(feature = "full")]
pub(crate) mod release_install;
#[cfg(feature = "full")]
pub mod shim_check;
#[cfg(feature = "full")]
pub mod uninstall;
#[cfg(feature = "full")]
pub(crate) mod update_prebuilt;
