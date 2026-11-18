//! The offscreen shot mode: one demo frame, one png, no visible window.
use std::path::PathBuf;
use std::time::Duration;

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
            let _ = cx.update(|app| app.quit());
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
    let _ = std::fs::remove_dir_all(&pin_dir);
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

/// Order the app's window below the desktop background and capture its
/// buffer by window id. The window server composites an ordered window
/// at any level, so the buffer holds the painted frame, while the user's
/// screen shows nothing (the desktop paints over it) and no focus moves
/// (the window never becomes key).
fn capture_to_png(out: &std::path::Path) -> i32 {
    let window_id = match unsafe { shot_window_id() } {
        Some(id) => id,
        None => {
            eprintln!("fno-app --shot: no capturable fno window in this process");
            return 1;
        }
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let rect = core_graphics::display::CGRect::new(
        core_graphics::geometry::CGPoint {
            x: -30000.0,
            y: -30000.0,
        },
        core_graphics::geometry::CGSize {
            width: SHOT_SIZE.0 as f64,
            height: SHOT_SIZE.1 as f64,
        },
    );
    let image = core_graphics::display::CGDisplay::screenshot(
        rect,
        core_graphics::display::kCGWindowListOptionIncludingWindow,
        window_id,
        core_graphics::display::kCGWindowImageBestResolution,
    );
    let Some(image) = image else {
        eprintln!("fno-app --shot: the window server held no image for the window");
        return 1;
    };
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

/// The app's one window, found through NSApplication's own list: the
/// process is single-window, so the first entry with a positive window
/// number is the shot. Ordering it below the desktop background level
/// keeps it composited but never seen; the window never becomes key.
unsafe fn shot_window_id() -> Option<u32> {
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
            return Some(num as u32);
        }
    }
    None
}

/// CGImage to png bytes through NSBitmapImageRep (file type 4 = png).
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
