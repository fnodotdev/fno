//! The offscreen shot mode: one demo frame, one png, no visible window.
use std::ffi::{c_char, c_void, CStr, CString};
use std::time::Duration;

use gpui::{point, px, size, App, Bounds, WindowBounds, WindowOptions};
// objc 0.2 macros expand textually at the call site: msg_send! calls sel!
// and sel! calls sel_impl!, so all three names must be imported here.
use objc::{class, msg_send, sel, sel_impl};

use crate::{MainView, ShotOptions};

/// The pinned palette spellings: the footnote twins. `paper` paints light
/// (bg 0xf7f7f7) and `superscript` is its dark twin; a wrong mapping is a
/// look defect, not a crash, so CI shots catch it.
const THEME_DARK: &str = "footnote-superscript";
const THEME_LIGHT: &str = "footnote-paper";

/// The shot window's frame in points.
const SHOT_SIZE: (f32, f32) = (1280.0, 800.0);

/// How long the shot waits for the board feeds to settle. The questions
/// read alone may take its own 30 s timeout on a busy machine, so the
/// budget sits above it: a frame is never captured while a section still
/// reads "reading…".
const SETTLE_BUDGET: Duration = Duration::from_secs(45);

/// The poll cadence while the feeds settle.
const SETTLE_POLL: Duration = Duration::from_millis(150);

/// How long the pre-warm waits for the demo world's first layout before
/// it gives up.
const PREWARM: Duration = Duration::from_secs(5);

