package org.playdrop.android.ui.device

import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage

/**
 * Words for a media command the desktop refused (anti-pattern 18: no silent failure). The
 * desktop answers `forbidden` when the Media grant for this phone is off.
 */
object MediaRefusal {
    /** Frame type the core reports for media commands. */
    const val FRAME_MEDIA = "media"

    private const val CODE_FORBIDDEN = "forbidden"

    /**
     * The message for a refusal of [frameType] with [code] and the desktop's [detail], or null
     * when it concerns another frame type. [label] is the local name of the desktop.
     */
    fun message(frameType: String?, code: String, detail: String?, label: String): UiMessage? = when {
        frameType != FRAME_MEDIA -> null
        code == CODE_FORBIDDEN -> UiMessage(R.string.ui_media_refused, listOf(label))
        else -> UiMessage(R.string.ui_media_refused_other, listOf(detail ?: code))
    }
}
