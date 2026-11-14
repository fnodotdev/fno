//! The surface the `fno-app` desktop crate links. Kept to what the app
//! imports: the modules behind these paths stay crate-private, so crates.io
//! users see no more API than before.

pub use crate::mux_cli::pane_submit::CR_SETTLE_MS;

pub mod client {
    pub use crate::client::{agent_lattice_state, tab_label_text};

    pub mod agent_group {
        pub use crate::client::agent_group::{click_command, RowClick};
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
        pub use crate::client::org_seats::{read_fleet, Machine};
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

pub mod digest_overlay {
    pub use crate::digest_overlay::{now_secs, theme_resolved};
}

pub mod lattice {
    pub use crate::lattice::{lattice_glyph, status_long, LatticeState};
}

pub mod plugins_feed {
    pub use crate::plugins_feed::*;
}

pub mod transcript_tail {
    pub use crate::transcript_tail::{claude_projects_dir, transcript_uuid_shaped};
}

pub mod web_hq {
    pub use crate::web_hq::{Board, OpenQuestion};
}
