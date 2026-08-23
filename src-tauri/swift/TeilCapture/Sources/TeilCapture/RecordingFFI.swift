import Foundation

// MARK: - C ABI for screen recording (host: Rust/Tauri via capture_macos.rs)
//
// Status codes returned by the recording entry points:
//   0 (teilOK)        — success
//   1 (teilCancelled) — user cancelled the selection, or a screenshot overlay was up
//   2 (teilError)     — error (*outErr holds a message; free with teil_string_free), or
//                       "no active recording" for pause/resume
//   3 (teilBusy)      — a recording is already active or starting
//
// `teil_record_begin` and `teil_record_stop` BLOCK the calling thread (selection +
// stream start / finishWriting respectively) — call them off the main thread.

private let teilBusy: Int32 = 3

private func reason(for error: RecorderError) -> Int32 {
    switch error {
    case .noFrames: return StopReason.noFrames.rawValue
    case .writer: return StopReason.writerError.rawValue
    default: return StopReason.none.rawValue
    }
}

@_cdecl("teil_record_begin")
public func teil_record_begin(
    _ mode: Int32,
    _ fps: Int32,
    _ captureAudio: Bool,
    _ showCursor: Bool,
    _ maxBytes: UInt64,
    _ outPath: UnsafePointer<CChar>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outErr?.pointee = nil
    guard let outPath, let recMode = RecordMode(rawValue: mode) else {
        outErr?.pointee = strdup("Invalid recording parameters.")
        return teilError
    }
    let path = String(cString: outPath)
    let options = RecordOptions(
        fps: fps <= 0 ? 30 : fps,
        captureAudio: captureAudio,
        showCursor: showCursor,
        maxBytes: maxBytes,
        outURL: URL(fileURLWithPath: path))

    switch runBlocking({ await RecordingController.shared.begin(mode: recMode, options: options) }) {
    case .started:
        return teilOK
    case .cancelled:
        return teilCancelled
    case .busy:
        return teilBusy
    case .error(let message):
        outErr?.pointee = strdup(message)
        return teilError
    }
}

@_cdecl("teil_record_pause")
public func teil_record_pause() -> Int32 {
    RecordingController.shared.pause() ? teilOK : teilError
}

@_cdecl("teil_record_resume")
public func teil_record_resume() -> Int32 {
    RecordingController.shared.resume() ? teilOK : teilError
}

@_cdecl("teil_record_stop")
public func teil_record_stop(
    _ outDurationMs: UnsafeMutablePointer<Int64>?,
    _ outBytes: UnsafeMutablePointer<UInt64>?,
    _ outWidth: UnsafeMutablePointer<Int32>?,
    _ outHeight: UnsafeMutablePointer<Int32>?,
    _ outReason: UnsafeMutablePointer<Int32>?,
    _ outErr: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outErr?.pointee = nil
    switch runBlocking({ await RecordingController.shared.stop() }) {
    case .success(let result):
        outDurationMs?.pointee = result.durationMs
        outBytes?.pointee = result.bytes
        outWidth?.pointee = result.width
        outHeight?.pointee = result.height
        outReason?.pointee = result.reason.rawValue
        return teilOK
    case .failure(let error):
        outReason?.pointee = reason(for: error)
        outErr?.pointee = strdup(error.localizedDescription)
        return teilError
    }
}

@_cdecl("teil_record_cancel")
public func teil_record_cancel() -> Int32 {
    RecordingController.shared.cancel()
    return teilOK
}

@_cdecl("teil_record_status")
public func teil_record_status(
    _ outState: UnsafeMutablePointer<Int32>?,
    _ outReason: UnsafeMutablePointer<Int32>?,
    _ outElapsedMs: UnsafeMutablePointer<Int64>?,
    _ outBytes: UnsafeMutablePointer<UInt64>?,
    _ outWidth: UnsafeMutablePointer<Int32>?,
    _ outHeight: UnsafeMutablePointer<Int32>?
) -> Int32 {
    let snapshot = RecordingController.shared.status()
    outState?.pointee = snapshot.state.rawValue
    outReason?.pointee = snapshot.reason.rawValue
    outElapsedMs?.pointee = snapshot.elapsedMs
    outBytes?.pointee = snapshot.bytes
    outWidth?.pointee = snapshot.width
    outHeight?.pointee = snapshot.height
    return teilOK
}
