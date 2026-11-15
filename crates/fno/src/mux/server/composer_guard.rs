//! The composer latch: a live draft must never be submitted by another
//! writer's CR. A mail live-inject types an envelope at the composer cursor
//! and submits with a CR; over a half-typed draft that CR lands the draft
//! fused with the envelope (observed 2026-11-15: seven user turns on one
//! lead session arrived with an incoming mail header mid-text, while the
//! operator_typing witness feed the quiet gate reads was silent).
//!
//! Two arms, in-memory on the core loop so a silent witness feed cannot
//! blind them:
//! - the USER arm opens on typed bytes for [`USER_DRAFT_HOLD`] (the C11
//!   `operator_typing` recency), so a delivery refused here queues durable
//!   instead of typing over the draft;
//! - the DELIVERY arm opens on every `PaneSend` write for
//!   [`DELIVERY_SETTLE`], the enter-delay window between an envelope's
//!   text and its CR. User bytes inside the window buffer (bounded) and
//!   replay in order once it expires, so they land after the CR instead of
//!   fusing into the envelope.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

// The parent module's scope: proto types, err codes, RegistryAgent.
use super::*;

/// How long typed bytes hold deliveries off the pane after the last
/// keystroke. Mirrors the C11 `operator_typing` recency (15s) so both
/// readers of "is a draft live" agree on one number.
pub(super) const USER_DRAFT_HOLD: Duration = Duration::from_secs(15);

/// How long a delivery write holds user bytes off the pane. The pane lane's
/// default enter delay is `CR_SETTLE_MS` (800ms); the extra 200ms slack
/// keeps a 1s flush tick from replaying between the envelope text and its
/// CR. An explicit `--enter-delay-ms` override above this window can still
/// submit a draft that flushes early: every packaged harness row reads
/// 800ms or 0.
pub(super) const DELIVERY_SETTLE: Duration = Duration::from_millis(800 + 200);

/// Bound on buffered user bytes per pane. A paste past this cap refuses
/// with a notice instead of growing without bound.
pub(super) const MAX_PENDING_BYTES: usize = 64 * 1024;

/// The refusal a remote viewer sees when their typed send reaches a held
/// (DND) pane. Mirrors the local `pane send` refusal's actionable text so
/// both doors read the same.
pub(super) const HOLD_REFUSAL_TEXT: &str =
    "target is held (DND); the pane takes no live sends while the hold stands: use `fno agents mail send` to queue durable, or `fno agents mail hold --off` to release it";

/// Why buffered bytes wait. The flush pass reads it to decide when the
/// bytes may reach the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BufferReason {
    /// A delivery write opened its settle window; replay after it closes.
    DeliveryHold,
    /// A live relay claim held the write; replay when the claim releases.
    ClaimHold,
}

#[derive(Debug)]
struct PendingInput {
    bytes: VecDeque<Vec<u8>>,
    len: usize,
    reason: BufferReason,
    /// When the first buffered chunk arrived: the never-hold-forever bound
    /// reads this.
    first_at: Instant,
}

/// Never hold typed bytes longer than this, whatever the reason: a wedged
/// delivery or a leaked claim must not eat a message silently. Past this,
/// the flush pass writes the bytes even into the condition that held them.
const MAX_BUFFER_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub(super) struct ComposerGuard {
    /// pane -> the instant the arm expires.
    user_arm: HashMap<u64, Instant>,
    delivery_arm: HashMap<u64, Instant>,
    pending: HashMap<u64, PendingInput>,
}

impl ComposerGuard {
    /// A typed burst (or its Enter) on `pane`. `submit` clears the arm: an
    /// Enter ends the draft.
    pub(super) fn note_user_input(&mut self, pane: u64, now: Instant, submit: bool) {
        if submit {
            self.user_arm.remove(&pane);
        } else {
            self.user_arm.insert(pane, now + USER_DRAFT_HOLD);
        }
    }

    /// A delivery write on `pane` (an envelope line, a drive keystroke).
    /// Each write re-arms, so the envelope's CR re-opens the window the
    /// text opened and the flush always lands after the last write.
    pub(super) fn note_delivery_write(&mut self, pane: u64, now: Instant) {
        self.delivery_arm.insert(pane, now + DELIVERY_SETTLE);
    }

    /// True while a typed draft holds deliveries off `pane`.
    pub(super) fn user_hold(&self, pane: u64, now: Instant) -> bool {
        self.user_arm.get(&pane).is_some_and(|until| *until > now)
    }

