package org.playdrop.android.share

import org.playdrop.core.ClipboardPush

/**
 * What the core reported for one clip push to a chosen set of devices
 * (`push_clipboard_to` or `push_clipboard_image_to`): one echo check covers the whole push,
 * so any set of devices can be reached with one call.
 */
internal data class Pushed(
    /** Devices the clip went to. */
    val sentTo: Set<String>,
    /** The core skipped the push as an echo of the last clip it sent. */
    val echo: Boolean,
    /** Chosen devices that are no longer paired. */
    val notPaired: Set<String> = emptySet(),
    /** How many targets the core skipped because they were offline (a count, not names). */
    val skippedOffline: Int = 0,
    /** How many targets the core skipped because their clipboard switch was off. */
    val skippedNoGrant: Int = 0,
    /** How many targets the core skipped because they cannot take this kind of clip. */
    val skippedUnsupported: Int = 0,
) {
    /**
     * Why the core skipped the targets it did not name: the core only counts its reasons, so
     * a reason is tied to those devices only when it is the single one counted. Null when
     * several reasons were counted, or none.
     */
    val skippedReason: Blocked?
        get() {
            val counted = listOfNotNull(
                Blocked.OFFLINE.takeIf { skippedOffline > 0 },
                Blocked.CLIPBOARD_OFF.takeIf { skippedNoGrant > 0 },
                Blocked.NO_IMAGES.takeIf { skippedUnsupported > 0 },
            )
            return counted.singleOrNull()
        }

    companion object {
        /**
         * The core's answer as the app reads it. `sentTo` lists by Device ID every device the
         * clip went to (docs/core-api.md section 16); the skip counters only count, and
         * `skippedNotPaired` lists the chosen IDs that are not paired.
         */
        fun of(p: ClipboardPush): Pushed = Pushed(
            sentTo = p.sentTo.toSet(),
            echo = p.skippedEcho,
            notPaired = p.skippedNotPaired.toSet(),
            skippedOffline = p.skippedOffline.toInt(),
            skippedNoGrant = p.skippedNoGrant.toInt(),
            skippedUnsupported = p.skippedUnsupported.toInt(),
        )
    }
}

/** Pure rules for clip sends to one or several devices. */
internal object ClipPlan {
    /**
     * The devices of [chosen] that one push should go to: those in [eligibleNow] (online,
     * switch on and, for an image, able to take it), in [chosen] order. Empty means no call.
     */
    fun targets(chosen: List<String>, eligibleNow: Collection<String>): List<String> =
        chosen.filter { it in eligibleNow }

    /**
     * Why each chosen device cannot take a clip of [kind] right now, by ID; null for a device
     * that can. A chosen device missing from [candidates] is no longer paired.
     *
     * Kept apart from the null of "can take it": `blocked(c)` returns null for an eligible
     * device, so folding a missing candidate in with `?:` would call every eligible device
     * offline (the toast said "A and B are offline" for a clip both had received).
     */
    fun blockedNow(chosen: List<String>, candidates: List<Candidate>, kind: SendKind): Map<String, Blocked?> =
        chosen.associateWith { id ->
            val c = candidates.firstOrNull { it.id == id }
            if (c == null) Blocked.REMOVED else kind.blocked(c)
        }

    /**
     * One result per chosen device, in [chosen] order. [blockedNow] holds why a chosen device
     * could not take the clip at send time (absent or null when it could). [error] is the
     * core's reason when the push call itself failed; [pushed] is null when no call was made.
     */
    fun results(
        chosen: List<String>,
        label: (String) -> String,
        blockedNow: Map<String, Blocked?>,
        pushed: Pushed?,
        error: String?,
    ): List<DeviceResult> = chosen.map { id ->
        val name = label(id)
        when {
            blockedNow[id] == Blocked.REMOVED -> DeviceResult.NotPaired(name)
            blockedNow[id] == Blocked.OFFLINE -> DeviceResult.Offline(name)
            blockedNow[id] == Blocked.CLIPBOARD_OFF -> DeviceResult.ClipboardOff(name)
            blockedNow[id] == Blocked.NO_IMAGES -> DeviceResult.NoImages(name)
            error != null -> DeviceResult.Failed(name, error)
            pushed == null -> DeviceResult.Offline(name)
            id in pushed.sentTo -> DeviceResult.Sent(name)
            id in pushed.notPaired -> DeviceResult.NotPaired(name)
            pushed.echo -> DeviceResult.AlreadySent(name)
            // Eligible a moment ago but skipped by the core: name the reason only when the
            // core's counts allow tying it to this device, otherwise say it plainly.
            else -> when (pushed.skippedReason) {
                Blocked.OFFLINE -> DeviceResult.Offline(name)
                Blocked.CLIPBOARD_OFF -> DeviceResult.ClipboardOff(name)
                Blocked.NO_IMAGES -> DeviceResult.NoImages(name)
                else -> DeviceResult.NotTaken(name)
            }
        }
    }
}