pub fn run(opts: ShotOptions) -> i32 {
    let hold = opts.hold;
    // The pin resolves before the theme pin chdirs: the newest lookup
    // reads the cwd's project slug, and the pin dir would slug one dir
    // off the world's seed.
    let pinned = opts.chat.as_deref().and_then(crate::chat::resolve_key);
    let (Some(session), Some(_pin)) = (shot_session(&opts), theme_pin(&opts.theme)) else {
        return 2;
    };
    let mode = opts.mode.unwrap_or(MainView::Orchestration);
    let find = opts.find;
    let out = opts.out.clone();
    let family = opts.font.clone();
    let font_size = opts.font_size;
    let first_run = opts.first_run;
    let empty_chat = opts.empty_chat || first_run == Some(crate::FirstRunShot::KeyCard);
    let cmd = opts.cmd;
    let tap = opts.tap;
    let ui = opts.ui;
    let sheet = opts.sheet;
    let draft = opts.draft.clone();
    let harness = opts.harness.clone();
    // Pre-warm before gpui starts: connect and drain to the world's
    // first layout on plain sockets, so the one frame the window paints
    // synchronously at open carries the world's data. gpui draws once at
    // open and only re-draws when the window server composites the
    // window (the display link gates on occlusion), so the first frame
    // is the shot.
    let warmed = match prewarm(&session, &opts) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("fno-app --shot: the world did not populate: {e}");
            return 1;
        }
    };
    gpui::Application::new().run(move |cx: &mut App| {
        // Accessory policy first. gpui pins the Regular policy at
        // finish-launching, so a Dock icon and the launch-activation
        // channel would otherwise open for every shot process. The shot
        // never activates: no Dock icon, no menu bar, and the window
        // below never becomes key (focus: false), so the user's key
        // window is untouched through the whole run.
        // SAFETY: plain read on this process's own NSApplication.
        let app: *mut objc::runtime::Object =
            unsafe { msg_send![class!(NSApplication), sharedApplication] };
        // SAFETY: plain setter on this process's own NSApplication; the
        // value 1 is NSApplicationActivationPolicyAccessory, so the
        // process never activates and no Dock icon appears.
        let _: () = unsafe { msg_send![app, setActivationPolicy: 1i64] };
        let out = out.clone();
        let family = family.clone();
        let open = cx.open_window(shot_window_options(), move |window, cx| {
            crate::window::root_for_shot(
                &session,
                mode,
                pinned,
                warmed,
                family,
                font_size,
                find,
                opts.settings,
                opts.update,
                empty_chat,
                cmd,
                sheet,
                draft,
                harness,
                first_run,
                window,
                cx,
            )
        });
        let Ok(handle) = open else {
            eprintln!("fno-app --shot: the window did not open");
            quit_shot(cx, 1);
            return;
        };
        // The fade lands in the same run-loop turn as the open, before
        // the window server's first composite: alpha one percent, mouse
        // events pass through, floating level. The user's screen shows
        // no frame of the birth. The tile color-precision lock does not
        // need a full-alpha birth: the lock re-arms on any full-alpha
        // present (measured: a window faded at open captures 244k
        // distinct colors after the capture phase's raise, identical to
        // an always-full-alpha control), so the birth composite's alpha
        // is irrelevant. What the capture needs is a composited window
        // at visible occlusion, which the floating level keeps true
        // under covering windows. Off-screen birth is not an option:
        // an off-screen window never composites (gpui's display link
        // gates on the occlusion visible bit) and the capture tool
        // cannot image it at all (both measured).
        // SAFETY: NSApp enumeration over this process's own windows only.
        let Some((_, _, occl, w)) = (unsafe { shot_window_id() }) else {
            eprintln!("fno-app --shot: the shot window did not open");
            quit_shot(cx, 1);
            return;
        };
        // SAFETY: setter on the window this process owns; the floating
        // level keeps occlusion visible under covering windows.
        let _: () = unsafe { msg_send![w, setLevel: 3i64] };
        // SAFETY: setter on the window this process owns; mouse events
        // pass through, so nothing the user clicks reaches the window.
        let _: () = unsafe { msg_send![w, setIgnoresMouseEvents: true] };
        // SAFETY: setter on the window this process owns; sharing stays
        // read-only, the spelling the capture service accepts.
        let _: () = unsafe { msg_send![w, setSharingType: 1usize] };
        // SAFETY: setter on the window this process owns; one-percent
        // alpha composites the window without showing it.
        let _: () = unsafe { msg_send![w, setAlphaValue: 0.01f64] };
        // The read-back shows what the window itself claims, so a
        // reverted setter never hides behind the server-side read.
        // SAFETY: plain read of the window's own level claim.
        let level_after: i64 = unsafe { msg_send![w, level] };
        // SAFETY: plain read of the window's own alpha claim.
        let alpha_after: f64 = unsafe { msg_send![w, alphaValue] };
        eprintln!(
            "fno-app --shot: window birth at one percent: level={level_after} alpha={alpha_after} occlusion={occl}"
        );
        cx.spawn(async move |cx| {
            // The board feeds land after the first paint. Poll the
            // board's settled flag, refresh once the feeds land, then
            // capture. The display link keeps running (the window stays
            // composited at visible occlusion), so the refresh presents
            // for real. The optional tap posts after the settle so the
            // fold it opens presents before the capture.
            if !settle_then_tap(cx, &handle, w, tap, ui).await {
                eprintln!("fno-app --shot: the board feeds never settled; no frame captured");
                remove_pin_dir(&pin_dir());
                cx.update(|app| app.quit()).ok();
                std::process::exit(1);
            }
            // Hold mode: leave the near-invisible composited window up
            // for a foreign capturer, skip the raise and the capture.
            // A foreign capture during hold reads one-percent precision
            // (the raise is skipped): enough for layout, never for a
            // color proof. Diagnostics only; the shot door never sets it.
            let code = if hold {
                eprintln!("fno-app --shot: hold mode: the capture is skipped");
                std::thread::sleep(std::time::Duration::from_secs(8));
                0
            } else {
                // The capture phase: the run's one full-alpha present.
                // The server's composited tile re-composites only when a
                // frame presents, and its color precision keys to the
                // alpha at composite time (measured: 40-43 distinct
                // colors captured at one-percent alpha, 244k at full
                // alpha), so raise alpha, present one fresh frame,
                // capture, restore. No PRESENT wait: the capture tool's
                // own spawn latency covers the composite (measured: full
                // precision with the capture fired right after the
                // refresh). This instant is the shot's only visible
                // window, about the capture tool's runtime; the recorded
                // worker decision for this node keeps it, with the
                // refusal branch as the recorded reversal.
                // SAFETY: setter on the window this process owns.
                let _: () = unsafe { msg_send![w, setAlphaValue: 1.0f64] };
                handle.update(cx, |_, window, _| window.refresh()).ok();
                let code = capture_to_png(&out);
                // SAFETY: symmetric restore of the raise above.
                let _: () = unsafe { msg_send![w, setAlphaValue: 0.01f64] };
                code
            };
            if code != 0 {
                eprintln!("fno-app --shot: the capture failed");
            }
            cx.update(|app| quit_shot(app, code)).ok();
        })
        .detach();
    });
    0
}

