package org.playdrop.android.ui.adddevice

import org.playdrop.android.ui.common.Addresses
import org.playdrop.core.AddDeviceCandidate

/**
 * Collapsing the core's candidate list, which has one entry per address, into one row per
 * device. Before pairing the name plus type is the only identity there is; the row keeps the
 * address most likely to work (private IPv4 first), like the desktop's `merge_candidate`.
 */
object Candidates {
    /** One entry per name and type, in first-seen order, each with its best address. */
    fun collapse(list: List<AddDeviceCandidate>): List<AddDeviceCandidate> {
        val out = ArrayList<AddDeviceCandidate>(list.size)
        for (c in list) {
            val i = out.indexOfFirst { it.name == c.name && it.deviceType == c.deviceType }
            if (i < 0) {
                out += c
            } else if (Addresses.rank(c.addr) < Addresses.rank(out[i].addr)) {
                out[i] = c
            }
        }
        return out
    }
}

/** The kind of device a candidate announced, for its icon and label. */
enum class CandidateKind {
    /** A desktop computer. */
    COMPUTER,

    /** A laptop. */
    LAPTOP,

    /** A phone. */
    PHONE,

    /** A tablet. */
    TABLET,

    /** A TV or anything else. */
    OTHER,
    ;

    companion object {
        /** Maps the wire token (`desktop`, `laptop`, `phone`, `tablet`, `tv`). */
        fun of(token: String): CandidateKind = when (token.lowercase()) {
            "desktop" -> COMPUTER
            "laptop" -> LAPTOP
            "phone", "mobile" -> PHONE
            "tablet" -> TABLET
            else -> OTHER
        }
    }
}
