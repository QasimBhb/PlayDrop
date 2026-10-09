import SwiftUI

// A device name is display only and can be chosen by anyone, so every screen shows it next
// to the Short ID, which comes from the pinned key (threat model SR-8). These two views are
// the only way the UI renders a peer's name.

/// A Short ID in monospace.
struct ShortIdText: View {
    /// The Short ID, for example "K7QX-M2PA".
    let shortId: String

    var body: some View {
        Text(shortId)
            .font(.system(.footnote, design: .monospaced))
            .foregroundStyle(.secondary)
            .textSelection(.enabled)
            .accessibilityLabel(Text(L10n.Common.shortIdAccessibility(shortId)))
    }
}

/// A peer's label with its Short ID next to it (SR-8).
struct PeerLabelText: View {
    /// Local label or announced name.
    let label: String
    /// Short ID.
    let shortId: String
    /// Font of the label.
    var font: Font = .body

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(label).font(font).lineLimit(1)
                ShortIdText(shortId: shortId)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(label).font(font).lineLimit(2)
                ShortIdText(shortId: shortId)
            }
        }
        .accessibilityElement(children: .combine)
    }
}