/// End the shot with `code`: the pin dir goes first. A quit ends the
/// process before any drop runs, so the guard never fires on a quit. A
/// failed shot exits non-zero, or the quit would return 0 and hide it.
fn quit_shot(cx: &mut App, code: i32) {
    remove_pin_dir(&pin_dir());
    cx.quit();
    if code != 0 {
        std::process::exit(code);
    }
}

/// The shot window: a fixed, undecorated 1280x800 tile at (60, 60).
fn shot_window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(60.0), px(60.0)),
            size(px(SHOT_SIZE.0), px(SHOT_SIZE.1)),
        ))),
        titlebar: None,
        focus: false,
        show: true,
        is_movable: false,
        is_resizable: false,
        ..Default::default()
    }
}

/// This process's theme-pin dir. The leaf name is fixed: the workspace
/// row paints the cwd's name, so a pid in it would differ between frames.
/// The pid lives in the parent, which keeps concurrent shots apart.
fn pin_dir() -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!("fno-shot-pin-{}", std::process::id()))
        .join("demo")
}

/// Delete the pin dir and its per-process parent. Unix removes a
/// directory the process still sits in.
fn remove_pin_dir(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir.parent().unwrap_or(dir)).ok();
}

/// Deletes the pin dir when the shot run returns.
struct PinGuard(std::path::PathBuf);

impl Drop for PinGuard {
    fn drop(&mut self) {
        remove_pin_dir(&self.0);
    }
}

/// Write the `[mux] theme` pin the ladder reads into `dir`.
fn write_pin(dir: &std::path::Path, theme: &str) -> std::io::Result<()> {
    std::fs::remove_dir_all(dir).ok();
    std::fs::create_dir_all(dir.join(".fno"))?;
    let name = match theme {
        "light" => THEME_LIGHT,
        "dark-inverse" => "footnote-inverse",
        "light-inverse" => "footnote-inverse-paper",
        _ => THEME_DARK,
    };
    let pin = format!("[mux]\ntheme = \"{name}\"\n");
    std::fs::write(dir.join(".fno/config.toml"), pin)
}

/// Write the pin in a temp dir the process then chdirs into. A pin beats
/// the system appearance, so the shot paints the asked twin on any host.
/// The guard removes the dir when the run returns; a quit ends the process
/// first, so every quit removes it by hand. A failure prints and reads
/// `None`.
fn theme_pin(theme: &str) -> Option<PinGuard> {
    let dir = pin_dir();
    let guard = PinGuard(dir.clone());
    let set = write_pin(&dir, theme).and_then(|()| std::env::set_current_dir(&dir));
    if let Err(e) = set {
        eprintln!("fno-app --shot: {e}");
        return None;
    }
    Some(guard)
}

