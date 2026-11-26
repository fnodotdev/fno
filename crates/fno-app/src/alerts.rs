//! The app's attention signals outside the window: a macOS notification with
//! click-through, a sound, and the Dock badge.
//!
//! The app derives agent transitions itself from `Event::Layout` diffs. The
//! daemon never sends a blocked alert over the mux wire (its blocked notice
//! rides the `fno inbox notify` lane) and gates its done notice behind
//! `mux.notify_on_done` (default off), so the wire can never carry the full
//! picture; the layout the sidebar already draws does. The diff is
//! enter-only: a state fires once on arrival, the first layout after attach
//! is silent, and an exited row counts as "done" only when its `result` is
//! set, because `DoneUnseen` belongs to a live pane's unseen done badge, and
//! an exit after done-unseen must not announce the same finish twice.
//!
//! A `DesktopNotice` from the other senders (`fno notify`, a pane's OSC
//! hook) still renders. Its wire shape is `(title, body)` with no sender
//! marker, and a done notice from the daemon titles the agent's own name -
//! the same string a thread pane titles its OSC notices. So a notice whose
//! title names an agent in the current layout stays in the corner toast:
//! the lattice diff owns that agent-lifecycle alerts, and without the
//! suppression a `mux.notify_on_done` user would hear every done twice.
//! Everything touching AppKit or UserNotifications handles
//! `#[cfg(target_os = "macos")]` with stub twins, and the UserNotifications
//! leg skips itself when the process is not running from a bundle (an
//! unbundled `cargo run` build has no bundle id for
//! `UNUserNotificationCenter` to key on).
//!
//! The three switches read the global settings file
//! (`~/.fno/settings.yaml`, the `fno config set` default target) as
//! `app.alert_*` booleans, all default on. A Finder-launched app has no
//! project cwd, so project-local config is out of reach by construction;
//! the global read is the one honest read.

use std::collections::HashMap;
use std::time::SystemTime;

use fno::app_api::client::agent_lattice_state;
use fno::app_api::lattice::{status_long, LatticeState};
use fno::proto::AgentRow;

/// Which transition fired. Maps onto the two states the ask names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Blocked,
    Done,
}

impl Kind {
    /// The lattice's own word for the transition, so an alert reads like the
    /// sidebar's detail line.
    pub fn word(&self) -> &'static str {
        status_long(match self {
            Kind::Blocked => LatticeState::Blocked,
            Kind::Done => LatticeState::DoneUnseen,
        })
    }
}

/// One transition worth an alert: agent name, its pane id when the
/// click-through can focus it, and which transition.
pub type Transition = (String, Option<u64>, Kind);

/// The state one agent landed in and whether that landing is an alert.
/// Exited-with-result is a one-shot's done; the guard against a previous
/// `DoneUnseen` keeps one finish from announcing itself twice (the badge
/// flips to done first, the exit stamp lands after).
fn entry_alert(old: LatticeState, new: LatticeState, result: Option<&str>) -> Option<Kind> {
    if old == new {
        return None;
    }
    match new {
        LatticeState::Blocked => Some(Kind::Blocked),
        LatticeState::DoneUnseen => Some(Kind::Done),
        LatticeState::Exited if result.is_some() && old != LatticeState::DoneUnseen => {
            Some(Kind::Done)
        }
        _ => None,
    }
}

/// The stable identity of an agent row across layouts. A pane id survives a
/// rename; the name is the only identity a remote row has.
fn agent_key(a: &AgentRow) -> String {
    match a.pane_id {
        Some(pane) => format!("p{pane}"),
        None => format!("n:{}", a.name),
    }
}