    /// True while a delivery write holds user bytes off `pane`.
    pub(super) fn delivery_hold(&self, pane: u64, now: Instant) -> bool {
        self.delivery_arm
            .get(&pane)
            .is_some_and(|until| *until > now)
    }

    /// Queue user bytes for later replay. Later chunks join the same queue
    /// whatever their own reason, so replay order stays typing order.
    /// False when the pane's buffer is over [`MAX_PENDING_BYTES`]: the
    /// caller refuses instead of queueing a loss.
    pub(super) fn buffer_user(
        &mut self,
        pane: u64,
        bytes: &[u8],
        reason: BufferReason,
        now: Instant,
    ) -> bool {
        let entry = self.pending.entry(pane).or_insert_with(|| PendingInput {
            bytes: VecDeque::new(),
            len: 0,
            reason,
            first_at: now,
        });
        if entry.len + bytes.len() > MAX_PENDING_BYTES {
            return false;
        }
        entry.len += bytes.len();
        entry.bytes.push_back(bytes.to_vec());
        true
    }

    /// Whether `pane`'s buffered bytes may reach the pty now: their reason
    /// cleared, or they waited past [`MAX_BUFFER_WAIT`].
    pub(super) fn pending_ready(&self, pane: u64, now: Instant, claim_live: bool) -> bool {
        let Some(entry) = self.pending.get(&pane) else {
            return false;
        };
        let waited = now.duration_since(entry.first_at) >= MAX_BUFFER_WAIT;
        if waited {
            return true;
        }
        match entry.reason {
            BufferReason::DeliveryHold => !self.delivery_hold(pane, now),
            BufferReason::ClaimHold => !claim_live,
        }
    }

    /// Whether `pane` holds buffered user bytes: later bursts must join the
    /// queue, never write ahead of them.
    pub(super) fn holds_pending(&self, pane: u64) -> bool {
        self.pending.contains_key(&pane)
    }

    /// Take `pane`'s buffered bytes for replay.
    pub(super) fn take_pending(&mut self, pane: u64) -> VecDeque<Vec<u8>> {
        self.pending
            .remove(&pane)
            .map(|entry| entry.bytes)
            .unwrap_or_default()
    }

    /// Drop a pane's latch and buffer: called where the pane is reaped.
    pub(super) fn purge(&mut self, pane: u64) {
        self.user_arm.remove(&pane);
        self.delivery_arm.remove(&pane);
        self.pending.remove(&pane);
    }
}

impl super::Core {
    /// The `CoreMsg::PaneSend` door through the composer latch: a live
    /// draft refuses the send (the pane lane maps not-delivered to the
    /// durable bus, so mail queues instead of typing over the draft; drive
    /// callers treat TARGET_NOT_IDLE as transient and retry), and a landed
    /// write opens the settle window that buffers user bytes through the
    /// envelope's CR.
    pub(super) fn pane_send_gated(
        &mut self,
        pane: u64,
        bytes: &[u8],
        guarded: bool,
        expected_identity: Option<&str>,
        agents: Result<Vec<RegistryAgent>, &'static str>,
        hold_pass: bool,
    ) -> ServerMsg {
        if self.composer.user_hold(pane, Instant::now()) {
            return ServerMsg::Err {
                code: err_code::TARGET_NOT_IDLE,
                msg: "composer busy: a draft is being typed; queue the send durable or retry once the draft submits"
                    .to_string(),
            };
        }
        let msg = self.pane_send(pane, bytes, guarded, expected_identity, agents, hold_pass);
        if matches!(msg, ServerMsg::Ok) {
            self.composer.note_delivery_write(pane, Instant::now());
        }
        msg
    }

    /// The 1s flush pass: replay buffered user bytes whose reason cleared.
    pub(super) fn flush_due_composer_buffers(&mut self) {
        let now = Instant::now();
        let ready: Vec<u64> = self
            .composer
            .pending
            .keys()
            .copied()
            .filter(|pane| {
                let claim_live = self
                    .claims
                    .get(pane)
                    .is_some_and(|&holder| super::pid_alive(holder));
                self.composer.pending_ready(*pane, now, claim_live)
            })
            .collect();
        for pane in ready {
            for bytes in self.composer.take_pending(pane) {
                self.write_user_burst(pane, &bytes, None);
            }
        }
    }