/// The session the frame attaches to, or `None` when the caller named
/// none. The no-server frame must not meet a real server, so it attaches
/// to a session no mux was ever started for. Any other frame needs a
/// named session: a fallback to `default` photographed the user's live
/// mux unasked.
fn shot_session(opts: &ShotOptions) -> Option<String> {
    let session = opts.session.clone().or_else(|| {
        (opts.first_run == Some(crate::FirstRunShot::NoServer))
            .then(|| format!("shot-no-server-{}", std::process::id()))
    });
    if session.is_none() {
        eprintln!("fno-app --shot: needs --session <name> of a throwaway world; the default session is the user's live mux");
    }
    session
}

/// The pre-warm: connect the link and drain events until the world's
/// first `Layout` arrives or the budget runs out. Returns the live link,
/// every event drained before the layout, and the receiver so the root
/// adopts the stream mid-flight.
/// The warmed link, or `None` for the no-server frame: it has no world to
/// wait for and attaches to a socket nobody listens on, so the offer paints.
fn prewarm(
    session: &str,
    opts: &ShotOptions,
) -> Result<Option<crate::link::WarmedLink>, crate::link::LinkError> {
    if opts.first_run == Some(crate::FirstRunShot::NoServer) {
        return Ok(None);
    }
    let seat = crate::link::Seat::for_shot(opts.own_world);
    crate::link::connect_warmed(session, PREWARM, seat).map(Some)
}

// The window-server reads the proof needs. CGWindowListCopyWindowInfo
// stays supported for window metadata; only CGWindowListCreateImage (the
// image fetch, replaced by ScreenCaptureKit below) is obsoleted. The
// window name reads only with screen-recording permission, so its
// absence is expected, never an error.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
    static kCGWindowNumber: *const c_void;
    static kCGWindowName: *const c_void;
    static kCGWindowBounds: *const c_void;
    static kCGWindowLayer: *const c_void;
    static kCGWindowAlpha: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValueIfPresent(
        dict: *const c_void,
        key: *const c_void,
        value: *mut *const c_void,
    ) -> bool;
    fn CFNumberGetValue(number: *const c_void, the_type: isize, value_ptr: *mut c_void) -> bool;
    fn CFRelease(cf: *const c_void);
}

/// SAFETY: `dict` is a CFDictionaryRef from CGWindowListCopyWindowInfo
/// and `key` a CFStringRef constant; CFNumberGetValue converts the
/// stored number to i64 itself.
unsafe fn cf_i64(dict: *const c_void, key: *const c_void) -> Option<i64> {
    if dict.is_null() {
        return None;
    }
    let mut v: *const c_void = std::ptr::null();
    if CFDictionaryGetValueIfPresent(dict, key, &mut v) && !v.is_null() {
        let mut out: i64 = 0;
        if CFNumberGetValue(v, 4, &mut out as *mut i64 as *mut c_void) {
            return Some(out);
        }
    }
    None
}

/// SAFETY: same contract as cf_i64, with a float64 conversion.
unsafe fn cf_f64(dict: *const c_void, key: *const c_void) -> Option<f64> {
    if dict.is_null() {
        return None;
    }
    let mut v: *const c_void = std::ptr::null();
    if CFDictionaryGetValueIfPresent(dict, key, &mut v) && !v.is_null() {
        let mut out: f64 = 0.0;
        if CFNumberGetValue(v, 6, &mut out as *mut f64 as *mut c_void) {
            return Some(out);
        }
    }
    None
}

/// SAFETY: `dict` is a CFDictionaryRef from CGWindowListCopyWindowInfo;
/// the CFString value is read as UTF-8 before anything is released.
unsafe fn cf_string(dict: *const c_void, key: *const c_void) -> Option<String> {
    let mut v: *const c_void = std::ptr::null();
    if CFDictionaryGetValueIfPresent(dict, key, &mut v) && !v.is_null() {
        let c: *const c_char = msg_send![v as *mut objc::runtime::Object, UTF8String];
        if !c.is_null() {
            return Some(CStr::from_ptr(c).to_string_lossy().into_owned());
        }
    }
    None
}

