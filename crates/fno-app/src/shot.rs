//! The offscreen shot mode: one demo frame, one png, no visible window.
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use block2::RcBlock;
use foreign_types::ForeignType;
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

/// How long the capture waits for the window server to hold the first
/// presented frame before it reads the buffer.
const SETTLE: Duration = Duration::from_millis(400);

/// How long the pre-warm waits for the demo world's first layout before
/// it gives up.
const PREWARM: Duration = Duration::from_secs(5);

/// How long the ScreenCaptureKit capture waits for its completion to
/// fire before it counts as a failure.
const CAPTURE_BUDGET: Duration = Duration::from_secs(10);

pub fn run(opts: ShotOptions) -> i32 {
    if let Err(e) = theme_pin(&opts.theme) {
        eprintln!("fno-app --shot: {e}");
        return 2;
    }
    let session = opts
        .session
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mode = opts.mode.unwrap_or(MainView::Orchestration);
    let out = opts.out.clone();
    let family = opts.font.clone();
    let font_size = opts.font_size;
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
        let out = out.clone();
        let family = family.clone();
        let open = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(-30000.0), px(-30000.0)),
                    size(px(SHOT_SIZE.0), px(SHOT_SIZE.1)),
                ))),
                titlebar: None,
                focus: false,
                show: false,
                is_movable: false,
                is_resizable: false,
                ..Default::default()
            },
            move |window, cx| {
                crate::window::root_for_shot(&session, mode, warmed, family, font_size, window, cx)
            },
        );
        let Ok(_handle) = open else {
            eprintln!("fno-app --shot: the window did not open");
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            cx.background_executor().timer(SETTLE).await;
            let code = capture_to_png(&out);
            if code != 0 {
                eprintln!("fno-app --shot: the capture failed");
            }
            cx.update(|app| app.quit()).ok();
        })
        .detach();
    });
    0
}

/// Write the `[mux] theme` pin the ladder reads, in a temp dir the
/// process then chdirs into. A pin beats the system appearance, so the
/// shot paints the asked twin on any host.
fn theme_pin(theme: &str) -> std::io::Result<()> {
    let pin_dir = std::env::temp_dir().join(format!("fno-shot-pin-{}", std::process::id()));
    std::fs::remove_dir_all(&pin_dir).ok();
    std::fs::create_dir_all(pin_dir.join(".fno"))?;
    let name = if theme == "light" {
        THEME_LIGHT
    } else {
        THEME_DARK
    };
    let pin = format!("[mux]\ntheme = \"{name}\"\n");
    std::fs::write(pin_dir.join(".fno/config.toml"), pin)?;
    std::env::set_current_dir(&pin_dir)
}

/// The pre-warm: connect the link and drain events until the world's
/// first `Layout` arrives or the budget runs out. Returns the live link,
/// every event drained before the layout, and the receiver so the root
/// adopts the stream mid-flight.
fn prewarm(
    session: &str,
    _opts: &ShotOptions,
) -> Result<crate::link::WarmedLink, crate::link::LinkError> {
    crate::link::connect_warmed(session, PREWARM)
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
    fn CFEqual(a: *const c_void, b: *const c_void) -> bool;
    fn CFRetain(cf: *const c_void);
    fn CFRelease(cf: *const c_void);
}

