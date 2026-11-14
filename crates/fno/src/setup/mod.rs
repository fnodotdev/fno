//! `fno config setup`, the onboarding wizard and the first-run migrations.

#[cfg(feature = "full")]
pub mod cli_hooks;
#[cfg(feature = "full")]
pub mod gh_proxy;
#[cfg(all(test, feature = "full"))]
#[path = "gh_proxy_tests.rs"]
mod gh_proxy_tests;
#[cfg(feature = "full")]
pub mod must_enable;
#[cfg(feature = "full")]
pub mod path_migration;
#[cfg(all(test, feature = "full"))]
#[path = "path_migration_tests.rs"]
mod path_migration_tests;
#[cfg(feature = "full")]
pub mod setup_autowire;
#[cfg(all(test, feature = "full"))]
#[path = "setup_autowire_tests.rs"]
mod setup_autowire_tests;
#[cfg(feature = "full")]
pub mod setup_cmds;
#[cfg(feature = "full")]
pub mod setup_detect;
#[cfg(feature = "full")]
pub mod setup_messaging;
#[cfg(feature = "full")]
pub mod setup_profile;
#[cfg(feature = "full")]
pub mod setup_run;
#[cfg(feature = "full")]
pub mod setup_services;
#[cfg(feature = "full")]
pub mod state_layout;
#[cfg(feature = "full")]
pub mod wizard;
#[cfg(feature = "full")]
pub mod wizard_term;
