import CoreGraphics
import Foundation

/// Pure geometry helpers for recording. Kept free of SCKit/AVFoundation so they
/// stay trivially testable.
enum RecordingGeometry {
    /// H.264 4:2:0 requires even dimensions; round down so we never upscale.
    static func evenDown(_ n: Int) -> Int { max(2, n & ~1) }

    /// H.264 Level 5.2 tops out at 36 864 macroblocks (9 437 184 px). Anything
    /// larger (5K/6K at 2×) is scaled down proportionally, keeping even dims.
    static func fitToH264Limit(_ w: Int, _ h: Int) -> (Int, Int) {
        let maxPixels = 9_437_184
        let pixels = w * h
        guard pixels > maxPixels, pixels > 0 else { return (evenDown(w), evenDown(h)) }
        let factor = (Double(maxPixels) / Double(pixels)).squareRoot()
        return (evenDown(Int((Double(w) * factor).rounded(.down))),
                evenDown(Int((Double(h) * factor).rounded(.down))))
    }

    /// Average H.264 bitrate target: ~0.12 bits per pixel·frame, clamped to a
    /// sane band. Deliberately generous — the share page plays this file 1:1
    /// (bunny renditions are only the fallback), so its text crispness is the
    /// product. Mostly-static screen content stays far below the average
    /// anyway; the cost only shows up while things move.
    static func videoBitrate(width: Int, height: Int, fps: Int) -> Int {
        let raw = Double(width * height * fps) * 0.12
        return min(max(Int(raw), 3_000_000), 32_000_000)
    }

    /// Screen (frame + scale) with the largest intersection with `rect`; ties go to
    /// the screen containing the rect's center. `rect` and frames are AppKit coords.
    static func pickRecordingScreen(
        for rect: CGRect,
        among screens: [(frame: CGRect, scale: CGFloat)]
    ) -> (frame: CGRect, scale: CGFloat, intersection: CGRect)? {
        var best: (frame: CGRect, scale: CGFloat, intersection: CGRect)?
        var bestArea: CGFloat = 0
        let center = CGPoint(x: rect.midX, y: rect.midY)
        for s in screens {
            let inter = s.frame.intersection(rect)
            guard !inter.isNull, inter.width > 0, inter.height > 0 else { continue }
            let area = inter.width * inter.height
            let containsCenter = s.frame.contains(center)
            if area > bestArea || (area == bestArea && containsCenter) {
                bestArea = area
                best = (s.frame, s.scale, inter)
            }
        }
        return best
    }
}
