import PlayDropCore
import PlayDropShared
import SwiftUI

// The presenter tab: big Previous and Next buttons for slides, Blackout, the laser pointer
// toggle and, when the computer accepts keys, volume buttons (protocol.md 5.12).

/// The presenter tab.
struct PresenterPane: View {
    let model: RemoteInputModel
    /// The computer accepts keys, so the volume buttons work.
    let volumeKeys: Bool

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 12) {
                big(L10n.Input.previous, "chevron.left") { model.presenter(.previous) }
                big(L10n.Input.next, "chevron.right") { model.presenter(.next) }
            }
            .frame(maxHeight: .infinity)
            HStack(spacing: 12) {
                Button {
                    model.presenter(.blackout)
                    InputHaptics.click()
                } label: {
                    Label(L10n.Input.blackout, systemImage: "rectangle.fill").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                Button {
                    model.toggleLaser()
                    InputHaptics.click()
                } label: {
                    Label(L10n.Input.pointer, systemImage: model.laserOn ? "light.max" : "light.min")
                        .frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .tint(model.laserOn ? .red : .secondary)
                .accessibilityAddTraits(model.laserOn ? .isSelected : [])
            }
            if volumeKeys {
                HStack(spacing: 12) {
                    Button {
                        model.mediaKey(RemoteKey.volumeDown)
                    } label: {
                        Label(L10n.Input.volumeDown, systemImage: "speaker.minus.fill").frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .buttonStyle(.bordered)
                    Button {
                        model.mediaKey(RemoteKey.volumeUp)
                    } label: {
                        Label(L10n.Input.volumeUp, systemImage: "speaker.plus.fill").frame(maxWidth: .infinity, minHeight: 44)
                    }
                    .buttonStyle(.bordered)
                }
            }
        }
    }

    private func big(_ title: String, _ symbol: String, action: @escaping () -> Void) -> some View {
        Button {
            action()
            InputHaptics.click()
        } label: {
            VStack(spacing: 8) {
                Image(systemName: symbol).font(.system(size: 48, weight: .semibold))
                Text(title).font(.title3)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .buttonStyle(.borderedProminent)
    }
}
