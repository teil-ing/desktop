import Foundation
import ScreenCaptureKit
import AppKit

// MARK: - PermissionService

enum PermissionService {

    /// Checks whether Screen Recording permission has been granted.
    ///
    /// Uses an 8-second timeout to guard against SCShareableContent hanging
    /// when the user has not yet granted or denied the permission prompt.
    static func checkScreenRecordingPermission() async -> Bool {
        await withTaskGroup(of: Bool?.self) { group in
            group.addTask {
                do {
                    _ = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
                    return true
                } catch {
                    return false
                }
            }

            group.addTask {
                try? await Task.sleep(for: .seconds(8))
                return nil
            }

            for await result in group {
                if let value = result {
                    group.cancelAll()
                    return value
                }
                // nil means timeout fired — permission check is hanging
                group.cancelAll()
                return false
            }

            return false
        }
    }

    /// Opens System Settings to the Screen Recording privacy pane.
    @MainActor
    static func openScreenRecordingSettings() {
        let url = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture")!
        NSWorkspace.shared.open(url)
    }
}