/// Diff one layout against the last: enter-only alerts, first sight silent.
/// Returns the transitions in layout order and the map to store for next
/// time.
pub fn diff(
    prev: Option<&HashMap<String, LatticeState>>,
    agents: &[AgentRow],
) -> (Vec<Transition>, HashMap<String, LatticeState>) {
    let mut out = Vec::new();
    let mut now = HashMap::with_capacity(agents.len());
    for a in agents {
        let key = agent_key(a);
        let state = agent_lattice_state(a);
        now.insert(key.clone(), state);
        if let Some(old) = prev.and_then(|p| p.get(&key)) {
            if let Some(kind) = entry_alert(*old, state, a.result.as_deref()) {
                out.push((a.name.clone(), a.pane_id, kind));
            }
        }
    }
    (out, now)
}

/// How many agents need the user right now: the Dock badge's number.
pub fn badge_count(agents: &[AgentRow]) -> u32 {
    agents
        .iter()
        .filter(|a| agent_lattice_state(a) == LatticeState::Blocked)
        .count() as u32
}

/// The write the Dock needs to move from `current` to `want`: the new
/// value on any transition, None when they already agree. A switch-off
/// (want None) stores None but writes the clear, so a turned-off switch
/// still takes the old count down.
fn badge_transition(want: Option<u32>, current: &mut Option<u32>) -> Option<u32> {
    if want == *current {
        return None;
    }
    *current = want;
    Some(want.unwrap_or(0))
}

/// What a consumed signal asks the window to do.
pub enum Signal {
    Toast(String),
    /// Post a macOS notification; pane_id drives click-through when the
    /// agent is focusable.
    Notify {
        title: String,
        body: String,
        pane_id: Option<u64>,
    },
    Sound(Kind),
}

/// The three switches, read from the global settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefs {
    pub sound: bool,
    pub notification: bool,
    pub badge: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            sound: true,
            notification: true,
            badge: true,
        }
    }
}

