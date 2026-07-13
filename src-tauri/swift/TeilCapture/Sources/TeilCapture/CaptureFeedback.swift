import AppKit
import QuartzCore

// MARK: - CaptureFeedback

/// Caseless enum namespace for post-capture feedback: flash window and system sound.
///
/// Trimmed from the macOS app's CaptureFeedback — the menu-bar icon states
/// (success/spinner/error) are owned by the Tauri tray on the crossplatform client.
@MainActor
enum CaptureFeedback {

    // MARK: - Flash Window

    /// Displays a brief white flash over the captured area to confirm the capture.
    ///
    /// The flash window appears AFTER the capture is complete — it is purely cosmetic
    /// and does not interfere with the captured image.
    ///
    /// - Parameter rect: The captured area in global AppKit screen coordinates.
    static func showCaptureFlash(in rect: CGRect) async {
        // Find the screen that contains the majority of the captured rect
        let targetScreen = NSScreen.screens.max(by: { a, b in
            a.frame.intersection(rect).area < b.frame.intersection(rect).area
        })

        // Position the flash window at the captured rect in screen coordinates.
        // NSWindow uses AppKit coordinates (origin at bottom-left, Y upward) which
        // matches the global coordinate space used by CaptureResult.capturedRect.
        let flashWindow = NSWindow(
            contentRect: rect,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false,
            screen: targetScreen
        )

        flashWindow.level = .screenSaver
        flashWindow.backgroundColor = .white
        flashWindow.isOpaque = true
        flashWindow.alphaValue = 0.8
        flashWindow.isReleasedWhenClosed = false
        flashWindow.hasShadow = false

        // Exclude the flash from being captured by ScreenCaptureKit on a rapid
        // second capture (belt-and-suspenders — flash appears post-capture anyway).
        flashWindow.sharingType = .none

        flashWindow.orderFront(nil)

        // Await the fade animation so the caller knows when the flash is done.
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            NSAnimationContext.runAnimationGroup(
                { ctx in
                    ctx.duration = 0.15
                    flashWindow.animator().alphaValue = 0.0
                },
                completionHandler: {
                    flashWindow.orderOut(nil)
                    continuation.resume()
                }
            )
        }
    }

    // MARK: - Sound

    /// Plays a camera-shutter system sound to confirm the capture.
    static func playCaptureSound() {
        NSSound(named: "Tink")?.play()
    }
}

// MARK: - CGRect area helper

private extension CGRect {
    var area: CGFloat { width * height }
}
