import PlayDropShared
import SwiftUI

// The keyboard tab: a typing field whose edits go out as text and Backspace frames
// (`KeyboardComposer`), a row of special keys, modifier chips that apply to the next key,
// and media keys. Key names are W3C UI Events `key` values (protocol.md 5.12).

/// The keyboard tab.
struct KeyboardPane: View {
    let model: RemoteInputModel

    @State private var text = KeyboardComposer.fieldReset
    @FocusState private var typing: Bool

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                TextField(L10n.Input.typeHere, text: $text)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.return)
                    .focused($typing)
                    .onChange(of: text) { _, new in
                        guard new != KeyboardComposer.fieldReset else { return }
                        model.fieldChanged(new)
                        text = KeyboardComposer.fieldReset
                    }
                    .onSubmit {
                        model.press(RemoteKey.enter)
                        typing = true
                    }

                HStack(spacing: 8) {
                    ForEach(KeyModifier.allCases, id: \.self) { mod in
                        let on = model.latched.contains(mod)
                        Button {
                            model.toggle(mod)
                        } label: {
                            Text(L10n.Input.modifierLabel(mod.rawValue)).frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                        .tint(on ? .accentColor : .secondary)
                        .accessibilityAddTraits(on ? .isSelected : [])
                    }
                }

                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 4), spacing: 8) {
                    ForEach(RemoteKey.special, id: \.self) { key in
                        Button {
                            model.press(key)
                            InputHaptics.click()
                        } label: {
                            Text(verbatim: L10n.Input.keyLabel(key)).frame(maxWidth: .infinity, minHeight: 36)
                        }
                        .buttonStyle(.bordered)
                        .accessibilityLabel(Text(verbatim: key))
                    }
                }

                HStack(spacing: 8) {
                    mediaKey(RemoteKey.volumeDown, "speaker.minus.fill", L10n.Input.volumeDown)
                    mediaKey(RemoteKey.volumeMute, "speaker.slash.fill", L10n.Input.mute)
                    mediaKey(RemoteKey.volumeUp, "speaker.plus.fill", L10n.Input.volumeUp)
                    mediaKey(RemoteKey.playPause, "playpause.fill", L10n.Input.playPause)
                }
            }
        }
        .onAppear { typing = true }
    }

    private func mediaKey(_ key: String, _ symbol: String, _ label: String) -> some View {
        Button {
            model.mediaKey(key)
            InputHaptics.click()
        } label: {
            Image(systemName: symbol).frame(maxWidth: .infinity, minHeight: 36)
        }
        .buttonStyle(.bordered)
        .accessibilityLabel(Text(label))
    }
}
