import UIKit

/// Test/automation hook for forcing the interface orientation at launch.
///
/// The simulator cannot be rotated headlessly without GUI Accessibility
/// permissions, so to capture the landscape full-screen-trackpad screenshot we
/// let the launcher request an orientation via a launch argument:
///
///     xcrun simctl launch <dev> <bundle> -startOrientation landscape
///
/// In normal use no argument is passed and the device's real orientation drives
/// the layout (requirement §1). This only *requests* a geometry update through
/// the supported iOS 16+ API; it does not lock rotation.
enum OrientationControl {
    /// Reads `-startOrientation <portrait|landscape>` (UserDefaults exposes
    /// launch args as keys) and, if present, asks the active window scene to
    /// adopt that orientation.
    @MainActor
    static func applyLaunchOrientationIfRequested() {
        guard let value = UserDefaults.standard.string(forKey: "startOrientation") else { return }
        switch value.lowercased() {
        case "landscape", "landscapeleft", "landscaperight":
            request(.landscapeRight)
        case "portrait":
            request(.portrait)
        default:
            break
        }
    }

    @MainActor
    private static func request(_ orientation: UIInterfaceOrientationMask) {
        guard let scene = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first else { return }
        let prefs = UIWindowScene.GeometryPreferences.iOS(interfaceOrientations: orientation)
        scene.requestGeometryUpdate(prefs) { _ in }
    }
}
