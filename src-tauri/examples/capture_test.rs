// Standalone diagnostic: verify xcap can actually capture the screen in this environment.
// Run:
//   cargo run --example capture_test
// Then open /tmp/teil-capture-test.png — if it's blank/desktop-only, grant screen permission.

/// macOS no longer uses xcap — capture is native (swift/TeilCapture) and its interactive
/// flows need the app's run loop, so test through the app itself.
#[cfg(target_os = "macos")]
fn main() {
    eprintln!("macOS capture is native (TeilCapture Swift library); this xcap diagnostic only applies to other platforms.");
    eprintln!("Test on macOS by running the app and using the tray/shortcuts.");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    let monitors = match xcap::Monitor::all() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Monitor::all failed: {e}");
            std::process::exit(1);
        }
    };
    println!("Found {} monitor(s)", monitors.len());
    for m in &monitors {
        println!(
            "  monitor: {}x{} at ({},{}) scale={:?} primary={:?}",
            m.width().unwrap_or(0),
            m.height().unwrap_or(0),
            m.x().unwrap_or(0),
            m.y().unwrap_or(0),
            m.scale_factor(),
            m.is_primary(),
        );
    }
    let primary = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .expect("no monitor");
    match primary.capture_image() {
        Ok(img) => {
            let path = "/tmp/teil-capture-test.png";
            img.save(path).expect("save png");
            println!("Captured {}x{} -> {path}", img.width(), img.height());
        }
        Err(e) => eprintln!("capture_image failed: {e}"),
    }

    match xcap::Window::all() {
        Ok(ws) => println!("Window::all -> {} windows", ws.len()),
        Err(e) => eprintln!("Window::all failed: {e}"),
    }
}
