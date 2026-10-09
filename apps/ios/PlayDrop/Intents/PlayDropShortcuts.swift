import AppIntents

// The App Shortcuts PlayDrop offers without any setup: Siri and Spotlight phrases for
// "Send Clipboard". Every phrase names the app, as App Shortcuts require.

/// PlayDrop's App Shortcuts.
struct PlayDropShortcuts: AppShortcutsProvider {
    /// The shortcuts and their phrases.
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: SendClipboardIntent(),
            phrases: [
                "Send clipboard with \(.applicationName)",
                "Send my clipboard with \(.applicationName)",
                "\(.applicationName) send clipboard",
            ],
            shortTitle: "Send Clipboard",
            systemImageName: "doc.on.clipboard"
        )
    }
}
