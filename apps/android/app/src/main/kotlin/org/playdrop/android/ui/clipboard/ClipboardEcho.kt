package org.playdrop.android.ui.clipboard

/**
 * The decision for one clipboard change while the app is in the foreground: the echo rule
 * (never push the same text twice in a row, so a clip that arrived from a peer and was
 * written to this clipboard does not bounce back), the sensitive flag (SR-24) and the 1 MiB
 * cap. Holds no clipboard text beyond the last one sent and never logs it (SR-32).
 */
class ClipboardEcho {
    /** What to do with a clip. */
    enum class Decision {
        /** Push it to peers. */
        PUSH,

        /** Same as the last text sent, empty, or not text: nothing to do. */
        SKIP,

        /** Marked sensitive by the source app: not sent, tell the user quietly. */
        SENSITIVE,

        /** Over 1 MiB of text: not synced (SR-24). */
        TOO_LARGE,
    }

    private var lastSent: String? = null

    /** Decides for [text]; null means the clip holds no text. */
    fun decide(text: String?, sensitive: Boolean): Decision = when {
        text.isNullOrEmpty() -> Decision.SKIP
        text == lastSent -> Decision.SKIP
        sensitive -> Decision.SENSITIVE
        text.toByteArray(Charsets.UTF_8).size > MAX_BYTES -> Decision.TOO_LARGE
        else -> Decision.PUSH
    }

    /** Records [text] as sent (or as received from a peer) so it is not pushed again. */
    fun remember(text: String) {
        lastSent = text
    }

    companion object {
        /** Text over this size is not synced (SR-24). */
        const val MAX_BYTES = 1024 * 1024
    }
}
