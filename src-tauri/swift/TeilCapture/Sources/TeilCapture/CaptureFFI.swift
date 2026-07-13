import AppKit
import Foundation
import ImageIO
import UniformTypeIdentifiers
@preconcurrency import ScreenCaptureKit

// MARK: - C ABI for the Rust (Tauri) host
//
// Each interactive entry point BLOCKS the calling thread until the user finishes
// or cancels — the host must call them from a background thread (Rust side uses
// tokio::task::spawn_blocking), never from the main thread, or the overlay's
// main-thread work would deadlock against the wait.
//
// Status codes returned by the capture entry points:
//   0 — success: *outPtr/*outLen hold a PNG buffer (free with teil_buffer_free)
//   1 — cancelled by the user (Escape / right-click / sub-minimum drag), or a
//       session was already in progress. No buffer, no error.
//   2 — error: *outErr holds a message (free with teil_string_free)

private let teilOK: Int32 = 0
private let teilCancelled: Int32 = 1
private let teilError: Int32 = 2

// MARK: - Capture outcome

private enum Outcome: Sendable {
    case png(Data)
    case cancelled
    case error(String)
}

// MARK: - Single-session guard

/// Serializes interactive sessions: a second hotkey press while an overlay is
/// already up is reported as `cancelled` instead of stacking overlays.
private let sessionLock = NSLock()
private nonisolated(unsafe) var sessionActive = false

private func beginSession() -> Bool {
    sessionLock.lock()
    defer { sessionLock.unlock() }
    if sessionActive { return false }
    sessionActive = true
    return true
}

private func endSession() {
    sessionLock.lock()
    sessionActive = false
    sessionLock.unlock()
}

// MARK: - Async → blocking bridge

private final class ResultBox<T>: @unchecked Sendable {
    var value: T?
}

/// Runs an async body on the concurrency pool and blocks the calling (non-main)
/// thread until it completes. MainActor work inside the body is serviced by the
/// host app's run loop, which Tauri keeps running on the real main thread.
private func runBlocking<T>(_ body: @escaping @Sendable () async -> T) -> T {
    let box = ResultBox<T>()
    let semaphore = DispatchSemaphore(value: 0)
    Task.detached(priority: .userInitiated) {
        box.value = await body()
        semaphore.signal()
    }
    semaphore.wait()
    return box.value!
}

// MARK: - PNG encoding

private func pngData(from image: CGImage) -> Data? {
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(
        data as CFMutableData,
        UTType.png.identifier as CFString,
        1,
        nil
    ) else { return nil }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { return nil }
    return data as Data
}

// MARK: - Capture flows (overlay → engine → feedback → PNG)

@MainActor
private func regionFlow(showFlash: Bool, playSound: Bool) async -> Outcome {
    guard let rect = await OverlayCoordinator().beginRegionSelection() else {
        return .cancelled
    }
    do {
        let result = try await CaptureEngine().captureRegion(rect)
        if playSound { CaptureFeedback.playCaptureSound() }
        if showFlash { await CaptureFeedback.showCaptureFlash(in: result.capturedRect) }
        guard let png = pngData(from: result.image) else {
            return .error("PNG encoding failed.")
        }
        return .png(png)
    } catch {
        return .error(error.localizedDescription)
    }
}

@MainActor
private func windowFlow(showFlash: Bool, playSound: Bool) async -> Outcome {
    let coordinator = WindowSelectionCoordinator()
    guard let selection = await coordinator.beginWindowSelection() else {
        return .cancelled
    }
    let engine = CaptureEngine()
    do {
        let result: CaptureResult
        let flashRect: CGRect
        switch selection {
        case .window(let scWindow):
            result = try await engine.captureWindow(scWindow)
            // captureWindow returns the rect in CG coordinates; the flash wants AppKit.
            flashRect = coordinator.cgFrameToAppKit(result.capturedRect)
        case .desktop:
            // Click with no window under the cursor — capture that display fullscreen.
            result = try await engine.captureFullscreen()
            flashRect = result.capturedRect
        }
        if playSound { CaptureFeedback.playCaptureSound() }
        if showFlash { await CaptureFeedback.showCaptureFlash(in: flashRect) }
        guard let png = pngData(from: result.image) else {
            return .error("PNG encoding failed.")
        }
        return .png(png)
    } catch {
        return .error(error.localizedDescription)
    }
}

@MainActor
private func fullscreenFlow(showFlash: Bool, playSound: Bool) async -> Outcome {
    do {
        let result = try await CaptureEngine().captureFullscreen()
        if playSound { CaptureFeedback.playCaptureSound() }
        if showFlash { await CaptureFeedback.showCaptureFlash(in: result.capturedRect) }
        guard let png = pngData(from: result.image) else {
            return .error("PNG encoding failed.")
        }
        return .png(png)
    } catch {
        return .error(error.localizedDescription)
    }
}

// MARK: - Shared FFI plumbing

private func runCapture(
    _ outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLen: UnsafeMutablePointer<UInt>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    flow: @escaping @Sendable () async -> Outcome
) -> Int32 {
    outPtr?.pointee = nil
    outLen?.pointee = 0
    outErr?.pointee = nil

    guard beginSession() else { return teilCancelled }
    defer { endSession() }

    switch runBlocking(flow) {
    case .cancelled:
        return teilCancelled
    case .error(let message):
        outErr?.pointee = strdup(message)
        return teilError
    case .png(let data):
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: data.count)
        data.copyBytes(to: buffer, count: data.count)
        outPtr?.pointee = buffer
        outLen?.pointee = UInt(data.count)
        return teilOK
    }
}

// MARK: - Exported entry points

@_cdecl("teil_capture_region_interactive")
public func teil_capture_region_interactive(
    _ showFlash: Bool,
    _ playSound: Bool,
    _ outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLen: UnsafeMutablePointer<UInt>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    runCapture(outPtr, outLen, outErr) {
        await regionFlow(showFlash: showFlash, playSound: playSound)
    }
}

@_cdecl("teil_capture_window_interactive")
public func teil_capture_window_interactive(
    _ showFlash: Bool,
    _ playSound: Bool,
    _ outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLen: UnsafeMutablePointer<UInt>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    runCapture(outPtr, outLen, outErr) {
        await windowFlow(showFlash: showFlash, playSound: playSound)
    }
}

@_cdecl("teil_capture_fullscreen")
public func teil_capture_fullscreen(
    _ showFlash: Bool,
    _ playSound: Bool,
    _ outPtr: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLen: UnsafeMutablePointer<UInt>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    runCapture(outPtr, outLen, outErr) {
        await fullscreenFlow(showFlash: showFlash, playSound: playSound)
    }
}

/// Blocking permission probe (up to ~8s if the TCC prompt is pending).
@_cdecl("teil_has_screen_permission")
public func teil_has_screen_permission() -> Bool {
    runBlocking { await PermissionService.checkScreenRecordingPermission() }
}

/// Opens System Settings at the Screen Recording privacy pane. Non-blocking.
@_cdecl("teil_open_screen_settings")
public func teil_open_screen_settings() {
    Task { @MainActor in
        PermissionService.openScreenRecordingSettings()
    }
}

@_cdecl("teil_buffer_free")
public func teil_buffer_free(_ ptr: UnsafeMutablePointer<UInt8>?, _ len: UInt) {
    _ = len
    ptr?.deallocate()
}

@_cdecl("teil_string_free")
public func teil_string_free(_ ptr: UnsafeMutablePointer<CChar>?) {
    free(ptr)
}
