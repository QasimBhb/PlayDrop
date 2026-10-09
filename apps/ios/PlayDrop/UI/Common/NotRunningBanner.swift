import SwiftUI

// The red strip shown on top of the Devices list while the core is down. On iOS the core runs
// whenever the app is open, so being down means the start failed; the banner says so and
// leads to the Local Network fix-it screen, which explains the likely cause and retries.

/// "PlayDrop is not running" with the start error and a link to the fix-it screen.
struct NotRunningBanner: View {
    /// Why the last start failed, if known.
    let startError: String?

    var body: some View {
        NavigationLink {
            LocalNetworkFixItView(reason: startError)
        } label: {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "wifi.exclamationmark")
                    .foregroundStyle(.red)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(L10n.notRunningBanner).font(.subheadline.weight(.semibold))
                    if let startError {
                        Text(L10n.Err.startFailed(startError))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(12)
            .background(Color.red.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .buttonStyle(.plain)
    }
}
