import PlayDropCore
import SwiftUI

// The pairing question, presented by `RootView` over any screen while
// `CoreHost.pairingPrompt` is set. For SAS pairing it shows the six digits large, with the
// name and the Short ID (SR-8), and asks the user to compare them with the other screen.
// For a QR scan on the other side it asks "Add <name> (<Short ID>)?" (SR-9). Reject is the
// default action (Return on a keyboard) and the sheet cannot be swiped away, so the user
// answers explicitly; the other side gives up after 30 seconds. The SAS is never logged
// (SR-32). Port of Android `ui/pairing/PairingDialog.kt`.

/// Asks the user to accept or reject a pairing.
struct PairingSheet: View {
    /// The prompt to answer.
    let pending: PendingPairing
    /// Called with the answer.
    let onAnswer: (Bool) -> Void

    @State private var answered = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 20) {
                    switch pending.prompt {
                    case .sas(let sas):
                        Text(L10n.Pair.sasTitle(pending.name))
                            .font(.title2.weight(.semibold))
                            .multilineTextAlignment(.center)
                        Text(L10n.Pair.sasBody)
                            .multilineTextAlignment(.center)
                        Text(verbatim: UIFormat.sas(sas))
                            .font(.system(size: 52, weight: .bold, design: .monospaced))
                            .tracking(4)
                            .minimumScaleFactor(0.5)
                            .lineLimit(1)
                            .privacySensitive()
                            .accessibilityLabel(Text(L10n.Pair.sasAccessibility(sas)))
                    case .scannerConfirm:
                        Text(L10n.Pair.confirmTitle(pending.name, pending.shortId))
                            .font(.title2.weight(.semibold))
                            .multilineTextAlignment(.center)
                        Text(L10n.Pair.confirmBody)
                            .multilineTextAlignment(.center)
                    }
                    VStack(spacing: 6) {
                        Text(L10n.Pair.name(pending.name))
                        ShortIdText(shortId: pending.shortId)
                    }
                    HintText(L10n.Pair.timeoutHint)
                        .multilineTextAlignment(.center)
                }
                .padding(24)
                .frame(maxWidth: .infinity)
            }
            .safeAreaInset(edge: .bottom) { buttons }
        }
        .interactiveDismissDisabled()
        .presentationDetents([.medium, .large])
    }

    private var buttons: some View {
        HStack(spacing: 12) {
            Button {
                answer(false)
            } label: {
                Text(isSas ? L10n.Pair.reject : L10n.cancel).frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .keyboardShortcut(.defaultAction)
            .controlSize(.large)

            Button {
                answer(true)
            } label: {
                Text(isSas ? L10n.Pair.accept : L10n.Pair.add).frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
        .disabled(answered)
        .padding(16)
        .background(.bar)
    }

    private var isSas: Bool {
        if case .sas = pending.prompt { return true }
        return false
    }

    private func answer(_ accept: Bool) {
        guard !answered else { return }
        answered = true
        onAnswer(accept)
    }
}
