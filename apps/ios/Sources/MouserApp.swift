import SwiftUI

#if DEBUG
import AppReveal
#endif

@main
struct MouserApp: App {
    init() {
        #if DEBUG
        // Debug-only in-app MCP server. Advertises `_appreveal._tcp` over the LAN
        // so agents can inspect/control this build. Compiled out of release builds.
        AppReveal.start()
        #endif
    }

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
