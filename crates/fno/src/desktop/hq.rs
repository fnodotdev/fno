//! The command center reads, off the UI thread: the fleet, the open
//! questions and the backlog, the same three reads the web bridge feeds into
//! the shared HQ board. Answers go through `needs_overlay::answer`, the one
//! writer the TUI calls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::backlog_model::Inputs;
use crate::needs_overlay::{AnswerPick, QuestionsFold};
use crate::proto::AgentRow;

/// The bridge refreshes every 60 s. The app shows live state, but `read_fleet`
/// may take up to 30 s, so a shorter cycle would overlap reads.
const POLL_EVERY: Duration = Duration::from_secs(10);

/// One cycle's raw reads. The UI thread records them into its board.
pub struct Reading {
    pub fleet: Result<Value, String>,
    pub questions: Result<QuestionsFold, String>,
    pub backlog: Inputs,
    pub measured_at: u64,
}

pub enum Reply {
    Reading(Reading),
    /// The answer door's receipt, or why it refused.
    Answered(Result<String, String>),
}

enum Ask {
    Answer { id: String, option: u32 },
}

/// Read the fleet, the questions and the backlog once, together.
pub async fn read(agents: Vec<AgentRow>) -> Reading {
    let graph = crate::backlog_view::graph_path();
    let (fleet, questions, backlog) = tokio::join!(
        crate::client::org_seats::read_fleet(),
        crate::needs_overlay::questions_now(),
        crate::backlog_model::gather(&graph, agents),
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
}

impl Poller {
    /// Start the thread. It reads at once, then every [`POLL_EVERY`], and
    /// again right after each answer, so an answered question drops at once.
    pub fn start() -> std::io::Result<(Poller, UnboundedReceiver<Reply>)> {
        let (asks, mut ask_rx) = unbounded_channel();
        let (out, replies) = unbounded_channel();
        let roster = Arc::new(Mutex::new(Vec::new()));
        let shared = roster.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        std::thread::Builder::new()
            .name("fno-desktop-hq".into())
            .spawn(move || {
                rt.block_on(async move {
                    loop {
                        let agents = shared.lock().map(|r| r.clone()).unwrap_or_default();
                        if out.send(Reply::Reading(read(agents).await)).is_err() {
                            return;
                        }
                        match tokio::time::timeout(POLL_EVERY, ask_rx.recv()).await {
                            Ok(None) => return,
                            Ok(Some(Ask::Answer { id, option })) => {
                                let receipt =
                                    crate::needs_overlay::answer(&id, AnswerPick::Option(option))
                                        .await;
                                if out.send(Reply::Answered(receipt)).is_err() {
                                    return;
                                }
                            }
                            Err(_) => {}
                        }
                    }
                })
            })?;
        Ok((Poller { asks, roster }, replies))
    }

    /// The roster the next backlog read joins sessions against.
    pub fn set_roster(&self, agents: &[AgentRow]) {
        if let Ok(mut r) = self.roster.lock() {
            *r = agents.to_vec();
        }
    }

    pub fn answer(&self, id: String, option: u32) {
        let _ = self.asks.send(Ask::Answer { id, option });
    }
}
