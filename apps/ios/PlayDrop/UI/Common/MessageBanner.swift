import SwiftUI

// The toast at the bottom of the window that shows `MessageCenter.current`. Applied once by
// `RootView`; every screen posts to the same center, so messages survive navigation. VoiceOver
// users hear each message as an announcement.

/// Shows the message center's current message over the content.
struct MessageBanner: ViewModifier {
    /// The center to show.
    let center: MessageCenter

    func body(content: Content) -> some View {
        content.overlay(alignment: .bottom) {
            if let message = center.current {
                MessageBannerLabel(message: message) { center.dismiss() }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 64)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .id(message.id)
                    .onAppear {
                        AccessibilityNotification.Announcement(message.text).post()
                    }
            }
        }
        .animation(.easeInOut(duration: 0.2), value: center.current)
    }
}

/// The capsule that holds one message; tap to dismiss.
private struct MessageBannerLabel: View {
    let message: UiMessage
    let onDismiss: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: message.kind == .error ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                .foregroundStyle(message.kind == .error ? Color.red : Color.green)
                .accessibilityHidden(true)
            Text(message.text)
                .font(.callout)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .shadow(radius: 6, y: 2)
        .contentShape(Rectangle())
        .onTapGesture(perform: onDismiss)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}

extension View {
    /// Shows `center`'s messages as a toast at the bottom.
    func messageBanner(_ center: MessageCenter) -> some View {
        modifier(MessageBanner(center: center))
    }
}
