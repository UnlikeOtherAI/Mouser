import SwiftUI

@main
struct MouserApp: App {
    var body: some Scene {
        WindowGroup {
            CompanionView()
                .onAppear {
                    // Honour an optional `-startOrientation` launch argument so the
                    // landscape full-screen-trackpad layout can be captured
                    // headlessly (see OrientationControl). No-op in normal use.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                        OrientationControl.applyLaunchOrientationIfRequested()
                    }
                }
        }
    }
}
