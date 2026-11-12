package org.playdrop.android.share

import org.playdrop.core.PeerSummary

/**
 * A paired device as the device chooser sees it. Built from [PeerSummary] so the selection
 * rules stay pure and unit-tested.
 */
internal data class Candidate(
    /** Hex Device ID. */
    val id: String,
    /** The local label, or the Short ID when the label is blank. */
    val label: String,
    /** Short ID (SR-8), shown under the label. */
    val shortId: String,
    /** A session is up. */
    val online: Boolean,
    /** The "Receive this phone's clipboard" switch (`clipboardOut` grant). */
    val clipboardOut: Boolean,
    /** What the device accepts while online (`hello.caps.in`); empty offline. */
    val capabilities: Set<String> = emptySet(),
    /**
     * The device removed this phone (`PeerSummary.removedByPeer`): nothing reaches it until
     * the two are paired again, so it is never a target.
     */
    val removed: Boolean = false,
) {
    companion object {
        /** The candidate for one paired peer. */
        fun of(p: PeerSummary): Candidate = Candidate(
            id = p.deviceId,
            label = p.label.ifBlank { p.shortId },
            shortId = p.shortId,
            online = p.online,
            clipboardOut = p.grants.clipboardOut,
            capabilities = p.capabilities.toSet(),
            removed = p.removedByPeer,
        )
    }
}

/** Why a chooser row cannot be ticked. */
internal enum class Blocked {
    /** The device removed this phone; it must be paired again first. */
    REMOVED,

    /** Clipboard sends need a session now; the device has none. */
    OFFLINE,

    /** The device's "Receive this phone's clipboard" switch is off. */
    CLIPBOARD_OFF,

    /** The device's build cannot take a copied image (no `clipboard.image` or `blobs`). */
    NO_IMAGES,
}

/** What is being sent, which decides which devices can take it. */
internal enum class SendKind {
    /** Shared or picked files: every paired device; an offline one keeps being tried. */
    FILES,

    /** Text from the share sheet: goes to the device's clipboard, dialling it first. */
    SHARED_TEXT,

    /** The phone's clipboard text, from the notification. */
    CLIPBOARD_TEXT,

    /** The phone's clipboard image, from the notification. */
    CLIPBOARD_IMAGE,
    ;

    /** True for the kinds that end on the other device's clipboard. */
    val isClip: Boolean get() = this != FILES

    /** True when only an online device can take it (no dialling, no retry). */
    val needsOnline: Boolean get() = this == CLIPBOARD_TEXT || this == CLIPBOARD_IMAGE

    /** Why [c] cannot take this kind, or null when it can. */
    fun blocked(c: Candidate): Blocked? = when {
        // Before anything else: a device that removed this phone takes nothing at all.
        c.removed -> Blocked.REMOVED
        else -> blockedPaired(c)
    }

    private fun blockedPaired(c: Candidate): Blocked? = when (this) {
        FILES -> null
        SHARED_TEXT -> if (!c.clipboardOut) Blocked.CLIPBOARD_OFF else null
        CLIPBOARD_TEXT, CLIPBOARD_IMAGE -> when {
            // The switch first: coming online would not help a device that refuses the clip.
            !c.clipboardOut -> Blocked.CLIPBOARD_OFF
            !c.online -> Blocked.OFFLINE
            this == CLIPBOARD_IMAGE && !takesImages(c) -> Blocked.NO_IMAGES
            else -> null
        }
    }

    companion object {
        /** Capability a device lists when it takes copied images (protocol.md 5.7). */
        const val CAP_CLIPBOARD_IMAGE = "clipboard.image"

        /** Capability for blob fetches, which copied images travel as (protocol.md 5.15). */
        const val CAP_BLOBS = "blobs"

        /** True when [c] lists both capabilities a copied image needs. */
        fun takesImages(c: Candidate): Boolean =
            CAP_CLIPBOARD_IMAGE in c.capabilities && CAP_BLOBS in c.capabilities
    }
}

/** One row of the chooser. */
internal data class ChooserRow(
    val id: String,
    val label: String,
    val shortId: String,
    val online: Boolean,
    val isDefault: Boolean,
    /** Why the row cannot be ticked, or null when it can. */
    val blocked: Blocked?,
) {
    /** True when the row has a tick box the person can use. */
    val selectable: Boolean get() = blocked == null
}

/**
 * The chooser's state: the rows in display order and the ticked IDs. Immutable; every action
 * returns a new value.
 */
internal data class Selection(val rows: List<ChooserRow>, val ticked: Set<String>) {
    private val selectableIds: List<String> get() = rows.filter { it.selectable }.map { it.id }

    /** Every row that can be ticked is ticked (and there is at least one). */
    val allTicked: Boolean get() = selectableIds.let { ids -> ids.isNotEmpty() && ticked.containsAll(ids) }

    /** "Select all" has something to do or undo. */
    val canToggleAll: Boolean get() = selectableIds.isNotEmpty()

