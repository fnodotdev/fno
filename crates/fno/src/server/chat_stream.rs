//! A `ClientMsg::ChatTail` connection: one session's chat events, streamed
//! until the client closes. The reader polls files and sqlite in
//! `spawn_blocking`, so the core loop never sees this connection.

use crate::chat_events::{pump, BatchSink, ChatReadError, ChatRoots, SessionReader, IDLE_POLL};
use crate::proto::chat_event::{ChatEvent, SessionKey};
use crate::proto::{check_attach_version, err_code, write_msg, ServerMsg};
use tokio::net::UnixStream;

struct SocketSink<'a> {
    stream: &'a mut UnixStream,
    session: SessionKey,
}

impl BatchSink for SocketSink<'_> {
    async fn send(&mut self, batch: Vec<ChatEvent>) -> bool {
        let msg = ServerMsg::ChatEvents {
            session: self.session.clone(),
            events: batch,
        };
        write_msg(self.stream, &msg).await.is_ok()
    }

    async fn idle(&mut self) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(IDLE_POLL) => true,
            _ = super::wait_for_peer_close(self.stream) => false,
        }
    }
}

pub(super) async fn serve(
    mut stream: UnixStream,
    roots: ChatRoots,
    proto: u32,
    build: String,
    session: SessionKey,
    from_seq: u64,
) {
    if let Err(reason) = check_attach_version(proto, &build) {
        let _ = write_msg(&mut stream, &ServerMsg::Bye { reason }).await;
        return;
    }
    let key = session.clone();
    let opened =
        tokio::task::spawn_blocking(move || SessionReader::open(&key, &roots, from_seq)).await;
    let reader = match opened {
        Ok(Ok(reader)) => reader,
        Ok(Err(error)) => return refuse(&mut stream, error).await,
        Err(_) => return,
    };
    let mut sink = SocketSink {
        stream: &mut stream,
        session,
    };
    if let Err(error) = pump(reader, &mut sink).await {
        refuse(&mut stream, error).await;
    }
}

async fn refuse(stream: &mut UnixStream, error: ChatReadError) {
    let msg = ServerMsg::Err {
        code: err_code::BAD_REQUEST,
        msg: error.to_string(),
    };
    let _ = write_msg(stream, &msg).await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::chat_events::{tail_sync, ChatReadError};
    use crate::proto::chat_event::ChatBody;
    use crate::proto::{read_msg_sync, write_msg_sync, ClientMsg};
    use std::io::Write;

    const SID: &str = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d";

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_stream_serves_batches_then_appends_and_refuses_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        let transcript = projects.join("-repo").join(format!("{SID}.jsonl"));
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"hello"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let socket = dir.path().join("mux.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let roots = ChatRoots {
            transcripts: vec![projects],
            opencode_stores: Vec::new(),
        };
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let roots = roots.clone();
                tokio::spawn(async move {
                    if let Some(ClientMsg::ChatTail {
                        proto,
                        build,
                        session,
                        from_seq,
                    }) = super::super::first_message::read_first(&mut stream).await
                    {
                        serve(stream, roots, proto, build, session, from_seq).await;
                    }
                });
            }
        });

        let key = |harness: &str, id: &str| SessionKey {
            harness: harness.into(),
            session_id: id.into(),
            subagent: None,
        };
        let sock = socket.clone();
        tokio::task::spawn_blocking(move || {
            let mut tail = tail_sync(&sock, key("claude", SID), 0).unwrap();
            let first = tail.next().unwrap().unwrap();
            assert_eq!(first.len(), 4, "{first:#?}");
            assert!(matches!(first[0].body, ChatBody::UserMessage { .. }));
            let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
            writeln!(
                f,
                r#"{{"type":"assistant","uuid":"a2","message":{{"id":"m2","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{}}}}]}}}}"#
            )
            .unwrap();
            let next = tail.next().unwrap().unwrap();
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].seq, 4);
            assert!(matches!(next[0].body, ChatBody::ToolCalled { .. }));

            let resumed = tail_sync(&sock, key("claude", SID), 4).unwrap().next().unwrap().unwrap();
            assert_eq!(resumed[0].seq, 4);

            for (k, needle) in [
                (key("pi", SID), "\"pi\""),
                (key("claude", "ffffffff-0000-4000-8000-000000000000"), "ffffffff-0000"),
            ] {
                let mut tail = tail_sync(&sock, k, 0).unwrap();
                match tail.next() {
                    Some(Err(ChatReadError::Refused(msg))) => assert!(msg.contains(needle), "{msg}"),
                    other => panic!("expected a refusal, got {other:?}"),
                }
                assert!(tail.next().is_none());
            }

            let mut old = std::os::unix::net::UnixStream::connect(&sock).unwrap();
            write_msg_sync(
                &mut old,
                &ClientMsg::ChatTail {
                    proto: 1,
                    build: "0.0.1".into(),
                    session: key("claude", SID),
                    from_seq: 0,
                },
            )
            .unwrap();
            match read_msg_sync::<_, ServerMsg>(&mut old).unwrap() {
                ServerMsg::Bye { reason } => assert!(reason.contains('1'), "{reason}"),
                other => panic!("expected Bye, got {other:?}"),
            }
        })
        .await
        .unwrap();
    }
}
