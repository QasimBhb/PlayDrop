import PlayDropShared
import SwiftUI

// The share sheet card: progress while the items are copied ("Preparing 3 items, 1.2 GiB"),
// then "Queued for <device>" with "Open PlayDrop" and "Done". The send itself happens in the
// app, so the card says plainly that PlayDrop must be opened (anti-pattern 18: nothing is
// silently pending). Actions are closures so the view stays free of extension plumbing.

/// The SwiftUI content of the share extension.
struct ShareView: View {
    /// The share's state.
    let model: ShareModel
    /// Opens PlayDrop, then closes the sheet.
    let onOpenApp: () -> Void
    /// Closes the sheet, keeping the queued batch.
    let onDone: () -> Void
    /// Aborts the batch and closes the sheet.
    let onCancel: () -> Void

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Spacer(minLength: 0)
                content
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationTitle("PlayDrop")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if model.phase == .preparing {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel", action: onCancel)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .preparing:
            preparing
        case .queued(let deviceLabel):
            queued(deviceLabel: deviceLabel)
        case .failed(let message):
            failed(message)
        }
    }

    private var preparing: some View {
        VStack(spacing: 16) {
            ProgressView(
                value: Double(model.processedItems),
                total: Double(max(model.totalItems, 1))
            )
            Text(preparingText)
                .font(.headline)
                .multilineTextAlignment(.center)
        }
        .accessibilityElement(children: .combine)
    }

    private var preparingText: String {
        let n = model.totalItems
        let items = n == 1 ? "1 item" : "\(n) items"
        return "Preparing \(items), \(ByteFormat.bytes(model.queuedBytes))"
    }

    private func queued(deviceLabel: String?) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 44))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(deviceLabel.map { "Queued for \($0)" } ?? "Queued")
                .font(.headline)
                .multilineTextAlignment(.center)
            Text(queuedDetail)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button(action: onOpenApp) {
                Text("Open PlayDrop").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            Button(action: onDone) {
                Text("Done").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
        }
    }

    private var queuedDetail: String {
        let n = model.queuedItems
        var text = "\(n == 1 ? "1 item" : "\(n) items"), \(ByteFormat.bytes(model.queuedBytes)). "
            + "PlayDrop sends it the next time you open the app."
        if model.failedItems > 0 {
            let f = model.failedItems
            text += " \(f == 1 ? "1 item" : "\(f) items") could not be read."
        }
        return text
    }

    private func failed(_ message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 44))
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            Text(message)
                .font(.headline)
                .multilineTextAlignment(.center)
            Button(action: onDone) {
                Text("Done").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
        }
    }
}