/// An NSString for the CG bounds-dictionary keys ("X", "Y", ...). The
/// CString is bound before its pointer enters msg_send, so the bytes
/// live through the call.
unsafe fn ns_string(s: &str) -> *const c_void {
    let c = CString::new(s).expect("constant key carries no interior NUL");
    let s: *mut objc::runtime::Object =
        msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
    s as *const c_void
}

/// Wait for the board feeds to settle, then post the optional tap and
/// let the frame it opened present before the capture. Returns false
/// when the feeds never settled, so the caller refuses the frame.
async fn settle_then_tap(
    cx: &mut gpui::AsyncApp,
    handle: &gpui::WindowHandle<crate::window::Root>,
    w: *mut objc::runtime::Object,
    tap: Option<(f64, f64)>,
    ui: Option<crate::UiShot>,
) -> bool {
    if !wait_settled(cx, handle).await {
        return false;
    }
    // The overlay opens through the same Root method a click calls.
    if let Some(ui) = ui {
        handle
            .update(cx, |root, window, cx| root.shot_ui(ui, window, cx))
            .ok();
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        handle.update(cx, |root, _, _| root.shot_ui_after(ui)).ok();
        cx.background_executor()
            .timer(Duration::from_millis(400))
            .await;
        handle.update(cx, |_, window, _| window.refresh()).ok();
    }
    // The tap: one real click inside the shot window before the
    // capture. The events queue in the app and run its own mouse
    // path, so the fold opens the way a user's click opens it,
    // not through a seeded state.
    if let Some((tx, ty)) = tap {
        // SAFETY: reads on the window this process owns.
        unsafe { tap_window_point(w, tx, ty) };
        // The yield lets the run loop dispatch the queued click.
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        handle.update(cx, |_, window, _| window.refresh()).ok();
        cx.background_executor()
            .timer(Duration::from_millis(150))
            .await;
    }
    true
}

/// Poll until the board has read every feed and the chat's event count
/// held still for one poll, or the budget runs out. The capture wants
/// the settled feeds in the model the frame renders. Returns whether it
/// settled.
async fn wait_settled(
    cx: &mut gpui::AsyncApp,
    handle: &gpui::WindowHandle<crate::window::Root>,
) -> bool {
    let deadline = std::time::Instant::now() + SETTLE_BUDGET;
    let mut settled = false;
    let mut last_events = None;
    while std::time::Instant::now() < deadline {
        let read = handle.update(cx, |root, _, _| {
            (root.board.read_yet(), root.chat.conv.events)
        });
        if let Ok((read_yet, events)) = read {
            // The board feeds land first; the chat stream keeps folding
            // events after that. Settled means the board is read AND the
            // chat's event count held still for one poll, so the last
            // assistant rows are in the model the frame renders.
            if read_yet && last_events == Some(events) {
                settled = true;
                break;
            }
            last_events = Some(events);
        }
        cx.background_executor().timer(SETTLE_POLL).await;
    }
    settled
}

