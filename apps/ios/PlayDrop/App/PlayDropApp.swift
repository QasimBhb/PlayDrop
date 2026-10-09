import SwiftUI

// The app's entry point. It only connects the system lifecycle to `AppServices`:
// - launch: `AppServices.start()` once (event subscribers, queue drain, stale-pick sweep);
// - scene phase: `active` starts the core and catches up, `background` stops it (iOS
//   suspends the process soon after; PLAN.md anti-pattern 11). `inactive` (Control Center,
//   an incoming call banner, the app switcher) is ignored, so a glance at Control Center
//   does not restart the core;
// - `playdrop://` opens go to `URLSchemeHandler`, which only drains the outgoing queue and
//   never pairs or runs any command from the URL (SR-30).
// The core itself is held back until onboarding is done, so the iOS Local Network prompt
// appears after the page that explains it (see `AppServices`).

/// The PlayDrop app.
@main
struct PlayDropApp: App {
    @Environment(\.scenePhase) private var scenePhase

    /// Starts the process-wide services once, before the first scene appears.
    init() {
        AppServices.start()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .onOpenURL { url in
                    URLSchemeHandler.handle(url)
                }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                Task { await AppServices.sceneDidBecomeActive() }
            case .background:
                Task { await AppServices.sceneDidEnterBackground() }
            case .inactive:
                break
            @unknown default:
                break
            }
        }
    }
}
