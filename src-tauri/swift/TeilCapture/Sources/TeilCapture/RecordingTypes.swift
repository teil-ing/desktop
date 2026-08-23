import CoreGraphics
import Foundation
@preconcurrency import ScreenCaptureKit

// MARK: - Recording enums (raw values are the C ABI contract with Rust)

/// Which surface to record. Matches `RecordMode` on the Rust side.
enum RecordMode: Int32 {
    case region = 0
    case window = 1
    case fullscreen = 2
}

/// Recorder lifecycle. Raw values are what `teil_record_status` reports.
enum RecordState: Int32 {
    case idle = 0
    case recording = 1
    case paused = 2
    case finishing = 3
    /// Recording ended; the file may already be finalized. Rust collects with `teil_record_stop`.
    case stopped = 4
    /// Recording failed; the file was deleted. Rust acknowledges with `teil_record_cancel`.
    case failed = 5
}

/// Why a recording ended. Raw values are what `teil_record_status`/`teil_record_stop` report.
enum StopReason: Int32 {
    case none = 0
    case user = 1
    case sourceClosed = 2
    case sizeLimit = 3
    case streamError = 4
    case writerError = 5
    case noFrames = 6
    case cancelled = 7
}

// MARK: - Options / results

/// Everything the host passes into `teil_record_begin`.
struct RecordOptions: Sendable {
    let fps: Int32
    let captureAudio: Bool
    let showCursor: Bool
    /// Auto-stop once the file reaches this many bytes. 0 = unlimited.
    let maxBytes: UInt64
    let outURL: URL
}

/// A cheap, Sendable status read for `teil_record_status`.
struct RecordStatusSnapshot: Sendable {
    let state: RecordState
    let reason: StopReason
    let elapsedMs: Int64
    let bytes: UInt64
    let width: Int32
    let height: Int32

    static let idle = RecordStatusSnapshot(state: .idle, reason: .none, elapsedMs: 0, bytes: 0, width: 0, height: 0)
}

/// The finalized-recording payload returned by `teil_record_stop`.
struct RecordResult: Sendable {
    let durationMs: Int64
    let bytes: UInt64
    let width: Int32
    let height: Int32
    let reason: StopReason
}

// MARK: - Errors

enum RecorderError: LocalizedError {
    case setup(String)
    case writer(Error?)
    case noFrames
    case notRecording

    var errorDescription: String? {
        switch self {
        case .setup(let m): return m
        case .writer(let e): return e?.localizedDescription ?? "The recording could not be written to disk."
        case .noFrames: return "No frames were captured."
        case .notRecording: return "No recording is in progress."
        }
    }
}

// MARK: - Resolved capture target

/// A fully resolved recording target: the SCKit filter + configuration and the
/// output geometry. Built by `RecordingController` from the user's selection and
/// consumed by `ScreenRecorder`. Not Sendable (holds SCKit reference types); it is
/// created and handed to the recorder on the same task.
struct RecordTarget {
    let filter: SCContentFilter
    let config: SCStreamConfiguration
    let width: Int
    let height: Int
    /// Set for window recordings so the recorder can detect the window closing.
    let windowID: CGWindowID?
    /// Set for region recordings so a border frame can be drawn (AppKit coords).
    let frameRectAppKit: CGRect?
}