/// The window-server truth about the shot window at a named moment:
/// layer, alpha and bounds from CGWindowListCopyWindowInfo, read before
/// and after the capture so the proof shows the window lived across it.
/// Returns whether the id was present.
fn log_window_live(window_id: u32, when: &str) -> bool {
    // SAFETY: the list is a Copy-rule result released below; every value
    // read is consumed before the release.
    let list = unsafe { CGWindowListCopyWindowInfo(0, 0) };
    if list.is_null() {
        eprintln!("fno-app --shot: window-live[{when}]: the window server returned no list");
        return false;
    }
    // SAFETY: indices stay inside 0..count of the live list, and every
    // value read out of it is consumed before the release below.
    let found = unsafe {
        let n = CFArrayGetCount(list);
        let mut hit = false;
        for i in 0..n {
            let dict = CFArrayGetValueAtIndex(list, i);
            if cf_i64(dict, kCGWindowNumber) == Some(window_id as i64) {
                let mut b: *const c_void = std::ptr::null();
                CFDictionaryGetValueIfPresent(dict, kCGWindowBounds, &mut b);
                let x = cf_f64(b, ns_string("X")).unwrap_or(-1.0);
                let y = cf_f64(b, ns_string("Y")).unwrap_or(-1.0);
                let w = cf_f64(b, ns_string("Width")).unwrap_or(-1.0);
                let h = cf_f64(b, ns_string("Height")).unwrap_or(-1.0);
                let layer = cf_i64(dict, kCGWindowLayer).unwrap_or(i64::MIN);
                let alpha = cf_f64(dict, kCGWindowAlpha).unwrap_or(-1.0);
                let name = cf_string(dict, kCGWindowName);
                eprintln!(
                    "fno-app --shot: window-live[{when}]: id={window_id} layer={layer} alpha={alpha} bounds=({x:.0},{y:.0} {w:.0}x{h:.0}) name={name:?}"
                );
                hit = true;
                break;
            }
        }
        if !hit {
            eprintln!(
                "fno-app --shot: window-live[{when}]: id={window_id} ABSENT from the window-server list ({n} windows)"
            );
        }
        hit
    };
    // SAFETY: releases the Copy-rule list read above; nothing borrows it.
    unsafe { CFRelease(list) };
    // The on-screen-only list is the liveness read: the window must sit
    // in the server's list at capture time (it does; the proof line
    // reads true). Invisibility comes from the alpha, not the list.
    // SAFETY: a Copy-rule result, released after the read below.
    let only = unsafe { CGWindowListCopyWindowInfo(1, 0) };
    if !only.is_null() {
        // SAFETY: indices stay inside 0..count of the live list.
        let present = unsafe {
            let n = CFArrayGetCount(only);
            let mut hit = false;
            for i in 0..n {
                let d = CFArrayGetValueAtIndex(only, i);
                if cf_i64(d, kCGWindowNumber) == Some(window_id as i64) {
                    hit = true;
                    break;
                }
            }
            hit
        };
        eprintln!("fno-app --shot: window-live[{when}]: on-screen-only list: {present}");
        // SAFETY: releases the Copy-rule list read above; nothing borrows it.
        unsafe { CFRelease(only) };
    }
    found
}

/// Capture the app's window buffer at one-percent alpha. The window
/// server composites the ordered window at the normal level, so the
/// buffer holds the painted frame and gpui's display link keeps
/// presenting, while the user's screen shows nothing perceptible (one
/// percent alpha, mouse events pass through) and no focus moves (the
/// window never becomes key). The capture runs in Apple's SCK-backed
/// tool: the in-process ScreenCaptureKit reply decode crashes this
/// binary in every process shape it tried, while the same window
/// captures cleanly from any foreign process. The proof lines land on
/// stderr: the TCC preflight read, the window-server liveness
/// before and after the capture, and the helper's own lines.
fn capture_to_png(out: &std::path::Path) -> i32 {
    // SAFETY: NSApp enumeration over this process's own windows only.
    let Some((window_id, _scale, occlusion, _w)) = (unsafe { shot_window_id() }) else {
        eprintln!("fno-app --shot: no capturable fno window in this process");
        return 1;
    };
    // The occlusion visible bit is the settled frame's precondition: a
    // window covering the shot rect stops gpui's display link, and the
    // buffer freezes at first paint. Fail loudly instead of handing a
    // first-paint frame off as a settled one.
    if occlusion & 2 == 0 {
        eprintln!(
            "fno-app --shot: the shot window reads fully occluded (occlusion={occlusion}); a window covers the shot rect, so the frame would hold first paint"
        );
        return 1;
    }
    // SAFETY: a plain read; it returns a bool and touches no pointer.
    let preflight = unsafe { CGPreflightScreenCaptureAccess() };
    eprintln!("fno-app --shot: preflight screen-capture access: {preflight}");
    let before = log_window_live(window_id, "before-capture");
    std::thread::sleep(std::time::Duration::from_millis(200));
    // The caller raised the window to full alpha and presented a fresh
    // frame before invoking this, so the server's composited tiles hold
    // full-precision pixels under a full-alpha window (see the spawn's
    // capture phase). The capture runs in Apple's own SCK-backed tool:
    // the in-process SCScreenshotManager reply decode crashes this
    // binary in every process shape it tried (gpui and helper alike),
    // while the same window captures cleanly from any foreign process.
    // -o drops the shadow margins, -x stays silent.
    let tool = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-o", "-x", "-l", &window_id.to_string()])
        .arg(out)
        .status();
    match tool {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!(
                "fno-app --shot: the capture tool exited {:?} (window lived before: {before})",
                s.code()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("fno-app --shot: the capture tool failed to run: {e}");
            return 1;
        }
    }
    let after = log_window_live(window_id, "after-capture");
    if !after {
        eprintln!("fno-app --shot: the window left the window-server list during the capture");
    }
    if let Err(e) = flatten_png(out) {
        eprintln!("fno-app --shot: the frame failed to flatten: {e}");
        return 1;
    }
    println!("shot wrote {}", out.display());
    0
}

