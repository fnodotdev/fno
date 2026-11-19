fn main() {
    // ScreenCaptureKit's ObjC classes (SCShareableContent,
    // SCScreenshotManager) resolve at runtime only if the framework is
    // loaded; this adds the LC_LOAD_DYLIB entry.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