    /** How many devices "Send" goes to. */
    val count: Int get() = ticked.size

    /** "Send" is enabled: at least one device is ticked. */
    val canSend: Boolean get() = ticked.isNotEmpty()

    /** The ticked IDs in row order. */
    val chosen: List<String> get() = rows.filter { it.id in ticked }.map { it.id }

    /** IDs of the rows that can be ticked, in row order. */
    val eligible: List<String> get() = selectableIds

    /** The default device's ID, if it is listed. */
    val defaultId: String? get() = rows.firstOrNull { it.isDefault }?.id

    /** Ticks or unticks [id]; a row that cannot be ticked is left alone. */
    fun toggle(id: String): Selection {
        if (rows.none { it.id == id && it.selectable }) return this
        return copy(ticked = if (id in ticked) ticked - id else ticked + id)
    }

    /** "Select all" ticks every row that can be ticked; once all are, it is "Clear". */
    fun toggleAll(): Selection = if (allTicked) copy(ticked = emptySet()) else copy(ticked = selectableIds.toSet())

    /** Moves the "Default" marker to [id]; ticks are unchanged. */
    fun withDefault(id: String): Selection = copy(rows = rows.map { it.copy(isDefault = it.id == id) })

    companion object {
        /**
         * Rows for [candidates], online first, then by label (ignoring case), then Short ID.
         * The default is pre-ticked when it can take [kind]; otherwise nothing is.
         */
        fun of(candidates: List<Candidate>, defaultId: String?, kind: SendKind): Selection {
            val rows = candidates
                .sortedWith(compareByDescending<Candidate> { it.online }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.label }.thenBy { it.shortId })
                .map { c -> ChooserRow(c.id, c.label, c.shortId, c.online, c.id == defaultId, kind.blocked(c)) }
            val pre = rows.firstOrNull { it.isDefault && it.selectable }?.id
            return Selection(rows, setOfNotNull(pre))
        }
    }
}

/** How a send starts, before anything is shown. */
internal sealed class Launch {
    /** No paired device at all. */
    data object NoDevices : Launch()

    /**
     * Exactly one way to go: the share card's 2 s countdown for a share, or an immediate send
     * from a notification action. No chooser.
     */
    data class Direct(val id: String) : Launch()

    /** More than one device: the chooser. */
    data class Choose(val selection: Selection) : Launch()

    /** Devices are paired but none can take this clip now; [reason] says why. */
    data class NoneEligible(val reason: ClipboardOutcome) : Launch()

    companion object {
        /**
         * Shares and picked files: one usable device goes straight to it (the one-tap path),
         * more than one opens the chooser. A device that removed this phone never counts as
         * the one device: it is listed in the chooser, greyed out with "Pair again to send",
         * and when it is all there is the chooser shows just that.
         */
        fun forShare(candidates: List<Candidate>, defaultId: String?, kind: SendKind): Launch {
            if (candidates.isEmpty()) return NoDevices
            val usable = candidates.filter { !it.removed }
            return if (usable.size == 1) Direct(usable.single().id) else Choose(Selection.of(candidates, defaultId, kind))
        }

        /**
         * The notification's "Send clipboard": only devices that would receive it count. One
         * such device is sent to at once, more than one opens the chooser (the others listed
         * greyed out with their reason), none gives the plain reason.
         */
        fun forClipboard(candidates: List<Candidate>, defaultId: String?, kind: SendKind): Launch {
            require(kind.needsOnline) { "not a clipboard kind: $kind" }
            if (candidates.none { !it.removed }) return NoneEligible(ClipboardOutcome.NoPairedDevice)
            val blocked = candidates.map { kind.blocked(it) }
            val eligible = candidates.filterIndexed { i, _ -> blocked[i] == null }
            return when (eligible.size) {
                0 -> NoneEligible(noneReason(blocked, candidates.size))
                1 -> Direct(eligible.single().id)
                else -> Choose(Selection.of(candidates, defaultId, kind))
            }
        }

        /** The same wording rules as a core push that reached nobody. */
        private fun noneReason(blocked: List<Blocked?>, paired: Int): ClipboardOutcome {
            if (blocked.isNotEmpty() && blocked.all { it == Blocked.NO_IMAGES }) return ClipboardOutcome.NoImageSupport
            return ClipboardOutcome.of(
                sentTo = 0,
                skippedEcho = false,
                skippedOffline = blocked.count { it == Blocked.OFFLINE },
                skippedNoGrant = blocked.count { it == Blocked.CLIPBOARD_OFF },
                pairedCount = paired - blocked.count { it == Blocked.REMOVED },
            )
        }
    }
}

/** The rule for how a default device comes to exist: the first single pick (unchanged). */
internal object DefaultRule {
    /** The device to make default after a send to [chosen], or null to leave it. */
    fun after(currentDefault: String?, chosen: List<String>): String? =
        if (currentDefault == null && chosen.size == 1) chosen.single() else null
}