/// Why a flatten fails: the file layer, the png codec, or a capture
/// shape the flattener does not parse. Typed, per R6.
#[derive(Debug)]
enum FrameError {
    Io(std::io::Error),
    Png(String),
    Shape,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "{e}"),
            FrameError::Png(e) => write!(f, "png: {e}"),
            FrameError::Shape => write!(f, "the capture is not RGBA8"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Composite the captured frame onto the theme fill. The window server
/// bakes the window's one-percent alpha into the capture's own alpha
/// channel, so a viewer composites the frame on white and reads it
/// nearly blank. Every pixel blends over `bg` and lands fully opaque.
fn flatten_png(path: &std::path::Path) -> Result<(), FrameError> {
    let file = std::fs::File::open(path).map_err(FrameError::Io)?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder
        .read_info()
        .map_err(|e| FrameError::Png(e.to_string()))?;
    let info = reader.info().clone();
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(FrameError::Shape);
    }
    let Some(size) = reader.output_buffer_size() else {
        return Err(FrameError::Shape);
    };
    let mut buf = vec![0u8; size];
    reader
        .next_frame(&mut buf)
        .map_err(|e| FrameError::Png(e.to_string()))?;
    // The capture phase reads full alpha, so the RGB holds the render as
    // painted; this pin stays as the backstop that makes any residual
    // alpha in the tool's output opaque. Blend or rescale the RGB and
    // the content crushes toward the background, so the flatten only
    // ever pins alpha and keeps the pixels.
    for px in buf.chunks_exact_mut(4) {
        let [_, _, _, a] = px else {
            continue;
        };
        *a = 255;
    }
    let file = std::fs::File::create(path).map_err(FrameError::Io)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), info.width, info.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| FrameError::Png(e.to_string()))?;
    writer
        .write_image_data(&buf)
        .map_err(|e| FrameError::Png(e.to_string()))?;
    writer
        .finish()
        .map_err(|e| FrameError::Png(e.to_string()))?;
    Ok(())
}

