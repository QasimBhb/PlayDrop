package org.playdrop.android.ui.input

/**
 * Which remote input features a peer accepts, read from its `PeerSummary.capabilities`
 * (its `hello.caps.in`). A feature the peer does not list is never offered (PLAN.md
 * anti-pattern 18): the desktop only lists `input.*` when it has a working input backend.
 */
object InputSupport {
    /** Pointer moves, buttons and scrolling. */
    const val POINTER = "input.pointer"

    /** Named keys and typed text. */
    const val KEY = "input.key"

    /** Presenter remote actions. */
    const val PRESENTER = "input.presenter"

    /** True when the "Remote input" screen has anything to offer for these capabilities. */
    fun canControl(capabilities: List<String>): Boolean = POINTER in capabilities || KEY in capabilities

    /** What the input screen shows for these capabilities. */
    fun of(capabilities: List<String>): Features = Features(
        pointer = POINTER in capabilities,
        keys = KEY in capabilities,
        presenter = PRESENTER in capabilities,
    )

    /** The input screen's tabs that the peer can take. */
    data class Features(val pointer: Boolean, val keys: Boolean, val presenter: Boolean) {
        /** Any tab at all. */
        val any: Boolean get() = pointer || keys || presenter
    }
}
