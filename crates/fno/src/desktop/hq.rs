//! The command center reads, off the UI thread: the fleet, the open
//! questions and the backlog, the same three reads the web bridge feeds into
//! the shared HQ board. Answers go through `needs_overlay::answer`, the one
//! writer the TUI calls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::backlog_model::Inputs;
use crate::needs_overlay::{AnswerPick, QuestionsFold};
use crate::proto::AgentRow;

/// The fleet and question reads, at the bridge's own cadence. Measured on
/// a 38-seat fleet: `needs` takes about 1 CPU-second a call, so a 10 s cycle
/// would hold a tenth of a core.
const POLL_EVERY: Duration = Duration::from_secs(60);
/// The backlog gather reads every graph row, the costliest of the three. The
/// board takes only titles and PR links from it, so it may lag.
const BACKLOG_EVERY: Duration = Duration::from_secs(300);

/// One cycle's raw reads. The UI thread records them into its board.
pub struct Reading {
    pub fleet: Result<Value, String>,
    pub questions: Result<QuestionsFold, String>,
    pub backlog: Arc<Inputs>,
    pub measured_at: u64,
}

pub enum Reply {
    Reading(Reading),
    /// The answer door's receipt, or why it refused.
    Answered(Result<String, String>),
}

enum Ask {
    Refresh,
    Answer { id: String, option: u32 },
}

/// Read the fleet and the questions, and the backlog unless `backlog` is
/// given, together.
pub async fn read(agents: Vec<AgentRow>, backlog: Option<Arc<Inputs>>) -> Reading {
    let graph = crate::backlog_view::graph_path();
    let gather = async {
        match backlog {
            Some(b) => b,
            None => Arc::new(crate::backlog_model::gather(&graph, agents).await),
        }
    };
    let (fleet, questions, backlog) = tokio::join!(
        crate::client::org_seats::read_fleet(),
        crate::needs_overlay::questions_now(),
        gather,
    );
    Reading {
        fleet,
        questions,
        backlog,
        measured_at: chrono::Utc::now().timestamp().max(0) as u64,
    }
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
            .name("fno-desktop-hq".into())
            .spawn(move || {
                rt.block_on(async move {
                    let mut backlog: Option<(Arc<Inputs>, Instant)> = None;
                    let mut due = true;
                    loop {
                        if due {
                            let agents = shared.lock().map(|r| r.clone()).unwrap_or_default();
                            let cached = backlog
                                .as_ref()
                                .filter(|(_, at)| at.elapsed() < BACKLOG_EVERY)
                                .map(|(b, _)| b.clone());
                            let fresh = cached.is_none();
                            let reading = read(agents, cached).await;
                            if fresh {
                                backlog = Some((reading.backlog.clone(), Instant::now()));
                            }
                            if out.send(Reply::Reading(reading)).is_err() {
                                return;
                            }
                        }
                        due = match tokio::time::timeout(POLL_EVERY, ask_rx.recv()).await {
                            Ok(None) => return,
                            Ok(Some(Ask::Refresh)) => true,
                            Ok(Some(Ask::Answer { id, option })) => {
                                let receipt =
                                    crate::needs_overlay::answer(&id, AnswerPick::Option(option))
                                        .await;
                                if out.send(Reply::Answered(receipt)).is_err() {
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

    pub fn answer(&self, id: String, option: u32) {
        let _ = self.asks.send(Ask::Answer { id, option });
    }
}
