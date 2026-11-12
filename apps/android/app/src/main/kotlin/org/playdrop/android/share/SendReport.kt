package org.playdrop.android.share

/** What happened for one chosen device. Names come from the local label. */
internal sealed class DeviceResult {
    abstract val label: String

    /** Sent (a clip) or started while the device was online (files). */
    data class Sent(override val label: String) : DeviceResult()

    /** Files: started while the device was offline; the core keeps trying for 10 minutes. */
    data class Waiting(override val label: String) : DeviceResult()

    /** A clip that did not go because the device was offline. */
    data class Offline(override val label: String) : DeviceResult()

    /** A clip that did not go because the device's clipboard switch is off. */
    data class ClipboardOff(override val label: String) : DeviceResult()

    /** A copied image that did not go because the device cannot take images. */
    data class NoImages(override val label: String) : DeviceResult()

    /** The echo rule kept it back: this clip was the last one sent or received. */
    data class AlreadySent(override val label: String) : DeviceResult()

    /** A clip for a device that is no longer paired (removed while the chooser was open). */
    data class NotPaired(override val label: String) : DeviceResult()

    /** A clip the core skipped for a reason that cannot be tied to this device. */
    data class NotTaken(override val label: String) : DeviceResult()

    /** Could not start or send, with the core's short reason. */
    data class Failed(override val label: String, val reason: String) : DeviceResult()
}

/** Joins device names in plain English: "A", "A and B", "A, B and C". */
internal object NameList {
    fun join(names: List<String>, and: String): String = when (names.size) {
        0 -> ""
        1 -> names.single()
        else -> names.dropLast(1).joinToString(", ") + " $and " + names.last()
    }
}

/**
 * The wording of a result toast, one sentence per group. The activity backs it with string
 * resources; tests back it with plain English.
 */
internal interface Phrases {
    /** The word between the last two names. */
    val and: String

    /** "Sent to A and B." for clips, "Sending 3 items to A and B." for files. */
    fun sent(names: String): String

    /** Files to offline devices: "B is offline. PlayDrop keeps trying for 10 minutes." */
    fun waiting(names: String, count: Int): String

    /** "B is offline." */
    fun offline(names: String, count: Int): String

    /** "Clipboard to B is off." */
    fun clipboardOff(names: String): String

    /** "B cannot take copied images." */
    fun noImages(names: String): String

    /** "Already sent to B." */
    fun alreadySent(names: String): String

    /** "B is no longer paired." */
    fun notPaired(names: String, count: Int): String

    /** "B did not take it." (the reason is not known for this device) */
    fun notTaken(names: String): String

    /** "Could not send to B: reason." */
    fun failed(name: String, reason: String): String
}

/** Builds the one message a send with several devices ends with. */
internal object SendReport {
    /**
     * Sentences in a fixed order (what went first, then what did not), each naming its
     * devices in the order of [results]. Failures name one device each, since reasons differ.
     */
    fun sentence(results: List<DeviceResult>, p: Phrases): String {
        fun names(of: List<DeviceResult>) = NameList.join(of.map { it.label }, p.and)
        val out = mutableListOf<String>()
        results.filterIsInstance<DeviceResult.Sent>().takeIf { it.isNotEmpty() }?.let { out += p.sent(names(it)) }
        results.filterIsInstance<DeviceResult.Waiting>().takeIf { it.isNotEmpty() }?.let { out += p.waiting(names(it), it.size) }
        results.filterIsInstance<DeviceResult.AlreadySent>().takeIf { it.isNotEmpty() }?.let { out += p.alreadySent(names(it)) }
        results.filterIsInstance<DeviceResult.Offline>().takeIf { it.isNotEmpty() }?.let { out += p.offline(names(it), it.size) }
        results.filterIsInstance<DeviceResult.ClipboardOff>().takeIf { it.isNotEmpty() }?.let { out += p.clipboardOff(names(it)) }
        results.filterIsInstance<DeviceResult.NoImages>().takeIf { it.isNotEmpty() }?.let { out += p.noImages(names(it)) }
        results.filterIsInstance<DeviceResult.NotPaired>().takeIf { it.isNotEmpty() }?.let { out += p.notPaired(names(it), it.size) }
        results.filterIsInstance<DeviceResult.NotTaken>().takeIf { it.isNotEmpty() }?.let { out += p.notTaken(names(it)) }
        results.filterIsInstance<DeviceResult.Failed>().forEach { out += p.failed(it.label, it.reason) }
        return out.joinToString(" ")
    }
}
