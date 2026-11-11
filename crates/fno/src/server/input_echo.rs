//! A pane's input deadline, reset by output and limited to one warning per minute.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::agents_view::RegistryAgent;
use crate::thread_viewer::{row_for_pane, Portal};

/// Who a silent pane belongs to. A portal seat has no registry row of its
/// own (its thread row carries `mux: None`), so the pane lookup alone names
/// every portal stall "unnamed"; the portal map is asked first.
pub(super) fn no_echo_subject(
    portals: &BTreeMap<u8, Portal>,
    agents: &[RegistryAgent],
    session: &str,
    pane: u64,
    pid: Option<u32>,
) -> String {
    // A re-seated pane worker keeps `mux: Some(..)`, which `row_for_pane`
    // skips, so the pane lookup still answers for it.
    let name = row_for_pane(portals, pane, agents)
        .map(|row| row.name.clone())
        .or_else(|| super::pane_meta::pane_registry_name(agents, session, pane, pid))
        .unwrap_or_else(|| "unnamed".into());
    match portals.iter().find(|(_, p)| p.seat == pane) {
        Some((idx, portal)) => format!("name={name:?} portal={idx} row_key={:?}", portal.row_key),
        None => format!("name={name:?}"),
    }
}

#[derive(Default)]
pub(super) struct EchoWatch {
    pending: Option<(Instant, Instant)>,
    last_warning: Option<Instant>,
}

impl EchoWatch {
    pub(super) fn sent(&mut self, now: Instant, last_output: Instant) {
        self.pending.get_or_insert((now, last_output));
    }
    pub(super) fn poll(&mut self, now: Instant, last_output: Instant) -> bool {
        let Some((sent, output)) = self.pending else {
            return false;
        };
        if output != last_output {
            self.pending = None;
            return false;
        }
        if now.duration_since(sent) < Duration::from_secs(3) {
            return false;
        }
        self.pending = None;
        if self
            .last_warning
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(60))
        {
            return false;
        }
        self.last_warning = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_log_input_warns_without_echo_and_limits_repeated_keys() {
        let t = Instant::now();
        let mut watch = EchoWatch::default();
        watch.sent(t, t);
        watch.sent(t + Duration::from_secs(1), t);
        assert!(!watch.poll(t + Duration::from_secs(2), t));
        assert!(watch.poll(t + Duration::from_secs(3), t));
        assert!(!watch.poll(t + Duration::from_secs(30), t));
        watch.sent(t + Duration::from_secs(31), t);
        assert!(!watch.poll(t + Duration::from_secs(34), t));
        watch.sent(t + Duration::from_secs(61), t);
        assert!(watch.poll(t + Duration::from_secs(64), t));
        watch.sent(t + Duration::from_secs(65), t);
        assert!(!watch.poll(t + Duration::from_secs(68), t + Duration::from_secs(66)));
        assert!(!watch.poll(t + Duration::from_secs(130), t + Duration::from_secs(66)));
    }

    #[test]
    fn mux_log_input_no_echo_names_the_portal_and_its_thread_row() {
        let row = RegistryAgent {
            name: "worker".into(),
            attach_id: Some("deadbee1".into()),
            ..RegistryAgent::default()
        };
        let mut portals = BTreeMap::new();
        portals.insert(
            3,
            Portal {
                row_key: "deadbee1".into(),
                seat: 42,
                tab: 1,
            },
        );
        let reseated = RegistryAgent {
            name: "pane-worker".into(),
            mux: Some(("sess".into(), 50)),
            ..RegistryAgent::default()
        };
        portals.insert(
            4,
            Portal {
                row_key: "pane-worker".into(),
                seat: 50,
                tab: 1,
            },
        );
        let agents = [row, reseated];
        assert_eq!(
            no_echo_subject(&portals, &agents, "sess", 50, None),
            "name=\"pane-worker\" portal=4 row_key=\"pane-worker\""
        );
        assert_eq!(
            no_echo_subject(&portals, &agents, "sess", 42, None),
            "name=\"worker\" portal=3 row_key=\"deadbee1\""
        );
        assert_eq!(
            no_echo_subject(&portals, &agents, "sess", 7, None),
            "name=\"unnamed\""
        );
    }
}