/// One real left click at the window-relative logical point. The down
/// and up go into NSApp's own event queue, addressed to the shot window
/// by number, so AppKit dispatches them to the view's mouseDown and
/// mouseUp exactly as it would a user's click. The window server never
/// hit-tests them. A CGEvent posted to the pid at the same point left
/// the fold shut in both themes (measured). The click-through flag stays up the whole
/// time, so no user click can reach the shot window. The events only
/// dispatch once the caller yields to the run loop. SAFETY: reads and
/// class-method calls on the window this process owns.
unsafe fn tap_window_point(w: *mut objc::runtime::Object, tx: f64, ty: f64) {
    let frame: core_graphics::geometry::CGRect = msg_send![w, frame];
    let num: i64 = msg_send![w, windowNumber];
    // AppKit window coordinates put the origin at the bottom left.
    let at = core_graphics::geometry::CGPoint {
        x: tx,
        y: frame.size.height - ty,
    };
    eprintln!(
        "fno-app --shot: tap window ({tx},{ty}) -> window-local ({:.0},{:.0})",
        at.x, at.y
    );
    let app: *mut objc::runtime::Object = msg_send![class!(NSApplication), sharedApplication];
    // NSEventTypeLeftMouseDown=1, NSEventTypeLeftMouseUp=2.
    for kind in [1u64, 2u64] {
        let ev: *mut objc::runtime::Object = msg_send![class!(NSEvent),
            mouseEventWithType: kind
            location: at
            modifierFlags: 0u64
            timestamp: 0f64
            windowNumber: num
            context: std::ptr::null::<c_void>()
            eventNumber: 0i64
            clickCount: 1i64
            pressure: 1f32];
        if ev.is_null() {
            eprintln!("fno-app --shot: the tap event did not create");
            return;
        }
        let _: () = msg_send![app, postEvent: ev atStart: false];
    }
}

/// The app's one window and its backing scale, found through
/// NSApplication's own list: the process is single-window, so the first
/// entry with a positive window number is the shot. This read only
/// enumerates; the visibility setters (floating level, mouse-events
/// pass-through, read-only sharing, one-percent alpha) live in run(),
/// applied in the same run-loop turn as the open so the birth never
/// shows.
unsafe fn shot_window_id() -> Option<(u32, f64, usize, *mut objc::runtime::Object)> {
    let app: *mut objc::runtime::Object = msg_send![class!(NSApplication), sharedApplication];
    let windows: *mut objc::runtime::Object = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    let mut chosen: Option<(*mut objc::runtime::Object, i64)> = None;
    for i in 0..count {
        let w: *mut objc::runtime::Object = msg_send![windows, objectAtIndex: i];
        let num: i64 = msg_send![w, windowNumber];
        if num <= 0 {
            continue;
        }
        let frame: core_graphics::geometry::CGRect = msg_send![w, frame];
        eprintln!(
            "fno-app --shot: window candidate: number={num} frame=({:.0},{:.0} {:.0}x{:.0})",
            frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
        );
        let is_shot =
            frame.size.width == SHOT_SIZE.0 as f64 && frame.size.height == SHOT_SIZE.1 as f64;
        if is_shot && chosen.is_none() {
            chosen = Some((w, num));
        }
    }
    let Some((w, num)) = chosen.or_else(|| {
        // Fallback: the first positive-numbered window.
        (0..count).find_map(|i| {
            let w: *mut objc::runtime::Object = msg_send![windows, objectAtIndex: i];
            let num: i64 = msg_send![w, windowNumber];
            (num > 0).then_some((w, num))
        })
    }) else {
        return None;
    };
    // SAFETY: plain reads on the window this process owns.
    let occl: usize = msg_send![w, occlusionState];
    let scale: f64 = msg_send![w, backingScaleFactor];
    Some((num as u32, scale, occl, w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_dir_holds_the_theme_and_the_guard_removes_it() {
        let root = std::env::temp_dir().join(format!("fno-shot-pin-test-{}", std::process::id()));
        let dir = root.join("demo");
        write_pin(&dir, "light").unwrap();
        let cfg = std::fs::read_to_string(dir.join(".fno/config.toml")).unwrap();
        assert!(cfg.contains(THEME_LIGHT));
        drop(PinGuard(dir.clone()));
        assert!(!root.exists());
        assert_eq!(pin_dir().file_name().and_then(|n| n.to_str()), Some("demo"));
    }
}
