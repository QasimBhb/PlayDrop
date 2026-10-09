import PlayDropShared
import SwiftUI

// "Add device": devices found nearby with a typed-address fallback, this iPhone's own name,
// Short ID and address for the other side to type, a QR code to show, or a camera to scan
// one. Opens the core's 120 s window on appear and closes it on disappear; goes back once a
// pairing succeeds. The SAS question itself is the root `PairingSheet`. Port of Android
// `ui/adddevice/AddDeviceScreen.kt`.

/// The Add device screen.
struct AddDeviceView: View {
    @Environment(CoreHost.self) private var core
    @Environment(AppSettings.self) private var settings
    @Environment(MessageCenter.self) private var messages
    @Environment(\.dismiss) private var dismiss

    @State private var model = AddDeviceModel()

    var body: some View {
        VStack(spacing: 0) {
            Picker(L10n.Devices.addDevice, selection: Binding(
                get: { model.mode },
                set: { mode in Task { await model.select(mode, messages: messages) } }
            )) {
                ForEach(AddDeviceModel.Mode.allCases) { mode in
                    Text(mode.title).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.vertical, 8)

            if let name = model.pairingWith {
                HStack(spacing: 12) {
                    ProgressView()
                    Text(name.isEmpty ? L10n.Add.pairing : L10n.Add.pairingWith(name))
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
                .accessibilityElement(children: .combine)
            }

            switch model.mode {
            case .nearby: nearby
            case .showQr: showQr
            case .scanQr: scanQr
            }
        }
        .navigationTitle(L10n.Devices.addDevice)
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await model.openWindow(messages: messages)
            await model.followPairingEvents(messages: messages)
        }
        .onDisappear {
            let m = model
            Task { await m.close() }
        }
        .onChange(of: model.finished) { _, finished in
            if finished { dismiss() }
        }
    }

    // MARK: Nearby

    private var nearby: some View {
        let candidates = Candidates.collapse(core.addDeviceCandidates)
        let busy = model.pairingWith != nil
        return List {
            Section {
                Text(L10n.Add.intro).font(.callout)
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let left = model.windowSecondsLeft(at: context.date)
                    if left > 0 {
                        Text(L10n.Add.looking(UIFormat.countdown(left)))
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(.tint)
                            .monospacedDigit()
                    } else {
                        HStack {
                            Text(L10n.Add.windowClosed)
                            Spacer()
                            Button(L10n.Add.lookAgain) {
                                Task { await model.openWindow(messages: messages) }
                            }
                            .buttonStyle(.bordered)
                        }
                    }
                }
            }
            Section {
                if candidates.isEmpty {
                    Text(L10n.Add.noneYet).foregroundStyle(.secondary)
                } else {
                    ForEach(candidates) { c in
                        Button {
                            Task { await model.pair(c, messages: messages) }
                        } label: {
                            CandidateRowView(candidate: c)
                        }
                        .disabled(busy)
                        .foregroundStyle(.primary)
                    }
                }
            }
            Section {
                HStack {
                    TextField(L10n.Add.addressHint, text: Bindable(model).typedAddress)
                        .keyboardType(.numbersAndPunctuation)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.go)
                        .onSubmit { Task { await model.pairTyped(messages: messages) } }
                        .font(.system(.body, design: .monospaced))
                    Button(L10n.Add.pair) {
                        Task { await model.pairTyped(messages: messages) }
                    }
                    .disabled(busy || model.typedAddress.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            } header: {
                Text(L10n.Add.typeAddress)
            }
            ownIdentitySection
        }
    }

    private var ownIdentitySection: some View {
        Section {
            if let id = core.identity {
                LabeledContent(L10n.Settings.name) { Text(verbatim: settings.deviceName) }
                LabeledContent(L10n.Common.shortId) { ShortIdText(shortId: id.shortId) }
                LabeledContent(L10n.Settings.address) {
                    Text(verbatim: id.addr)
                        .font(.system(.footnote, design: .monospaced))
                        .textSelection(.enabled)
                }
            } else {
                Text(L10n.Add.identityUnknown).foregroundStyle(.secondary)
            }
        } header: {
            Text(L10n.Add.thisDevice)
        } footer: {
            Text(L10n.Add.thisDeviceHint)
        }
    }

    // MARK: Show QR

    private var showQr: some View {
        ScrollView {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                QRCodeView(
                    uri: model.liveQrUri(at: context.date),
                    secondsLeft: model.qrSecondsLeft(at: context.date)
                ) {
                    Task { await model.showQr(messages: messages) }
                }
            }
            .padding(16)
        }
    }

    // MARK: Scan QR

    private var scanQr: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.Qr.scanIntro).font(.callout)
            CameraGate {
                QRScannerView(
                    onCode: { code in Task { await model.scanned(code, messages: messages) } },
                    onFailure: { messages.show(.error(L10n.Qr.cameraFailed)) }
                )
            }
            .aspectRatio(1, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            Spacer(minLength: 0)
        }
        .padding(16)
    }
}

/// One nearby device: type icon, name, kind and every address (best first).
private struct CandidateRowView: View {
    let candidate: CandidateRow

    var body: some View {
        HStack(spacing: 12) {
            DeviceTypeIcon(token: candidate.deviceType)
                .font(.title2)
                .frame(width: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: candidate.name).font(.headline)
                Text(verbatim: "\(DeviceKind(token: candidate.deviceType).label) · \(candidate.addrs.joined(separator: ", "))")
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
            Text(L10n.Add.pair)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.tint)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}
