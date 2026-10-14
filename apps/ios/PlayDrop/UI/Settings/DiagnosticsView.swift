import PlayDropCore
import PlayDropShared
import SwiftUI

// Diagnostics: the summary findings first (`DiagnosticsSummary.of`, with the Android
// wording), then each layer (listener, interfaces, multicast beacon, mDNS, paired devices),
// the Local Network fix-it entry when access looks denied, and the raw report in a
// disclosure for bug reports. Nothing in the report is secret: no SAS, QR URI or clipboard
// text is part of it (SR-32). Port of Android `ui/diagnostics/DiagnosticsScreen.kt`.

/// The Diagnostics screen.
struct DiagnosticsView: View {
    @Environment(CoreHost.self) private var core
    @State private var loading = false

    var body: some View {
        List {
            if let d = core.diagnostics {
                let snapshot = DiagnosticsMapping.snapshot(d)
                let issues = DiagnosticsSummary.of(snapshot)
                let access = LocalNetworkInference.infer(
                    from: snapshot,
                    peerOnlineThisRun: core.peerSeenOnlineThisRun
                )
                summary(issues)
                helpEntry(suggested: ConnectivityHelp.isSuggested(for: issues))
                if let reason = Self.deniedReason(access) {
                    Section {
                        NavigationLink {
                            LocalNetworkFixItView(reason: reason)
                        } label: {
                            Label(L10n.LocalNetwork.entry, systemImage: "wifi.exclamationmark")
                                .foregroundStyle(.red)
                        }
                    }
                }
                sections(d)
                Section {
                    DisclosureGroup(L10n.Diag.raw) {
                        Text(verbatim: Self.raw(d))
                            .font(.system(.caption2, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }
            } else {
                Section {
                    Text(loading ? L10n.Diag.running : L10n.Diag.unavailable)
                    NavigationLink {
                        ConnectivityHelpView()
                    } label: {
                        Label(L10n.Connect.title, systemImage: "lifepreserver")
                    }
                    if !core.running {
                        NavigationLink {
                            LocalNetworkFixItView(reason: core.startError)
                        } label: {
                            Label(L10n.LocalNetwork.entry, systemImage: "wifi.exclamationmark")
                        }
                    }
                }
            }
        }
        .navigationTitle(L10n.Diag.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                if loading {
                    ProgressView()
                } else {
                    Button {
                        Task { await refresh() }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .accessibilityLabel(Text(L10n.refresh))
                }
            }
        }
        .refreshable { await refresh() }
        .task { await refresh() }
    }

    private func refresh() async {
        loading = true
        await core.refreshDiagnostics()
        loading = false
    }

    private func summary(_ issues: [DiagIssue]) -> some View {
        let fine = issues == [.allFine]
        return Section(L10n.Diag.summary) {
            ForEach(Array(issues.enumerated()), id: \.offset) { _, issue in
                Label {
                    Text(DiagnosticsMapping.text(issue))
                } icon: {
                    Image(systemName: fine ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .foregroundStyle(fine ? Color.green : Color.orange)
                }
            }
        }
    }

    /// The link to the connection help; highlighted when the findings point to it.
    private func helpEntry(suggested: Bool) -> some View {
        Section {
            NavigationLink {
                ConnectivityHelpView(suggested: suggested)
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Label(L10n.Connect.title, systemImage: "lifepreserver")
                        .foregroundStyle(suggested ? Color.orange : Color.primary)
                    Text(L10n.Connect.entryHint).font(.footnote).foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private func sections(_ d: Diagnostics) -> some View {
        DiagSection(
            title: L10n.Diag.listener,
            ok: d.bindOk,
            lines: [d.bindOk ? L10n.Diag.listeningOn(d.addr) : L10n.Diag.notListening(d.port)]
        )
        DiagSection(
            title: L10n.Diag.interfaces,
            ok: !d.interfaces.isEmpty,
            lines: d.interfaces.isEmpty
                ? [L10n.Diag.noInterface]
                : d.interfaces.map { L10n.Diag.interfaceLine($0.addr, $0.netmask, $0.index) }
        )
        DiagSection(
            title: L10n.Diag.multicast,
            ok: d.multicast.enabled && d.multicast.joinFailed.isEmpty && d.multicast.beaconRecently,
            lines: multicastLines(d.multicast)
        )
        DiagSection(
            title: L10n.Diag.mdns,
            ok: d.mdns.enabled && d.mdns.registered && d.mdns.error == nil,
            lines: mdnsLines(d.mdns)
        )
        if !d.peers.isEmpty {
            DiagSection(
                title: L10n.Diag.peers,
                ok: d.peers.allSatisfy { $0.lastResult == nil || $0.online },
                lines: d.peers.flatMap(peerLines)
            )
        }
    }

    private func multicastLines(_ m: MulticastStatus) -> [String] {
        guard m.enabled else { return [L10n.Diag.off] }
        let beacon = m.lastBeaconSecsAgo.map { L10n.Diag.beaconAgo(UIFormat.duration(seconds: $0)) } ?? L10n.Diag.noBeacon
        return m.joined.map(L10n.Diag.joinedOn) + m.joinFailed.map(L10n.Diag.joinFailed) + [beacon]
    }

    private func mdnsLines(_ n: MdnsStatus) -> [String] {
        guard n.enabled else { return [L10n.Diag.off] }
        var lines = [n.registered ? L10n.Diag.mdnsRegistered : L10n.Diag.mdnsNotRegistered]
        if let e = n.error { lines.append(L10n.Diag.error(e)) }
        lines.append(n.lastSeenSecsAgo.map { L10n.Diag.advertAgo(UIFormat.duration(seconds: $0)) } ?? L10n.Diag.noAdvert)
        return lines
    }

    private func peerLines(_ p: PeerDiagnostic) -> [String] {
        var lines = [L10n.Diag.peerLine(
            p.label,
            p.shortId,
            p.online ? L10n.Common.online : L10n.Common.offline,
            p.mdnsSeen ? L10n.yes : L10n.no,
            p.beaconSeen ? L10n.yes : L10n.no
        )]
        if let r = p.lastResult {
            lines.append(L10n.Diag.peerFailure(DiagnosticsMapping.pathName(r.path), r.failure))
        }
        return lines
    }

    /// The reason when Local Network access looks denied, else nil.
    static func deniedReason(_ access: LocalNetworkAccess) -> String? {
        if case .likelyDenied(let reason) = access { return reason }
        return nil
    }

    /// The raw record as text, for bug reports.
    static func raw(_ d: Diagnostics) -> String {
        var out = ""
        dump(d, to: &out)
        return out
    }
}

/// One layer of the report: an OK or Problem mark, the title and its lines.
private struct DiagSection: View {
    let title: String
    let ok: Bool
    let lines: [String]

    var body: some View {
        Section {
            ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                Text(verbatim: line)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
            }
        } header: {
            HStack(spacing: 6) {
                Image(systemName: ok ? "checkmark.circle.fill" : "xmark.octagon.fill")
                    .foregroundStyle(ok ? Color.green : Color.red)
                    .accessibilityLabel(Text(ok ? L10n.Diag.ok : L10n.Diag.problem))
                Text(title)
            }
        }
    }
}
