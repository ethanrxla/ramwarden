import SwiftUI
import AppKit

@main
struct WardenSetupApp: App {
    var body: some Scene {
        WindowGroup("Set up your wardens") { SetupView().frame(minWidth: 640, minHeight: 520) }
            .windowStyle(.hiddenTitleBar)
    }
}
