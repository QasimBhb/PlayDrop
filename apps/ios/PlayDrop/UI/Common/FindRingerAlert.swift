import SwiftUI

// The root-level alert shown while a paired device rings this iPhone ("find my phone").
// `FindRinger` owns the ringing; this alert only shows who is looking and offers Stop, the
// iOS counterpart of Android's find notification. Applied once by `RootView`, so it appears
// over every screen, onboarding included.

/// Shows the "is looking for this iPhone" alert while `FindRinger.shared` rings.
struct FindRingerAlert: ViewModifier {
    @State private var ringer = FindRinger.shared

    func body(content: Content) -> some View {
        content.alert(
            L10n.Find.title(ringer.ringingLabel ?? ""),
            isPresented: Binding(
                get: { ringer.isRinging },
                set: { shown in if !shown { ringer.stop() } }
            )
        ) {
            Button(L10n.Find.stop, role: .cancel) { ringer.stop() }
        } message: {
            Text(L10n.Find.text)
        }
    }
}

extension View {
    /// Shows the find-my-phone alert while a peer rings this iPhone.
    func findRingerAlert() -> some View {
        modifier(FindRingerAlert())
    }
}

extension L10n {
    /// Find my phone.
    enum Find {
        /// "<label> is looking for this iPhone".
        static func title(_ label: String) -> String {
            String(localized: "ui_find_title", defaultValue: "\(label) is looking for this iPhone")
        }
        /// "Tap Stop to silence".
        static var text: String { String(localized: "ui_find_text", defaultValue: "Tap Stop to silence") }
        /// "Stop".
        static var stop: String { String(localized: "ui_find_stop", defaultValue: "Stop") }
    }
}