    /// The write tail a user burst shares between live input and a replay:
    /// the pty write plus the anchor and echo-watch stamps. `notice_to`
    /// hears a bounced write (queue full); a replay has no client to notice
    /// and logs instead.
    pub(super) fn write_user_burst(
        &mut self,
        focus: u64,
        bytes: &[u8],
        notice_to: Option<u64>,
    ) -> bool {
        if self.buffer_in_flight_input(focus, bytes) {
            return true;
        }
        let Some(entry) = self.panes.get(&focus) else {
            return false;
        };
        match entry.pty.write_input(bytes) {
            Ok(()) => {
                self.stamp_anchor(focus, bytes);
                self.watch_input(focus, bytes);
                true
            }
            Err(crate::pty::PtyError::Write(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                match notice_to {
                    Some(id) => self.notice(id, "pane not accepting input; keys dropped"),
                    None => crate::mux_log!(warn;
                        "fno mux: replayed input held pane={focus}: pty input queue full"),
                }
                false
            }
            Err(error) => {
                crate::mux_log!(error;
                    "fno mux: input failed pane={focus}: {error}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Instant {
        Instant::now() + Duration::from_millis(n)
    }

    #[test]
    fn a_draft_holds_deliveries_and_an_enter_clears_it() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        guard.note_user_input(7, t0, false);
        assert!(guard.user_hold(7, t0 + Duration::from_secs(14)));
        assert!(!guard.user_hold(7, t0 + USER_DRAFT_HOLD + Duration::from_secs(1)));
        guard.note_user_input(7, t0, true);
        assert!(!guard.user_hold(7, t0), "an Enter ends the draft");
    }

    #[test]
    fn a_delivery_write_holds_user_bytes_until_settle_and_replay_reads_the_cr_lands_first() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        guard.note_delivery_write(7, t0);
        assert!(guard.delivery_hold(7, t0 + Duration::from_millis(500)));
        let late = t0 + DELIVERY_SETTLE + Duration::from_millis(1);
        assert!(!guard.delivery_hold(7, late));
        // The envelope's CR re-arms inside the window: the flush that reads
        // the latch after the CR sees the refreshed expiry, so replay can
        // never land between the envelope text and its submit.
        let t_cr = t0 + Duration::from_millis(800);
        guard.note_delivery_write(7, t_cr);
        assert!(guard.delivery_hold(7, t_cr + Duration::from_millis(500)));
    }

    #[test]
    fn user_bytes_buffer_in_order_and_flush_once_the_delivery_settle_expires() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        guard.note_delivery_write(7, t0);
        assert!(guard.buffer_user(7, b"hello ", BufferReason::DeliveryHold, t0));
        // Typing continues after the first chunk: same queue, same reason.
        assert!(guard.buffer_user(7, b"world", BufferReason::DeliveryHold, t0));
        assert!(
            !guard.pending_ready(7, t0 + Duration::from_millis(500), false),
            "inside the settle window the bytes still wait"
        );
        let due = t0 + DELIVERY_SETTLE + Duration::from_millis(1);
        assert!(guard.pending_ready(7, due, false));
        let replay: Vec<u8> = guard.take_pending(7).into_iter().flatten().collect();
        assert_eq!(replay, b"hello world");
        assert!(
            guard.take_pending(7).is_empty(),
            "the flush drains the queue"
        );
    }

    #[test]
    fn claim_held_bytes_flush_when_the_claim_releases_or_the_wait_cap_fires() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        assert!(guard.buffer_user(7, b"q", BufferReason::ClaimHold, t0));
        assert!(!guard.pending_ready(7, t0 + Duration::from_secs(1), true));
        assert!(guard.pending_ready(7, t0 + Duration::from_secs(1), false));
        // The cap: even a live claim cannot hold bytes forever.
        assert!(guard.pending_ready(7, t0 + MAX_BUFFER_WAIT, true));
    }

    #[test]
    fn a_buffer_over_the_cap_refuses_instead_of_growing() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        assert!(!guard.buffer_user(
            7,
            &vec![b'x'; MAX_PENDING_BYTES + 1],
            BufferReason::DeliveryHold,
            t0
        ));
        assert!(
            guard.take_pending(7).is_empty(),
            "the refused chunk left no queue"
        );
    }

    #[test]
    fn purge_drops_every_arm_and_buffer_for_the_pane() {
        let mut guard = ComposerGuard::default();
        let t0 = ms(0);
        guard.note_user_input(7, t0, false);
        guard.note_delivery_write(7, t0);
        assert!(guard.buffer_user(7, b"a", BufferReason::ClaimHold, t0));
        guard.purge(7);
        assert!(!guard.user_hold(7, t0));
        assert!(!guard.delivery_hold(7, t0));
        assert!(guard.take_pending(7).is_empty());
    }
}
