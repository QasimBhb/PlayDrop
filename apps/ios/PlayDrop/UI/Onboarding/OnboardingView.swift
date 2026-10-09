import SwiftUI

// First run: three short pages. What PlayDrop does; what iOS lets it do, said honestly
// (send any time, receive while open, queued files from desktops and Android phones arrive
// when the app opens; PLAN.md anti-pattern 11, no always-on claims) together with the Local
// Network question iOS will ask; then notifications, asked for only from the user's tap.
// Finishing sets `AppSettings.onboardingDone` and tells `AppServices`, which only then starts
// the core, so the Local Network prompt comes after the page that explains it. Port of
// Android `ui/onboarding/OnboardingScreen.kt` without the foreground-service and battery pages.

/// The first-run pages.
struct OnboardingView: View {
    @Environment(AppSettings.self) private var settings
    @State private var step = 0
    @State private var asking = false

    /// Number of pages.
    static let pageCount = 3

    var body: some View {
        VStack(spacing: 0) {
            ProgressView(value: Double(step + 1), total: Double(Self.pageCount))
                .padding(.horizontal, 24)
                .padding(.top, 16)
                .accessibilityLabel(Text(L10n.Onboarding.progress(step + 1, Self.pageCount)))
            TabView(selection: $step) {
                page(icon: "laptopcomputer.and.iphone", title: L10n.Onboarding.title1, text: L10n.Onboarding.body1)
                    .tag(0)
                page(icon: "iphone.radiowaves.left.and.right", title: L10n.Onboarding.title2, text: L10n.Onboarding.body2)
                    .tag(1)
                page(icon: "bell.badge", title: L10n.Onboarding.title3, text: L10n.Onboarding.body3)
                    .tag(2)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .animation(.easeInOut, value: step)
            buttons
                .padding(24)
        }
    }

    @ViewBuilder
    private var buttons: some View {
        HStack(spacing: 12) {
            Spacer()
            if step < Self.pageCount - 1 {
                Button(L10n.next) { step += 1 }
                    .buttonStyle(.borderedProminent)
            } else {
                Button(L10n.notNow) { finish() }
                    .buttonStyle(.bordered)
                    .disabled(asking)
                Button {
                    Task { await allowAndFinish() }
                } label: {
                    if asking { ProgressView() } else { Text(L10n.allow) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(asking)
            }
        }
        .controlSize(.large)
    }

    private func page(icon: String, title: String, text: String) -> some View {
        ScrollView {
            VStack(spacing: 20) {
                Image(systemName: icon)
                    .font(.system(size: 64))
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text(title)
                    .font(.title2.weight(.semibold))
                    .multilineTextAlignment(.center)
                Text(text)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
            .frame(maxWidth: .infinity)
        }
    }

    private func allowAndFinish() async {
        asking = true
        _ = await LocalNotifications.shared.requestPermission()
        asking = false
        finish()
    }

    private func finish() {
        settings.onboardingDone = true
        // The core (and with it the Local Network prompt) waits for this point.
        AppServices.onboardingDidFinish()
    }
}

extension L10n {
    /// Onboarding pages.
    enum Onboarding {
        /// Page 1 title.
        static var title1: String {
            String(localized: "ui_onboarding_1_title", defaultValue: "Your iPhone and computer, together")
        }
        /// Page 1 body.
        static var body1: String {
            String(
                localized: "ui_onboarding_1_body",
                defaultValue: "Send photos and files to your computer in a tap, share text both ways, ring a lost device. Everything stays on your own network, with no account."
            )
        }
        /// Page 2 title.
        static var title2: String {
            String(localized: "ui_onboarding_ios_title", defaultValue: "What iOS allows")
        }
        /// Page 2 body.
        static var body2: String {
            String(
                localized: "ui_onboarding_ios_body",
                defaultValue: "You can send at any time, from PlayDrop or the share sheet in any app. This iPhone receives while PlayDrop is open. When it is closed, your computers and Android phones keep files for it, and they arrive as soon as you open PlayDrop. iOS will ask to find devices on your local network: choose Allow, or PlayDrop cannot see them."
            )
        }
        /// Page 3 title.
        static var title3: String {
            String(localized: "ui_onboarding_2_title", defaultValue: "Notifications")
        }
        /// Page 3 body.
        static var body3: String {
            String(
                localized: "ui_onboarding_notifications_body",
                defaultValue: "PlayDrop tells you when files arrive, when a device asks before sending, and when a new device is paired."
            )
        }
        /// "Step 2 of 3".
        static func progress(_ step: Int, _ total: Int) -> String {
            String(localized: "ui_onboarding_progress", defaultValue: "Step \(step) of \(total)")
        }
    }
}