fn raw_bool(dict: Option<&serde_json::Value>, key: &str, default: bool) -> bool {
    dict.and_then(|raw| fno::config_cli::raw_get(raw, key))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Parse the three switches out of a merged settings value. Missing keys and
/// wrong types fall to the defaults; a switch the user never wrote stays on.
fn prefs_from(raw: Option<&serde_json::Value>) -> Prefs {
    Prefs {
        sound: raw_bool(raw, "app.alert_sound", true),
        notification: raw_bool(raw, "app.alert_notification", true),
        badge: raw_bool(raw, "app.alert_badge", true),
    }
}

/// The switch read, re-read only when the settings file changes mtime, so a
/// layout push never parses YAML.
struct PrefsCache {
    mtime: Option<SystemTime>,
    prefs: Prefs,
}

impl PrefsCache {
    fn new() -> Self {
        Self {
            mtime: None,
            prefs: Prefs::default(),
        }
    }

    fn get(&mut self) -> Prefs {
        // The mtime watch follows the file the read follows, pins included.
        let path =
            crate::global_config_path().unwrap_or_else(fno::config_cli::global_settings_path);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime != self.mtime {
            self.mtime = mtime;
            let raw = crate::global_config_raw();
            self.prefs = prefs_from(raw.as_ref());
        }
        self.prefs
    }
}

/// The per-window alert state: the switch cache, the lattice map the diff
/// reads, and the badge value already on the Dock (so a no-change layout
/// write once.
pub struct Alerts {
    prefs: PrefsCache,
    seen: Option<HashMap<String, LatticeState>>,
    badge: Option<u32>,
}

impl Default for Alerts {
    fn default() -> Self {
        Self::new()
    }
}

impl Alerts {
    pub fn new() -> Self {
        Self {
            prefs: PrefsCache::new(),
            seen: None,
            badge: None,
        }
    }
}

impl Alerts {
    /// Consume one layout push: transition alerts + Dock badge. `active` is
    /// whether the window is frontmost; sound and the OS notification are
    /// the background form, the corner toast the in-window form.
    pub fn on_layout(&mut self, agents: &[AgentRow], active: bool) -> Vec<Signal> {
        let prefs = self.prefs.get();
        let (transitions, now) = diff(self.seen.as_ref(), agents);
        self.seen = Some(now);
        let mut out = Vec::new();
        for (name, pane_id, kind) in transitions {
            if active {
                // The toast names the agent and says what a click does: the
                // notification tap and the sidebar row both focus that agent.
                out.push(Signal::Toast(format!(
                    "{}: {} - click to focus",
                    name,
                    kind.word()
                )));
            } else {
                if prefs.notification {
                    out.push(Signal::Notify {
                        title: name.clone(),
                        body: kind.word().to_string(),
                        pane_id,
                    });
                }
                if prefs.sound {
                    out.push(Signal::Sound(kind));
                }
            }
        }
        out
    }

    /// Consume one DesktopNotice. A title that names a known agent is the
    /// lattice diff's news (module doc), so it stays in the corner toast and
    /// never as a second OS notification; other titles keep AC behavior.
    pub fn on_desktop(
        &mut self,
        title: &str,
        body: &str,
        known_agent: bool,
        active: bool,
    ) -> Option<Signal> {
        if active || known_agent {
            // A title that names a known agent is focusable, so the note
            // rides; an unknown title's click has nowhere to land.
            let hint = known_agent.then_some(" - click to focus").unwrap_or("");
            return Some(Signal::Toast(format!("{title}: {body}{hint}")));
        }
        if self.prefs.get().notification {
            return Some(Signal::Notify {
                title: title.to_string(),
                body: body.to_string(),
                pane_id: None,
            });
        }
        None
    }

    /// The Dock value to write for this layout, when it changed. Some(0)
    /// clears, and a switch-off transition returns Some(0) too: the Dock
    /// must drop the old count the moment the switch reads off, not wait
    /// for a close.
    pub fn badge(&mut self, agents: &[AgentRow]) -> Option<u32> {
        let want = if self.prefs.get().badge {
            Some(badge_count(agents))
        } else {
            None
        };
        badge_transition(want, &mut self.badge)
    }

    /// The link closed: the count is unknown, so the badge clears and the
    /// lattice map resets (a reconnect re-learns without a burst of alerts).
    pub fn closed(&mut self) -> Option<u32> {
        self.seen = None;
        if self.badge != Some(0) {
            self.badge = Some(0);
            Some(0)
        } else {
            None
        }
    }

    /// Whether the notification switch is on, read fresh for the install
    /// gate at startup.
    pub fn notification_on(&mut self) -> bool {
        self.prefs.get().notification
    }
}

#[cfg(target_os = "macos")]
pub use mac::{beep, install_click, notify, play, set_badge};

#[cfg(not(target_os = "macos"))]
pub fn install_click(_wants_auth: bool) -> Option<tokio::sync::mpsc::UnboundedReceiver<u64>> {
    None
}
#[cfg(not(target_os = "macos"))]
pub fn notify(_title: &str, _body: &str, _pane_id: Option<u64>) {}
#[cfg(not(target_os = "macos"))]
pub fn play(_kind: Kind) {}
#[cfg(not(target_os = "macos"))]
pub fn beep() {}
#[cfg(not(target_os = "macos"))]
pub fn set_badge(_count: u32) {}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CStr, CString};

    use block2::{Block, RcBlock};
    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::{class, msg_send, sel};

    use super::Kind;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    type Id = *mut AnyObject;

    const TX_IVAR: &CStr = c"_fno_tx";

    fn ns(s: &str) -> Id {
        // An interior NUL cannot reach a label or a sound name; it would
        // only truncate the string.
        let c = CString::new(s.replace('\0', "")).unwrap_or_default();
        // SAFETY: stringWithUTF8String: copies the bytes, which live through
        // the call; it returns an autoreleased NSString or nil.
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
    }

    /// UserNotifications keys off a bundle id; a plain `cargo run` binary
    /// has none and the center raises on first use, so the leg skips itself.
    fn bundle_present() -> bool {
        // SAFETY: mainBundle and bundleIdentifier exist on every macOS and
        // return objects (or nil) the null check handles.
        unsafe {
            let bundle: Id = msg_send![class!(NSBundle), mainBundle];
            let bid: Id = msg_send![bundle, bundleIdentifier];
            !bid.is_null()
        }
    }

    /// The UN didReceive method: on a plain click, read the pane id out of
    /// the content's userInfo and hand it to the click channel. The
    /// completion block must always run: the runtime frees the response
    /// only after it does.
    extern "C-unwind" fn did_receive(
        this: &AnyObject,
        _cmd: Sel,
        _center: Id,
        response: Id,
        completion: &Block<dyn Fn()>,
    ) {
        // SAFETY: `response` is the object the notification center passed in;
        // every selector here exists on UNNotificationResponse's chain. The
        // ivar was declared on this class with type usize and set at install.
        unsafe {
            let default_action = ns("com.apple.UNNotificationDefaultActionIdentifier");
            let action: Id = msg_send![response, actionIdentifier];
            let clicked: bool = msg_send![action, isEqualToString: default_action];
            if clicked {
                let notification: Id = msg_send![response, notification];
                let request: Id = msg_send![notification, request];
                let content: Id = msg_send![request, content];
                let user_info: Id = msg_send![content, userInfo];
                let num: Id = msg_send![user_info, objectForKey: ns("pane_id")];
                if !num.is_null() {
                    let pane: i64 = msg_send![num, longLongValue];
                    let ivar = this
                        .class()
                        .instance_variable(TX_IVAR)
                        .expect("the delegate class declares the sender ivar");
                    let tx = *ivar.load::<usize>(this) as *const UnboundedSender<u64>;
                    // A closed window loop has nowhere to put the click;
                    // dropping it is the correct handling.
                    (*tx).send(pane as u64).ok();
                }
            }
        }
        completion.call(());
    }

    /// The delegate class: one registered subclass of NSObject carrying the
    /// UN didReceive method, with the click sender parked in an ivar as a
    /// boxed raw pointer. The delegate lives for the process (the center
    /// holds a strong reference and nothing drops the allocation), so
    /// nothing here ever drops the sender either.
    fn click_delegate_class() -> &'static AnyClass {
        static CELL: std::sync::OnceLock<&'static AnyClass> = std::sync::OnceLock::new();
        CELL.get_or_init(|| {
            let mut decl = ClassBuilder::new(c"FnoAppClickDelegate", class!(NSObject))
                .expect("register FnoAppClickDelegate once");
            let imp = did_receive as extern "C-unwind" fn(_, _, _, _, _);
            // SAFETY: the method's signature matches the delegate selector's
            // (receiver, _cmd, center, response, completion block).
            unsafe {
                // macOS 26 renamed the delegate's completion label; register
                // both spellings and let each system pick its own.
                decl.add_method(
                    sel!(userNotificationCenter:didReceiveNotificationResponse:completionHandler:),
                    imp,
                );
                decl.add_method(
                    sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                    imp,
                );
            }
            decl.add_ivar::<usize>(TX_IVAR);
            decl.register()
        })
    }

    /// Install the delegate and ask for notification authorization once.
    /// `wants_auth` is the `app.alert_notification` switch: a user who
    /// turned the signal off never sees the permission dialog.
    pub fn install_click(wants_auth: bool) -> Option<tokio::sync::mpsc::UnboundedReceiver<u64>> {
        if !bundle_present() {
            return None;
        }
        // SAFETY: every msg_send targets a class/singleton with the selector
        // named; the center is null-checked before use and the delegate is a
        // freshly alloc/init'd instance of the class registered above, whose
        // ivar is declared as usize.
        unsafe {
            let center: Id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
            if center.is_null() {
                return None;
            }
            let (tx, rx) = unbounded_channel();
            let cls = click_delegate_class();
            let obj: Id = msg_send![cls, alloc];
            let obj: Id = msg_send![obj, init];
            let boxed = Box::into_raw(Box::new(tx));
            let ivar = cls
                .instance_variable(TX_IVAR)
                .expect("the delegate class declares the sender ivar");
            *ivar.load_ptr::<usize>(&*obj) = boxed as usize;
            let _: () = msg_send![center, setDelegate: obj];
            if wants_auth {
                // Denied shows nothing; the switch is the user's.
                let granted = RcBlock::new(|_ok: Bool, _err: Id| {});
                let _: () = msg_send![center,
                    requestAuthorizationWithOptions: 4usize,
                    completionHandler: &*granted
                ];
            }
            Some(rx)
        }
    }

    pub fn notify(title: &str, body: &str, pane_id: Option<u64>) {
        if !bundle_present() {
            return;
        }
        // SAFETY: selectors on UNUserNotificationCenter/UNMutableNotificationContent
        // exist on every macOS here; the center is null-checked and the
        // macOS 26 scheduler spelling is respondsToSelector-gated.
        unsafe {
            let center: Id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
            if center.is_null() {
                return;
            }
            let content: Id = msg_send![class!(UNMutableNotificationContent), new];
            let _: () = msg_send![content, setTitle: ns(title)];
            let _: () = msg_send![content, setBody: ns(body)];
            if let Some(pane) = pane_id {
                let info: Id =
                    msg_send![class!(NSMutableDictionary), dictionaryWithCapacity: 1usize];
                let num: Id = msg_send![class!(NSNumber), numberWithLongLong: pane as i64];
                let _: () = msg_send![info, setObject: num, forKey: ns("pane_id")];
                let _: () = msg_send![content, setUserInfo: info];
            }
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos().to_string())
                .unwrap_or_default();
            let request: Id = msg_send![class!(UNNotificationRequest),
                requestWithIdentifier: ns(&nanos),
                content: content,
                trigger: std::ptr::null_mut::<AnyObject>()
            ];
            let no_handler: Option<&Block<dyn Fn(Id)>> = None;
            // macOS 26 renamed the completion label on the scheduler; older
            // systems only carry the pre-rename spelling. Ask the center
            // which one it answers and send that, or skip rather than raise.
            let post26: bool = msg_send![center,
                respondsToSelector: sel!(addNotificationRequest:withCompletionHandler:)
            ];
            if post26 {
                let _: () = msg_send![center,
                    addNotificationRequest: request,
                    withCompletionHandler: no_handler
                ];
            } else {
                let _: () = msg_send![center,
                    addNotificationRequest: request,
                    completionHandler: no_handler
                ];
            }
        }
    }

    pub fn play(kind: Kind) {
        let name = match kind {
            Kind::Blocked => "Tink",
            Kind::Done => "Glass",
        };
        // SAFETY: NSSound responds to soundNamed: and play; soundNamed
        // returns nil for an unknown name and nil is checked.
        unsafe {
            let sound: Id = msg_send![class!(NSSound), soundNamed: ns(name)];
            if !sound.is_null() {
                let _: bool = msg_send![sound, play];
            }
        }
    }

    /// The system alert sound, the user's chosen beep.
    pub fn beep() {
        #[link(name = "AppKit", kind = "framework")]
        extern "C" {
            fn NSBeep();
        }
        // SAFETY: NSBeep takes no arguments and is callable from any thread.
        unsafe { NSBeep() }
    }

    pub fn set_badge(count: u32) {
        // SAFETY: sharedApplication/dockTile/setBadgeLabel exist on every
        // macOS; each receiver is null-checked before use.
        unsafe {
            let app: Id = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let tile: Id = msg_send![app, dockTile];
            if tile.is_null() {
                return;
            }
            let label: Id = if count > 0 {
                ns(&count.to_string())
            } else {
                std::ptr::null_mut()
            };
            let _: () = msg_send![tile, setBadgeLabel: label];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fno::proto::AgentRow;

    fn row(name: &str, pane: u64) -> AgentRow {
        let mut a = AgentRow::default();
        a.name = name.to_string();
        a.pane_id = Some(pane);
        a
    }

    #[test]
    fn first_layout_is_silent_and_stores_the_map() {
        let agents = [row("claude-1", 7)];
        let (hits, map) = diff(None, &agents);
        assert!(hits.is_empty());
        assert_eq!(
            map.get("p7").copied(),
            Some(agent_lattice_state(&agents[0]))
        );
    }

    #[test]
    fn blocked_enters_once_no_repeat_on_identical_layout() {
        let agents = [row("a", 1), row("b", 2)];
        let (_, first) = diff(None, &agents);
        let mut b = agents[1].clone();
        b.badge = Some(fno::proto::AgentBadge::Blocked);
        let blocked = [agents[0].clone(), b];
        let (hits, seen) = diff(Some(&first), &blocked);
        assert_eq!(hits, vec![("b".to_string(), Some(2), Kind::Blocked)]);
        let (hits, _) = diff(Some(&seen), &blocked);
        assert!(
            hits.is_empty(),
            "identical layout must not refire: {hits:?}"
        );
    }

    #[test]
    fn done_unseen_then_exit_does_not_double_fire() {
        let mut a = row("a", 1);
        a.badge = Some(fno::proto::AgentBadge::Done);
        a.seen = false;
        let (_, live) = diff(None, &[row("a", 1)]);
        let (hits, seen) = diff(Some(&live), &[a.clone()]);
        assert_eq!(hits, vec![("a".to_string(), Some(1), Kind::Done)]);
        a.exited = true;
        a.result = Some("done".into());
        let (_, seen) = diff(Some(&seen), &[a.clone()]);
        let (hits, _) = diff(Some(&seen), &[a]);
        assert!(hits.is_empty());
    }

    #[test]
    fn one_shot_exit_announces_only_with_a_result() {
        let mut a = row("one-shot", 3);
        a.exited = true;
        a.result = Some("shipped".into());
        let (_, live) = diff(None, &[row("one-shot", 3)]);
        let (hits, _) = diff(Some(&live), &[]);
        assert!(hits.is_empty(), "row gone = removed, never fired");
        // The same exit with no result stays silent.
        let mut bare = a.clone();
        bare.result = None;
        let (hits, _) = diff(Some(&live), &[bare]);
        assert!(hits.is_empty());
        // The exit carrying a result announces once, then never again.
        let (hits, seen) = diff(Some(&live), &[a.clone()]);
        assert_eq!(hits, vec![("one-shot".to_string(), Some(3), Kind::Done)]);
        let (hits, _) = diff(Some(&seen), &[a]);
        assert!(hits.is_empty());
    }

    #[test]
    fn badge_counts_blocked_only() {
        let mut w = row("w", 1);
        w.badge = Some(fno::proto::AgentBadge::Working);
        let mut b = row("b", 2);
        b.badge = Some(fno::proto::AgentBadge::Blocked);
        let mut d = row("d", 3);
        d.badge = Some(fno::proto::AgentBadge::Done);
        assert_eq!(badge_count(&[w, b, d]), 1);
    }

    #[test]
    fn the_switch_off_transition_writes_the_clear() {
        let mut current = None;
        // First pass with the switch on and one blocked agent: the count.
        assert_eq!(badge_transition(Some(1), &mut current), Some(1));
        // A no-change pass writes nothing.
        assert_eq!(badge_transition(Some(1), &mut current), None);
        // Switch off while the Dock reads 1: the clear goes out at once.
        assert_eq!(badge_transition(None, &mut current), Some(0));
        assert_eq!(current, None);
        // Steady state off: no write, the Dock keeps the clear.
        assert_eq!(badge_transition(None, &mut current), None);
        // Switch back on: the count returns.
        assert_eq!(badge_transition(Some(3), &mut current), Some(3));
    }

    #[test]
    fn switches_default_on_and_override_from_settings() {
        let none = serde_json::json!({});
        assert_eq!(prefs_from(Some(&none)), Prefs::default());
        let mixed = serde_json::json!({"app": {"alert_sound": false}});
        let p = prefs_from(Some(&mixed));
        assert!(!p.sound);
        assert!(p.notification);
        assert!(p.badge);
        assert_eq!(prefs_from(None), Prefs::default());
    }
}
