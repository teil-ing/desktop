fn main() {
    // Compile and statically link the native TeilCapture Swift package on macOS.
    // Swift's runtime ships with macOS 14+, so nothing extra is bundled.
    // cfg gates on the HOST (build scripts always do) — the env check gates on the
    // TARGET, so cross-checking e.g. the Windows target from a Mac skips Swift.
    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        swift_rs::SwiftLinker::new("14.0")
            .with_package("TeilCapture", "./swift/TeilCapture")
            .link();
        // The Swift autolink stubs reference the runtime as @rpath/libswift*.dylib;
        // point rpath at the OS-provided runtime (present on all macOS 14+ systems).
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    tauri_build::build()
}
