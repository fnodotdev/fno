//! `fno config setup`, the onboarding wizard and the first-run migrations.

#[cfg(feature = "app")]
pub mod cli_hooks;
#[cfg(feature = "app")]
pub mod first_step;
#[cfg(feature = "app")]
pub mod gh_proxy;
#[cfg(all(test, feature = "app"))]
#[path = "gh_proxy_tests.rs"]
mod gh_proxy_tests;
#[cfg(feature = "app")]
pub mod must_enable;
#[cfg(feature = "app")]
pub mod setup_attention_remote;
#[cfg(feature = "app")]
pub mod setup_autowire;
#[cfg(all(test, feature = "app"))]
#[path = "setup_autowire_tests.rs"]
mod setup_autowire_tests;
#[cfg(feature = "app")]
pub mod setup_cmds;
#[cfg(feature = "app")]
pub mod setup_daemon_agent;
#[cfg(feature = "app")]
pub mod setup_detect;
#[cfg(feature = "app")]
pub mod setup_messaging;
#[cfg(feature = "app")]
pub mod setup_profile;
#[cfg(feature = "app")]
pub mod setup_run;
#[cfg(feature = "app")]
pub mod setup_services;
#[cfg(feature = "app")]
pub mod state_layout;
#[cfg(feature = "app")]
pub mod wizard;
#[cfg(feature = "app")]
pub mod wizard_term;
