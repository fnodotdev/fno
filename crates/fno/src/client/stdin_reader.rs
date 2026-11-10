//! The client's one stdin reader: raw chunks to the channel the prefix layer
//! scans.

use std::io::Read;

use tokio::sync::mpsc;

/// Bytes the ground probe consumed that were not its reply - keystrokes
/// typed during the exchange, a partial reply - ride the channel ahead of
/// the reader thread, in order, before stdin is touched. `try_send`, not
/// `send`: this is a sync function, so an awaited send is impossible and a
/// bare `send` future would drop unpolled - the bytes would never enter
/// the channel. The receiver is alive (the caller owns it) and the channel
/// is empty here, so `try_send` cannot fail.
pub(super) fn feed_prefix(tx: &mpsc::Sender<Vec<u8>>, passthrough: Vec<u8>) {
    if !passthrough.is_empty() {
        let _ = tx.try_send(passthrough);
    }
}

/// Pick the terminal's image protocol, then start the reader thread. The
/// order is the point: the protocol query reads the terminal's reply from
/// stdin, and once this thread owns stdin that reply would reach a pane.
pub(super) fn spawn(tx: mpsc::Sender<Vec<u8>>) -> Result<(), String> {
    super::image_layer::detect();
    std::thread::Builder::new()
        .name("fno-mux-stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map(|_| ())
        .map_err(|e| format!("stdin thread: {e}"))
}