/// SAFETY: `dict` is a CFDictionaryRef from CGWindowListCopyWindowInfo
/// and `key` a CFStringRef constant; CFNumberGetValue converts the
/// stored number to i64 itself.
unsafe fn cf_i64(dict: *const c_void, key: *const c_void) -> Option<i64> {
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
unsafe fn ns_string(s: &str) -> *mut objc::runtime::Object {
    let c = CString::new(s).expect("constant key carries no interior NUL");
    let s: *mut objc::runtime::Object =
        msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
    s
}

/// The localizedDescription of an NSError (or a marker for nil), so a
/// failed capture reports what the framework itself said.
unsafe fn ns_error_message(err: *mut objc::runtime::Object) -> String {
    if err.is_null() {
        return "nil error with a nil result".to_string();
    }
    let desc: *mut objc::runtime::Object = msg_send![err, localizedDescription];
    let c: *const c_char = msg_send![desc, UTF8String];
    if c.is_null() {
        return "unreadable error".to_string();
    }
    CStr::from_ptr(c).to_string_lossy().into_owned()
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
                let x = cf_f64(dict, ns_string("X")).unwrap_or(-1.0);
                let y = cf_f64(dict, ns_string("Y")).unwrap_or(-1.0);
                let w = cf_f64(dict, ns_string("Width")).unwrap_or(-1.0);
                let h = cf_f64(dict, ns_string("Height")).unwrap_or(-1.0);
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
    unsafe { CFRelease(list) };
    found
}

/// The ScreenCaptureKit capture: the supported replacement for the
/// obsoleted CGWindowListCreateImage. Reads the shareable content (the
/// window server's own shareable list), proves our window id is there
/// at capture time, then captures that one desktop-independent window.
/// The SC liveness line lands on stderr beside the CG list reads.
unsafe fn sck_capture(window_id: u32, scale: f64) -> Result<core_graphics::image::CGImage, String> {
    let (tx, rx) = channel::<Result<*const c_void, String>>();
    let tx_content = tx.clone();
    let tx_cap = tx.clone();
    // The capture block is created here and a clone moves into the
    // content handler, so the allocation outlives the asynchronous
    // capture call: the framework keeps the block after this frame ends.
    let capture_block = RcBlock::new(
        move |image: *const c_void, err: *mut objc::runtime::Object| {
            if !image.is_null() && err.is_null() {
                // SAFETY: the handler's image is valid only for the call,
                // so it is retained once and the channel carries that +1;
                // the returned CGImage releases it on drop.
                CFRetain(image);
                let _ = tx_cap.send(Ok(image));
            } else {
                let _ = tx_cap.send(Err(ns_error_message(err)));
            }
        },
    );
    let content_block = RcBlock::new(
        move |content: *mut objc::runtime::Object, err: *mut objc::runtime::Object| {
            if content.is_null() {
                let _ = tx_content.send(Err(format!(
                    "SCShareableContent returned nil: {}",
                    ns_error_message(err)
                )));
                return;
            }
            let windows: *mut objc::runtime::Object = msg_send![content, windows];
            let count: usize = msg_send![windows, count];
            let mut live: Option<*mut objc::runtime::Object> = None;
            for i in 0..count {
                let w: *mut objc::runtime::Object = msg_send![windows, objectAtIndex: i];
                let wid: u32 = msg_send![w, windowID];
                if wid == window_id {
                    let frame: core_graphics::geometry::CGRect = msg_send![w, frame];
                    let on_screen: bool = msg_send![w, isOnScreen];
                    eprintln!(
                        "fno-app --shot: window-live[sc-content]: id={window_id} frame=({:.0},{:.0} {:.0}x{:.0}) onScreen={on_screen}",
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    );
                    live = Some(w);
                    break;
                }
            }
            let Some(w) = live else {
                let _ = tx_content.send(Err(format!(
                    "no SCWindow in the shareable content carries id {window_id} ({count} windows): the window was not alive at capture time"
                )));
                return;
            };
            // SAFETY: both alloc/init results are owned here and released
            // right after the capture is scheduled; the SCWindow comes
            // from the list the framework handed this handler and
            // outlives both calls.
            let filter: *mut objc::runtime::Object = msg_send![class!(SCContentFilter), alloc];
            let filter: *mut objc::runtime::Object =
                msg_send![filter, initWithDesktopIndependentWindow: w];
            let config: *mut objc::runtime::Object =
                msg_send![class!(SCStreamConfiguration), alloc];
            let config: *mut objc::runtime::Object = msg_send![config, init];
            let width_px = (SHOT_SIZE.0 as f64 * scale) as i64;
            let height_px = (SHOT_SIZE.1 as f64 * scale) as i64;
            let _: () = msg_send![config, setWidth: width_px];
            let _: () = msg_send![config, setHeight: height_px];
            let _: () = msg_send![config, setShowsCursor: false];
            let _: () = msg_send![class!(SCScreenshotManager),
                captureImageWithContentFilter: filter
                configuration: config
                completionHandler: &*capture_block];
            let _: () = msg_send![filter, release];
            let _: () = msg_send![config, release];
        },
    );
    let _: () = msg_send![class!(SCShareableContent),
        getShareableContentExcludingDesktopWindows: false
        onScreenWindowsOnly: false
        completionHandler: &*content_block];
    drop(tx);
    match rx.recv_timeout(CAPTURE_BUDGET) {
        Ok(Ok(ptr)) => Ok(core_graphics::image::CGImage::from_ptr(ptr as *mut _)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the ScreenCaptureKit completion never fired within the budget".to_string()),
    }
}

/// Order the app's window below the desktop background and capture its
/// buffer by window id through ScreenCaptureKit. The window server
/// composites an ordered window at any level, so the buffer holds the
/// painted frame, while the user's screen shows nothing (the desktop
/// paints over it) and no focus moves (the window never becomes key).
/// The proof lines land on stderr: the TCC preflight read, the
/// window-server liveness before and after the capture, and the SC
/// shareable-content read inside the capture itself.
fn capture_to_png(out: &std::path::Path) -> i32 {
    let Some((window_id, scale)) = (unsafe { shot_window_id() }) else {
        eprintln!("fno-app --shot: no capturable fno window in this process");
        return 1;
    };
    let scale = if scale >= 1.0 { scale } else { 1.0 };
    let preflight = unsafe { CGPreflightScreenCaptureAccess() };
    eprintln!("fno-app --shot: preflight screen-capture access: {preflight}");
    let before = log_window_live(window_id, "before-capture");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let image = sck_capture(window_id, scale);
    let image = match image {
        Ok(image) => image,
        Err(e) => {
            eprintln!("fno-app --shot: the SCK capture returned no image: {e}");
            return 1;
        }
    };
    let after = log_window_live(window_id, "after-capture");
    if !after {
        eprintln!("fno-app --shot: the window left the window-server list during the capture");
    }
    // SAFETY: objc msg_send over the NSBitmapImageRep path: the rep is
    // initialized from the CGImageRef the window server returned, both
    // objects are released before return, and the bytes are copied into
    // the Vec while the data object is alive.
    let png = unsafe { png_bytes(image) };
    match std::fs::write(out, &png) {
        Ok(()) => {
            println!("shot wrote {}", out.display());
            0
        }
        Err(e) => {
            eprintln!("fno-app --shot: write {} failed: {e}", out.display());
            1
        }
    }
}

/// The app's one window and its backing scale, found through
/// NSApplication's own list: the process is single-window, so the first
/// entry with a positive window number is the shot. Ordering it below
/// the desktop background level keeps it composited but never seen; the
/// window never becomes key.
unsafe fn shot_window_id() -> Option<(u32, f64)> {
    let app: *mut objc::runtime::Object = msg_send![class!(NSApplication), sharedApplication];
    let windows: *mut objc::runtime::Object = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    for i in 0..count {
        let w: *mut objc::runtime::Object = msg_send![windows, objectAtIndex: i];
        let num: i64 = msg_send![w, windowNumber];
        if num > 0 {
            // Below the desktop background level: composited, never seen.
            let _: () = msg_send![w, setLevel: -2147483100i64];
            let _: () = msg_send![w, orderFront: std::ptr::null_mut::<objc::runtime::Object>()];
            let scale: f64 = msg_send![w, backingScaleFactor];
            return Some((num as u32, scale));
        }
    }
    None
}

/// CGImage to png bytes through NSBitmapImageRep (file type 4 = png).
/// The image arrives from the ScreenCaptureKit capture.
unsafe fn png_bytes(image: core_graphics::image::CGImage) -> Vec<u8> {
    let nil: *mut objc::runtime::Object = std::ptr::null_mut();
    let rep: *mut objc::runtime::Object = msg_send![class!(NSBitmapImageRep), alloc];
    let rep: *mut objc::runtime::Object = msg_send![rep, initWithCGImage: image.as_ptr()];
    let data: *mut objc::runtime::Object = msg_send![rep,
        representationUsingType: 4i64
        properties: nil
    ];
    let mut out = Vec::new();
    if !data.is_null() {
        let len: usize = msg_send![data, length];
        let bytes: *const u8 = msg_send![data, bytes];
        out = std::slice::from_raw_parts(bytes, len).to_vec();
    }
    let _: () = msg_send![rep, release];
    let _: () = msg_send![data, release];
    out
}
