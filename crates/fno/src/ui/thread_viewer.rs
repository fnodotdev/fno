use std::collections::BTreeMap;

use crate::agents_view::RegistryAgent;
use crate::tree::TabId;

/// One open portal under the TV model: the channel it is tuned to, the
/// screen pane playing it, and the tab that holds the screen's leaf. A
/// portal is never a pane and never a tab; the pane is only what plays
/// now. `row_key` is the channel: the row's id
/// ([`RegistryAgent::stored_key`]) as a legacy fallback. New local portals
/// prefer the harness session id. Display labels and terminal titles never
/// select the channel.
#[derive(Clone)]
pub(crate) struct Portal {
    pub(crate) row_key: String,
    pub(crate) seat: u64,
    pub(crate) tab: TabId,
}

pub(crate) fn row_for_pane<'a>(
    portals: &BTreeMap<u8, Portal>,
    pane: u64,
    agents: &'a [RegistryAgent],
) -> Option<&'a RegistryAgent> {
    row_for_pane_argv(portals, pane, agents, None)
}

/// [`row_for_pane`] with the seat's live child argv. Two rows can share one
/// channel key (two threads carrying the same name); the seat's child runs
/// `attach <id>` for exactly one of them, so the argv settles the tie.
/// Without an argv a tie still reads `None`.
pub(crate) fn row_for_pane_argv<'a>(
    portals: &BTreeMap<u8, Portal>,
    pane: u64,
    agents: &'a [RegistryAgent],
    argv: Option<&[String]>,
) -> Option<&'a RegistryAgent> {
    let key = portals
        .values()
        .find(|portal| portal.seat == pane)?
        .row_key
        .as_str();
    let mut matches = agents
        .iter()
        .filter(|row| row.mux.is_none() && !row.exited && row.keyed_by(key));
    let row = matches.next()?;
    let Some(second) = matches.next() else {
        return Some(row);
    };
    let argv = argv?;
    let mut attached = [row, second].into_iter().chain(matches).filter(|r| {
        r.attach_id
            .as_deref()
            .is_some_and(|id| crate::pane_argv::attaches(argv, id))
    });
    let hit = attached.next()?;
    attached.next().is_none().then_some(hit)
}

pub(crate) fn identity_for_pane(
    portals: &BTreeMap<u8, Portal>,
    pane: u64,
    agents: &[RegistryAgent],
    argv: Option<&[String]>,
) -> Option<String> {
    row_for_pane_argv(portals, pane, agents, argv)
        .and_then(|row| row.effective_identity())
        .map(str::to_string)
}
