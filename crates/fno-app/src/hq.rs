//! The command center reads, off the UI thread: the fleet, the open
//! questions and the backlog, the same three reads the web bridge feeds into
//! the shared HQ board. Answers go through `needs_overlay::answer`, the one
//! writer the TUI calls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use fno::app_api::web_hq::Board;
use fno::backlog_model::Inputs;
use fno::needs_overlay::AnswerPick;
use fno::proto::AgentRow;

/// The fleet and question reads, at the bridge's own cadence. Measured on
/// a 38-seat fleet: `needs` takes about 1 CPU-second a call, so a 10 s cycle
/// would hold a tenth of a core.
const POLL_EVERY: Duration = Duration::from_secs(60);
/// The backlog gather reads every graph row, the costliest of the three. The
/// board takes only titles and PR links from it, so it may lag.
const BACKLOG_EVERY: Duration = Duration::from_secs(300);

pub enum Reply {
    /// The poll thread's board after a read. It folds off the UI thread.
    Board(Box<Board>),
    /// The answer door's receipt for question `id`.
    Answered { id: String, receipt: String },
    /// Why the answer door refused question `id`.
    Refused { id: String, why: String },
    /// The composer's send verdict: the porcelain's own line, or why the
    /// send never left.
    Mailed { ok: bool, text: String },
}

/// The machine the app runs on, then its first lead: who the composer's
/// message routes to. A machine with no lead refuses the send.
pub fn owning_lead(fleet: Option<&fno::app_api::client::org_seats::Fleet>) -> Option<String> {
    let machine = fleet?.machines.iter().find(|m| m.local)?;
    machine.leads.first().map(|l| l.name.clone())
}

enum Ask {
    Refresh,
    Answer { id: String, pick: AnswerPick },
    Mail { text: String },
}

/// Read the fleet and the questions, and the backlog unless `backlog` is
/// given, together, and record them into `board`. Returns the backlog read
/// so a caller can reuse it.
pub async fn read(
    board: &mut Board,
    agents: &[AgentRow],
    backlog: Option<Arc<Inputs>>,
) -> Arc<Inputs> {
    let graph = fno::backlog_view::graph_path();
    let gather = async {
        match backlog {
            Some(b) => b,
            None => Arc::new(fno::backlog_model::gather(&graph, agents.to_vec()).await),
        }
    };
    let (fleet, questions, requests, backlog) = tokio::join!(
        fno::app_api::client::org_seats::read_fleet(),
        fno::needs_overlay::questions_now(),
        fno::requests_model::gather(),
        gather,
    );
    let measured_at = chrono::Utc::now().timestamp().max(0) as u64;
    board.record(fleet, questions, &backlog, measured_at, agents);
    board.record_requests(requests);
    backlog
}

/// The poll thread's handle. Dropping it stops the thread at its next wake.
pub struct Poller {
    asks: UnboundedSender<Ask>,
    roster: Arc<Mutex<Vec<AgentRow>>>,
    active: Arc<AtomicBool>,
}

impl Poller {
    /// Start the thread. It reads at once, then every [`POLL_EVERY`] while
    /// the window is active, and again right after each answer and each
    /// activation, so an answered question drops at once and a returning
    /// user sees fresh rows.
    pub fn start() -> std::io::Result<(Poller, UnboundedReceiver<Reply>)> {
        let (asks, mut ask_rx) = unbounded_channel();
        let (out, replies) = unbounded_channel();
        let roster = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicBool::new(true));
        let (shared, awake) = (roster.clone(), active.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("fno-app-hq".into())
            .spawn(move || {
                rt.block_on(async move {
                    let mut backlog: Option<(Arc<Inputs>, Instant)> = None;
                    let mut board = Board::default();
                    let mut due = true;
                    loop {
                        if due {
                            let agents = shared.lock().map(|r| r.clone()).unwrap_or_default();
                            let cached = backlog
                                .as_ref()
                                .filter(|(_, at)| at.elapsed() < BACKLOG_EVERY)
                                .map(|(b, _)| b.clone());
                            let fresh = cached.is_none();
                            let read_back = read(&mut board, &agents, cached).await;
                            if fresh {
                                backlog = Some((read_back, Instant::now()));
                            }
                            if out.send(Reply::Board(Box::new(board.clone()))).is_err() {
                                return;
                            }
                        }
                        due = match tokio::time::timeout(POLL_EVERY, ask_rx.recv()).await {
                            Ok(None) => return,
                            Ok(Some(Ask::Refresh)) => true,
                            Ok(Some(Ask::Answer { id, pick })) => {
                                let reply = match fno::needs_overlay::answer(&id, pick).await {
                                    Ok(receipt) => Reply::Answered { id, receipt },
                                    Err(why) => Reply::Refused { id, why },
                                };
                                if out.send(reply).is_err() {
                                    return;
                                }
                                true
                            }
                            Ok(Some(Ask::Mail { text })) => {
                                let reply = match owning_lead(board.fleet()) {
                                    None => Reply::Mailed {
                                        ok: false,
                                        text: "no lead on this machine".into(),
                                    },
                                    Some(lead) => {
                                        let sent = fno::app_api::mail_door::send(
                                            &lead,
                                            fno::app_api::mail_door::FROM,
                                            &text,
                                        )
                                        .await;
                                        match sent {
                                            fno::app_api::mail_door::MailVerdict::Sent(receipt) => {
                                                Reply::Mailed {
                                                    ok: true,
                                                    text: receipt,
                                                }
                                            }
                                            fno::app_api::mail_door::MailVerdict::Refused(
                                                reason,
                                            ) => Reply::Mailed {
                                                ok: false,
                                                text: reason,
                                            },
                                        }
                                    }
                                };
                                if out.send(reply).is_err() {
                                    return;
                                }
                                true
                            }
                            // An inactive window reads nothing until it returns.
                            Err(_) => awake.load(Ordering::Relaxed),
                        };
                    }
                })
            })?;
        Ok((
            Poller {
                asks,
                roster,
                active,
            },
            replies,
        ))
    }

    /// The roster the next backlog read joins sessions against.
    pub fn set_roster(&self, agents: &[AgentRow]) {
        if let Ok(mut r) = self.roster.lock() {
            *r = agents.to_vec();
        }
    }

    /// Pause the timed reads while the window is inactive; read at once
    /// when it becomes active again.
    pub fn set_active(&self, active: bool) {
        let was = self.active.swap(active, Ordering::Relaxed);
        if active && !was {
            let _ = self.asks.send(Ask::Refresh);
        }
    }

    /// One answer, a choice or the user's words, off the UI thread. The
    /// verdict comes back as [`Reply::Answered`] or [`Reply::Refused`].
    /// False when the poll thread is gone.
    pub fn answer(&self, id: String, pick: AnswerPick) -> bool {
        self.asks.send(Ask::Answer { id, pick }).is_ok()
    }

    /// Route one composer message to the owning lead, off the UI thread.
    /// The verdict comes back as [`Reply::Mailed`]. False when the poll
    /// thread is gone: the caller refuses up front instead of waiting on a
    /// receipt that never lands.
    pub fn mail(&self, text: String) -> bool {
        self.asks.send(Ask::Mail { text }).is_ok()
    }
}
