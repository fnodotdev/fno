//! The `fno` front door as a library, so integration tests (and the
//! client-under-portable-pty e2e harness) can link the same code the binary
//! runs. The binary (`main.rs`) is a thin role-select over these modules.

// The repo bans em-dashes, so doc comments use a leading "- " as a prose dash;
// clippy misreads that as an unindented markdown list item. The convention is
// deliberate, so silence the false positive crate-wide rather than reflow it
// away at every site.
#![allow(clippy::doc_lazy_continuation)]

#[cfg(feature = "app")]
pub mod app_control;
#[cfg(feature = "app")]
pub mod bus_read;
pub mod chokepoint;
#[cfg(feature = "app")]
pub mod cli_args;
pub mod effect_map;
#[cfg(feature = "app")]
pub mod event_store;
pub mod footnote_transcript;
#[cfg(feature = "app")]
pub mod git_paths;
pub mod glob;
pub mod harness;
#[cfg(feature = "app")]
pub mod identity_markers;
pub mod keeper_wire;
pub mod legacy_env;
#[cfg(feature = "app")]
mod lenient_de;
#[cfg(feature = "app")]
pub mod live_store_fence;
#[cfg(feature = "app")]
pub mod mail_ids;
#[cfg(feature = "app")]
pub mod mail_sent;
#[cfg(feature = "app")]
pub mod mail_sent_core;
#[cfg(feature = "app")]
pub mod mail_unread;
#[cfg(feature = "app")]
pub mod notify_dispatch;
pub mod pid;
#[cfg(feature = "app")]
pub mod proc_table;
pub mod product_boundary;
pub mod provider_key;
#[cfg(feature = "app")]
pub mod registry_read;
#[cfg(feature = "app")]
pub mod role_migration;
pub mod rules;
pub mod sandbox_door;
#[cfg(feature = "app")]
pub mod self_identity;
#[cfg(feature = "app")]
pub mod store_conn;
#[cfg(all(test, feature = "app"))]
mod test_keeper_cleanup;
#[cfg(all(test, feature = "app"))]
pub(crate) mod test_owner;
#[cfg(feature = "app")]
pub mod trusted_config;
pub mod version;

#[cfg(feature = "app")]
pub mod otel_read;

#[cfg(feature = "app")]
mod json_compat;

#[cfg(feature = "app")]
mod repo_identity;

pub mod backlog;
pub mod checks;
pub mod cli;
pub mod config;
pub mod events;
pub mod install;
pub mod mux;
pub mod serve;
pub mod sessions;
pub mod setup;
pub mod ui;

// The flat `crate::<module>` paths stay valid: each module moved into a
// group directory is re-exported here under its old name.
#[cfg(feature = "app")]
use backlog::refusal_surface_core;
#[cfg(feature = "app")]
pub use backlog::{
    backlog_list, backlog_model, backlog_write, blocked_rung, board_reader, inbox_user,
    join_marker, law_cli, lawbook, pr_diff, review_prior_head, search_query,
};
pub use backlog::{node_row, store_client};
#[cfg(feature = "app")]
pub use checks::{claude_fallback, doctor, doctor_cost, lint_cli, menu_caps, repo_lint};
#[cfg(feature = "app")]
pub use cli::{
    accounts_route, agents_client_route, agents_mcp_route, agents_menu, bootstrap, command_names,
    fleet_route, group_help, lead_refresh_canon, lead_route, paths_route, remote_target,
    whoami_seam, worktree_door,
};
#[cfg(feature = "app")]
pub use config::{config_cli, config_defaults, config_doctor, config_schema, model_catalog};
#[cfg(feature = "app")]
pub(crate) use events::first_check;
#[cfg(feature = "app")]
pub use events::{attest, chokepoint_events, cli_usage, event_cli};
#[cfg(feature = "app")]
use events::{event_corrections, event_signals};
#[cfg(feature = "app")]
pub use install::{build_drift, codesign, doctor_update, shim_check, uninstall};
#[cfg(feature = "app")]
pub(crate) use install::{release_install, update_prebuilt};
#[cfg(feature = "app")]
use mux::pane_send_audit;
#[cfg(feature = "app")]
pub use mux::{
    mux_cli, mux_log, notify_command, paint_stats, pane_argv, pane_cwd, proto, pty, redact,
    review_invocation, server, server_stats, shell_init, squad, squad_cascade, squad_store, vt,
    workspace_sidecar,
};
#[cfg(feature = "web")]
pub(crate) use serve::web_answer;
#[cfg(feature = "app")]
pub use serve::{app_api, backlog_snapshot, chat_events, frame_ansi, frame_html, org_root};
#[cfg(feature = "web")]
pub use serve::{attention_api, chat_ws, mux_link, observe, share, web};
#[cfg(feature = "app")]
pub(crate) use serve::{screen, web_agent, web_core, web_hq};
#[cfg(feature = "web")]
pub(crate) use serve::{web_invites, web_mine, web_trace};
#[cfg(feature = "app")]
pub use sessions::{
    agents_history, argv_witness, process_admission, restore_gate, restore_liveness,
    served_liveness, session_origin, spawn_floor, spawn_journal,
};
#[cfg(feature = "transfer")]
pub use sessions::{session_move, transcript_transfer};
#[cfg(feature = "app")]
pub use setup::{
    cli_hooks, gh_proxy, must_enable, setup_attention_remote, setup_autowire, setup_cmds,
    setup_daemon_agent, setup_detect, setup_messaging, setup_profile, setup_run, setup_services,
    state_layout, wizard, wizard_term,
};
#[cfg(feature = "app")]
pub use ui::{
    agents_view, backlog_graph, backlog_view, chrome, client, clipboard, connections_view,
    context_used, cursor, decisions_overlay, decisions_table, digest_overlay, dispatch_launch,
    feed_overlay, keybindings_file, keys, lane_colors_panel, link, messages_model, modal_select,
    mouse, mux_rows, needs_overlay, notify, open_router, org_model, org_overlay, org_titles,
    pane_border, popup, provider_picker, ratatui_blit, registry_label, sideline_color, splash,
    team_census, templates, text_style, theme, theme_import, transcript_tail, tree, ui_usage,
    view_store, wordmark,
};
#[cfg(feature = "app")]
pub(crate) use ui::{band_remap, lattice, plugins_feed, thread_viewer};
