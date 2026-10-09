import Foundation
import PlayDropShared

// The one `ClipboardEcho` of the process, shared by `ClipboardApplier` (which remembers every
// text it puts on the pasteboard) and `ClipboardSender` (which refuses to push that text
// straight back to peers within the echo window). Holds at most the last applied text, in
// memory only, and never logs it (SR-32).

/// Process-wide echo state; see the file comment.
@MainActor
enum ClipboardEchoState {
    /// The echo rule shared by applier and sender.
    static var echo = ClipboardEcho()
}
