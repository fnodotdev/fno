//! The surface the `fno-app` desktop crate links. Kept to what the app
//! imports: the modules behind these paths stay crate-private, so crates.io
//! users see no more API than before.

pub use crate::mux_cli::pane_submit::CR_SETTLE_MS;

pub mod client {
    pub use crate::client::{agent_lattice_state, tab_label_text};

    pub mod agent_group {
        pub use crate::client::agent_group::{click_command, RowClick};
    }
    pub mod agent_launcher {
        pub use crate::client::agent_launcher::projects::{
            add_project, config_projects, ConfigProject,
        };
        pub use crate::client::agent_launcher::{
            epic_rows, harness_rows, EpicChoice, HarnessRow, LEAD_SEED,
        };
    }
    pub mod agent_sort {
        pub use crate::client::agent_sort::sideline_order;
    }
    pub mod card_line {
        pub use crate::client::card_line::{binding_suffix, card_facts, CardFacts};
    }
    pub mod header_band {
        pub use crate::client::header_band::section_rollup;
    }
    pub mod org_seats {
        pub use crate::client::org_seats::{read_fleet, Fleet, Lead, Machine};
    }
    pub mod node_detail {
        pub mod backlog_md {
            pub use crate::client::node_detail::backlog_md::md_lines;
        }
    }
}

pub mod proto {
    pub use crate::mux_rows::server_wire;
}

pub mod mail_door {
    pub use crate::serve::mail_door::{send, MailVerdict, FROM};
}

pub mod splash {
    pub use crate::ui::splash::{mark_cells, MARK_SECS};
}

/// Start a detached mux server for `session` and wait for its socket, the
/// same spawn `fno mux` makes when none runs. Returns once the socket
/// accepts, or the reason it did not.
pub fn start_server(session: &str) -> std::io::Result<()> {
    let path = crate::proto::socket_path(session).map_err(std::io::Error::other)?;
    crate::client::server_spawn::start_server_at(&path)
}

pub mod digest_overlay {
    pub use crate::digest_overlay::{fno_agent_bin, now_secs, theme_resolved};
}

pub mod lattice {
    pub use crate::lattice::{lattice_glyph, status_long, LatticeState};
}

pub mod plugins_feed {
    pub use crate::plugins_feed::*;
}

pub mod transcript_tail {
    pub use crate::transcript_tail::{
        claude_projects_dir, harness_of_session, newest_transcript, transcript_uuid_shaped,
    };
}

pub mod chat_events {
    pub use crate::chat_events::opencode::newest_session_id;
}

pub mod web_hq {
    pub use crate::web_hq::{Board, OpenQuestion};
}
