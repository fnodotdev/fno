//! The macOS platform fno-app links draws text. gpui-pre gates its real text
//! system behind the platform crate's `font-kit` feature; without it the
//! platform falls back to a no-op system that lists no fonts and rasterizes
//! empty glyphs, and every window paints borders with no words. The platform
//! must be built on the main thread, so this test owns `main` (harness off).

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{font, px, Bounds, DevicePixels, Point, RenderGlyphParams};

    let platform = gpui_platform::current_platform(true);
    let text = platform.text_system();
    assert!(
        !text.all_font_names().is_empty(),
        "the platform lists no fonts: gpui_platform was built without font-kit"
    );
    let font_id = text.font_id(&font("Menlo")).expect("Menlo resolves");
    let glyph_id = text.glyph_for_char(font_id, 'm').expect("Menlo has an m");
    let params = RenderGlyphParams {
        font_id,
        glyph_id,
        font_size: px(14.0),
        subpixel_variant: Point::default(),
        scale_factor: 2.0,
        is_emoji: false,
        subpixel_rendering: false,
        dilation: 0,
    };
    let bounds: Bounds<DevicePixels> = text
        .glyph_raster_bounds(&params)
        .expect("the m has raster bounds");
    let (_, bytes) = text
        .rasterize_glyph(&params, bounds)
        .expect("the m rasterizes");
    assert!(
        bytes.iter().any(|b| *b != 0),
        "the m rasterized with no ink: the platform has no text system"
    );
    println!("text_system: the platform rasterizes an m with ink");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
