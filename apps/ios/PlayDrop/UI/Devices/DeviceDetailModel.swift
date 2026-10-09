import Foundation
import Observation
import PlayDropCore

// State of the Device screen that is not in `CoreHost`: a grant change on its way to the
// core (so the switch does not flicker back while the call runs) and the rename. Grant and
// label changes raise no event, so both refresh the peer list afterwards. Port of the state
// half of Android `DeviceDetailViewModel.kt`.

/// State and edits of one Device screen.
@MainActor
@Observable
final class DeviceDetailModel {
    /// Hex Device ID.
    let peerId: String
    /// Grants being written, shown instead of the core's until the write finishes.
    private(set) var pendingGrants: Grants?

    @ObservationIgnored private let core: CoreHost

    /// Creates the model for `peerId` over `core`, by default the shared host.
    // Defaults are nil rather than `.shared`: a default argument is evaluated outside the
    // main actor, where the main-actor `shared` instances cannot be read.
    init(peerId: String, core: CoreHost? = nil) {
        self.peerId = peerId
        self.core = core ?? .shared
    }

    /// The grants to show for `peer`.
    func grants(of peer: PeerSummary) -> Grants { pendingGrants ?? peer.grants }

    /// Changes one grant; only this local screen may do so (SR-14).
    func setGrant(_ kind: GrantKind, to value: Bool, current: Grants, messages: MessageCenter) async {
        let next = kind.applying(value, to: current)
        pendingGrants = next
        let peer = peerId
        await CoreCall.run(messages) {
            try await self.core.withHost { host in try await host.setGrants(peer: peer, grants: next) }
        }
        await core.refreshPeers()
        pendingGrants = nil
    }

    /// Sets the local label (the device cannot change it, SR-8). Empty labels are ignored.
    func rename(to label: String, messages: MessageCenter) async {
        let clean = DeviceDetailModel.cleanLabel(label)
        guard !clean.isEmpty else { return }
        let peer = peerId
        await CoreCall.run(messages) {
            try await self.core.withHost { host in try await host.setLabel(peer: peer, label: clean) }
        }
        await core.refreshPeers()
    }

    /// Longest label the rename field accepts.
    static let maxLabelLength = 64

    /// Trims whitespace and caps the length.
    static func cleanLabel(_ label: String) -> String {
        String(label.trimmingCharacters(in: .whitespacesAndNewlines).prefix(maxLabelLength))
    }
}
