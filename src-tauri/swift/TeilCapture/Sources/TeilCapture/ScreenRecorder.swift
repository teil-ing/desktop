import AppKit
import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
@preconcurrency import ScreenCaptureKit

/// Records one SCStream to an H.264/AAC MP4 via AVAssetWriter.
///
/// All mutable state lives on a single serial `queue`, which is also the SCStream
/// sample-handler queue — so frame delivery and control calls (pause/stop/status)
/// are serialized without extra locks. Non-Sendable AVFoundation/SCKit objects
/// never leave the queue; control methods hop in with `queue.sync`.
final class ScreenRecorder: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let queue = DispatchQueue(label: "ing.teil.recorder", qos: .userInitiated)

    // Immutable config
    private let outURL: URL
    private let fps: Int32
    private let captureAudio: Bool
    private let maxBytes: UInt64
    private let width: Int
    private let height: Int
    private let windowID: CGWindowID?
    private let filter: SCContentFilter
    private let config: SCStreamConfiguration

    // Queue-confined state
    private var state: RecordState = .idle
    private var reason: StopReason = .none
    private var stream: SCStream?
    private var writer: AVAssetWriter?
    private var videoInput: AVAssetWriterInput?
    private var audioInput: AVAssetWriterInput?
    private var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    private var clock = CMClockGetHostTimeClock()
    private var clockValid = false
    private var startedAt = CMTime.zero
    private var pausedTotal = CMTime.zero
    private var pauseStartedAt = CMTime.zero
    private var firstFramePTS: CMTime?
    private var lastVideoPTS = CMTime.invalid
    private var lastAudioPTS = CMTime.invalid
    private var lastPixelBuffer: CVPixelBuffer?
    private var endTime = CMTime.zero
    private var bytesWritten: UInt64 = 0
    private var sizeTimer: DispatchSourceTimer?
    private var finalizeTask: Task<Result<RecordResult, RecorderError>, Never>?
    private var cachedResult: RecordResult?
    /// Consecutive ticks the recorded window was absent from the window list.
    private var windowMissingTicks = 0

    init(target: RecordTarget, options: RecordOptions) {
        self.outURL = options.outURL
        self.fps = max(1, options.fps)
        self.captureAudio = options.captureAudio
        self.maxBytes = options.maxBytes
        self.width = target.width
        self.height = target.height
        self.windowID = target.windowID
        self.filter = target.filter
        self.config = target.config
        super.init()
    }

    // MARK: - Start

    /// Builds the writer + stream and begins capture. Throws on any setup failure
    /// (file already cleaned up). Call off the main thread.
    func start() async throws {
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        if captureAudio {
            try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        }
        self.stream = stream

        try queue.sync {
            try buildWriter()
            guard writer?.startWriting() == true else {
                throw RecorderError.writer(writer?.error)
            }
            self.clock = stream.synchronizationClock ?? CMClockGetHostTimeClock()
            self.clockValid = true
            self.startedAt = CMClockGetTime(self.clock)
            self.state = .recording
        }

        do {
            try await stream.startCapture()
        } catch {
            queue.sync {
                self.state = .failed
                self.reason = .streamError
                self.writer?.cancelWriting()
                try? FileManager.default.removeItem(at: self.outURL)
            }
            throw error
        }

        queue.sync { self.armSizeTimer() }
    }

    private func buildWriter() throws {
        try? FileManager.default.removeItem(at: outURL)
        try FileManager.default.createDirectory(
            at: outURL.deletingLastPathComponent(), withIntermediateDirectories: true)

        let writer = try AVAssetWriter(outputURL: outURL, fileType: .mp4)
        // Deliberately NOT shouldOptimizeForNetworkUse: with it the writer holds the
        // whole mdat back until finishWriting (the output file stays at 0 bytes), which
        // blinds the size-cap stat AND makes finalize rewrite a near-500-MiB file. The
        // server re-transcodes uploads to HLS anyway, so moov-first buys nothing here.
        writer.shouldOptimizeForNetworkUse = false

        let bitrate = RecordingGeometry.videoBitrate(width: width, height: height, fps: Int(fps))
        let videoSettings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: bitrate,
                AVVideoExpectedSourceFrameRateKey: Int(fps),
                AVVideoMaxKeyFrameIntervalKey: Int(fps) * 2,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                AVVideoAllowFrameReorderingKey: false,
                AVVideoH264EntropyModeKey: AVVideoH264EntropyModeCABAC,
            ],
            AVVideoColorPropertiesKey: [
                AVVideoColorPrimariesKey: AVVideoColorPrimaries_ITU_R_709_2,
                AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
                AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_709_2,
            ],
        ]
        let videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
        videoInput.expectsMediaDataInRealTime = true
        guard writer.canAdd(videoInput) else { throw RecorderError.setup("Could not add the video track.") }
        writer.add(videoInput)
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: videoInput, sourcePixelBufferAttributes: nil)

        var audioInput: AVAssetWriterInput?
        if captureAudio {
            let audioSettings: [String: Any] = [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 128_000,
            ]
            let input = AVAssetWriterInput(mediaType: .audio, outputSettings: audioSettings)
            input.expectsMediaDataInRealTime = true
            if writer.canAdd(input) {
                writer.add(input)
                audioInput = input
            }
        }

        self.writer = writer
        self.videoInput = videoInput
        self.adaptor = adaptor
        self.audioInput = audioInput
    }

    // MARK: - Frame handling (on `queue`)

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard state == .recording || state == .paused else { return }
        guard CMSampleBufferDataIsReady(sampleBuffer) else { return }
        switch type {
        case .screen: handleVideo(sampleBuffer)
        case .audio: handleAudio(sampleBuffer)
        @unknown default: break
        }
    }

    private func handleVideo(_ sb: CMSampleBuffer) {
        // Only complete frames carry new pixels; idle/blank/started/suspended are skipped.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false)
            as? [[SCStreamFrameInfo: Any]],
            let statusRaw = attachments.first?[.status] as? Int {
            guard statusRaw == SCFrameStatus.complete.rawValue else { return }
        }
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sb) else { return }
        if state == .paused { return }

        let pts = CMSampleBufferGetPresentationTimeStamp(sb)
        // Session start lives on the OUTPUT timeline (pause offsets applied) so a
        // pause before the first frame can't put samples before the session start.
        let outPTS = pts - pausedTotal
        if firstFramePTS == nil {
            firstFramePTS = outPTS
            writer?.startSession(atSourceTime: outPTS)
        }
        if lastVideoPTS.isValid && outPTS <= lastVideoPTS { return }
        guard let videoInput, videoInput.isReadyForMoreMediaData, let adaptor else { return }
        if adaptor.append(pixelBuffer, withPresentationTime: outPTS) {
            lastVideoPTS = outPTS
            lastPixelBuffer = pixelBuffer
        } else {
            fail(.writerError)
        }
    }

    private func handleAudio(_ sb: CMSampleBuffer) {
        guard firstFramePTS != nil, state == .recording, let audioInput else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sb)
        let outPTS = pts - pausedTotal
        if lastAudioPTS.isValid && outPTS <= lastAudioPTS { return }
        guard audioInput.isReadyForMoreMediaData else { return }

        let toAppend: CMSampleBuffer
        if pausedTotal == .zero {
            toAppend = sb
        } else {
            var timing = CMSampleTimingInfo()
            CMSampleBufferGetSampleTimingInfo(sb, at: 0, timingInfoOut: &timing)
            timing.presentationTimeStamp = timing.presentationTimeStamp - pausedTotal
            timing.decodeTimeStamp = .invalid
            var retimed: CMSampleBuffer?
            CMSampleBufferCreateCopyWithNewTiming(
                allocator: kCFAllocatorDefault, sampleBuffer: sb,
                sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &retimed)
            guard let retimed else { return }
            toAppend = retimed
        }
        if audioInput.append(toAppend) {
            lastAudioPTS = outPTS
        } else {
            fail(.writerError)
        }
    }

    // MARK: - Controls

    func pause() -> Bool {
        queue.sync {
            guard state == .recording else { return false }
            pauseStartedAt = CMClockGetTime(clock)
            state = .paused
            return true
        }
    }

    func resume() -> Bool {
        queue.sync {
            guard state == .paused else { return false }
            pausedTotal = pausedTotal + (CMClockGetTime(clock) - pauseStartedAt)
            state = .recording
            return true
        }
    }

    /// Finalizes the file and returns the result. Idempotent — repeated calls (or a
    /// call after an auto-stop) await the same finalize and return the cached result.
    func stop(reason: StopReason = .user) async -> Result<RecordResult, RecorderError> {
        let task: Task<Result<RecordResult, RecorderError>, Never>? = queue.sync {
            if state == .idle { return nil }
            return beginFinalize(reason: reason)
        }
        guard let task else { return .failure(.notRecording) }
        return await task.value
    }

    /// Discards the recording and deletes the file. Non-blocking.
    func cancel() {
        queue.sync {
            guard state == .recording || state == .paused else { return }
            state = .failed
            reason = .cancelled
            cancelSizeTimer()
            stream?.stopCapture { _ in }
            writer?.cancelWriting()
            try? FileManager.default.removeItem(at: outURL)
        }
    }

    func snapshot() -> RecordStatusSnapshot {
        queue.sync {
            guard clockValid, state != .idle else { return .idle }
            let now = (state == .paused) ? pauseStartedAt : CMClockGetTime(clock)
            let elapsed = max(0, Int64(CMTimeGetSeconds(now - startedAt - pausedTotal) * 1000))
            return RecordStatusSnapshot(
                state: state, reason: reason, elapsedMs: elapsed, bytes: bytesWritten,
                width: Int32(width), height: Int32(height))
        }
    }

    // MARK: - Finalize (once)

    /// Must be called on `queue`. Transitions to `.finishing`, captures the end time,
    /// and kicks off (or returns the in-flight) finalize task.
    private func beginFinalize(reason: StopReason) -> Task<Result<RecordResult, RecorderError>, Never> {
        if let finalizeTask { return finalizeTask }
        if self.reason == .none { self.reason = reason }
        let wasPaused = (state == .paused)
        let now = wasPaused ? pauseStartedAt : CMClockGetTime(clock)
        var end = now - pausedTotal
        if lastVideoPTS.isValid {
            let minEnd = lastVideoPTS + CMTime(value: 1, timescale: fps)
            if end < minEnd { end = minEnd }
        }
        endTime = end
        state = .finishing
        cancelSizeTimer()
        let task = Task { await self.performFinalize() }
        finalizeTask = task
        return task
    }

    private func performFinalize() async -> Result<RecordResult, RecorderError> {
        // Stop the stream (safe even if already stopped by the system).
        if let stream {
            await withCheckedContinuation { cont in
                stream.stopCapture { _ in cont.resume() }
            }
        }

        let hadFrames: Bool = queue.sync { firstFramePTS != nil }
        if !hadFrames {
            queue.sync {
                writer?.cancelWriting()
                try? FileManager.default.removeItem(at: outURL)
                state = .failed
                if reason == .user || reason == .none { reason = .noFrames }
            }
            return .failure(.noFrames)
        }

        queue.sync {
            videoInput?.markAsFinished()
            audioInput?.markAsFinished()
            writer?.endSession(atSourceTime: endTime)
        }

        if let writer {
            await withCheckedContinuation { cont in
                writer.finishWriting { cont.resume() }
            }
        }

        return queue.sync {
            guard let writer, writer.status == .completed else {
                try? FileManager.default.removeItem(at: outURL)
                state = .failed
                reason = .writerError
                return .failure(.writer(writer?.error))
            }
            let attrs = try? FileManager.default.attributesOfItem(atPath: outURL.path)
            let bytes = (attrs?[.size] as? NSNumber)?.uint64Value ?? 0
            let durMs = max(0, Int64(CMTimeGetSeconds(endTime - (firstFramePTS ?? .zero)) * 1000))
            let result = RecordResult(
                durationMs: durMs, bytes: bytes, width: Int32(width), height: Int32(height),
                reason: reason == .none ? .user : reason)
            cachedResult = result
            state = .stopped
            return .success(result)
        }
    }

    private func fail(_ reason: StopReason) {
        // On `queue`. Writer failed mid-stream — abandon the file.
        guard state == .recording || state == .paused else { return }
        state = .failed
        self.reason = reason
        cancelSizeTimer()
        stream?.stopCapture { _ in }
        writer?.cancelWriting()
        try? FileManager.default.removeItem(at: outURL)
    }

    // MARK: - Auto-stop timer (size cap + window-closed)

    private func armSizeTimer() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 1, repeating: 1)
        timer.setEventHandler { [weak self] in self?.timerTick() }
        sizeTimer = timer
        timer.resume()
    }

    private func cancelSizeTimer() {
        sizeTimer?.cancel()
        sizeTimer = nil
    }

    private func timerTick() {
        guard state == .recording || state == .paused else { return }
        if let attrs = try? FileManager.default.attributesOfItem(atPath: outURL.path),
            let size = (attrs[.size] as? NSNumber)?.uint64Value {
            bytesWritten = size
        }
        if maxBytes > 0 && bytesWritten >= maxBytes {
            _ = beginFinalize(reason: .sizeLimit)
            return
        }
        if let windowID {
            // CGWindowListCreateDescriptionFromArray returns an EMPTY array even for
            // live windows on modern macOS (verified 26.5) — it false-positived every
            // window recording into a 1-second auto-stop. Membership in the full
            // window list is reliable; .optionAll keeps minimized windows "alive"
            // (minimize should hold the last frame, not end the recording), and two
            // consecutive misses are required before declaring the window closed.
            let all = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]]
            if let all {
                let alive = all.contains {
                    ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == windowID
                }
                if alive {
                    windowMissingTicks = 0
                } else {
                    windowMissingTicks += 1
                    if windowMissingTicks >= 2 {
                        _ = beginFinalize(reason: .sourceClosed)
                    }
                }
            }
        }
    }

    // MARK: - SCStreamDelegate

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        queue.async {
            guard self.state == .recording || self.state == .paused else { return }
            _ = self.beginFinalize(reason: .streamError)
        }
    }
}
