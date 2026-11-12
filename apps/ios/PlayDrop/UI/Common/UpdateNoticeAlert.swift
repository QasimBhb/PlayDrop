import PlayDropShared
import SwiftUI

// The alert that offers a newer release: "Open release page" opens it in Safari, "Later"
// hides the notice until a check brings it back. The iPhone never downloads or installs.
// Applied once by `RootView`.

/// Shows the update notice of `UpdateChecker` as an alert.
private struct UpdateNoticeAlert: ViewModifier {
    /// The checker to show; passed in because the modifier sits outside the `environment`
    /// calls of `RootView`, where the environment does not reach it.
    let checker: UpdateChecker
    @Environment(\.openURL) private var openURL

    func body(content: Content) -> some View {
        let notice = checker.presentation.notice
        content.alert(
            L10n.Update.title(notice?.version ?? ""),
            isPresented: Binding(
                get: { checker.presentation.notice != nil },
                // Any way out of the alert hides the notice, so it cannot come back on its own.
                set: { shown in if !shown { checker.later() } }
            ),
            presenting: notice
        ) { notice in
            Button(L10n.Update.openPage) { openURL(notice.pageURL) }
            Button(L10n.Update.later, role: .cancel) {}
        } message: { _ in
            Text(L10n.Update.installHint)
        }
    }
}

extension View {
    /// Offers a newer release as an alert.
    func updateNoticeAlert(_ checker: UpdateChecker) -> some View {
        modifier(UpdateNoticeAlert(checker: checker))
    }
}
