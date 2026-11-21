//! `fno doctor` and the repository lints.

#[cfg(feature = "app")]
pub mod claude_fallback;
#[cfg(feature = "app")]
pub mod doctor;
#[cfg(feature = "app")]
pub mod doctor_cost;
#[cfg(feature = "app")]
pub mod lint_cli;
#[cfg(feature = "app")]
pub mod menu_caps;
#[cfg(feature = "app")]
pub mod repo_lint;
