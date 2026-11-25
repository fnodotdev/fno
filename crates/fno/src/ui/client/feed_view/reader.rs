//! The live panel's link to its held reader, `fno agent fleet feed
//! --serve`. The panel sends one request per view change or scroll and the
//! answers land through the fold channel like any fold, so the client never
//! holds the history. Dropping the [`Reader`] closes the reader's stdin, and
//! the reader exits with the panel.

use crate::feed_overlay::{FeedError, Land};
use fno_proto::feed::serve::{Answer, Request};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// The request side of one reader process.
pub(crate) struct Reader {
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
    next_id: u64,
}

impl Reader {
    /// Start the reader. Its answers arrive on `fold` under `gen`; a reader
    /// that exits sends one failure, which an already-closed panel ignores.
    pub(crate) fn spawn(gen: u64, fold: super::FoldTx) -> Result<Reader, FeedError> {
        let mut command = crate::feed_overlay::feed_command(&["agent", "fleet", "feed", "--serve"]);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = crate::process_admission::tokio_spawn(&mut command)
            .map_err(|e| FeedError::Spawn(e.to_string()))?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(FeedError::Spawn("the reader's pipes did not open".into()));
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let Ok(mut line) = serde_json::to_vec(&req) else {
                    continue;
                };
                line.push(b'\n');
                if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let outcome = serde_json::from_str::<Answer>(&line)
                    .map(|a| Land::Answer(Box::new(a)))
                    .map_err(|e| FeedError::Malformed(format!("reader answer: {e}")));
                if fold.send((gen, outcome)).is_err() {
                    return;
                }
            }
            let status = child
                .wait()
                .await
                .map(|s| s.to_string())
                .unwrap_or_default();
            if fold
                .send((
                    gen,
                    Err(FeedError::Exit(format!("the feed reader exited {status}"))),
                ))
                .is_err()
            {
                return;
            }
        });
        Ok(Reader { tx, next_id: 0 })
    }

    /// Send one request under a fresh id. `None` when the reader is gone.
    pub(crate) fn send(&mut self, mut req: Request) -> Option<u64> {
        self.next_id += 1;
        req.id = self.next_id;
        self.tx.send(req).ok().map(|()| self.next_id)
    }
}
