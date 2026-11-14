//! `fno doctor` and the repository lints.

#[cfg(feature = "full")]
pub mod claude_fallback;
#[cfg(feature = "full")]
pub mod doctor;
#[cfg(feature = "full")]
pub mod doctor_cost;
#[cfg(feature = "full")]
pub mod lint_cli;
#[cfg(feature = "full")]
pub mod menu_caps;
#[cfg(feature = "full")]
pub mod repo_lint;
