package org.playdrop.android.share

import android.content.Context
import org.playdrop.android.R

/** [Phrases] from string resources; only the "it went" sentence differs per kind of send. */
internal class SharePhrases private constructor(
    private val context: Context,
    private val sentLine: (String) -> String,
) : Phrases {
    private val res get() = context.resources

    override val and: String get() = context.getString(R.string.share_and)

    override fun sent(names: String): String = sentLine(names)

    override fun waiting(names: String, count: Int): String = res.getQuantityString(R.plurals.share_report_waiting, count, names)

    override fun offline(names: String, count: Int): String = res.getQuantityString(R.plurals.share_report_offline, count, names)

    override fun clipboardOff(names: String): String = context.getString(R.string.share_report_clipboard_off, names)

    override fun noImages(names: String): String = context.getString(R.string.share_report_no_images, names)

    override fun alreadySent(names: String): String = context.getString(R.string.share_report_already_sent, names)

    override fun notPaired(names: String, count: Int): String = res.getQuantityString(R.plurals.share_report_not_paired, count, names)

    override fun notTaken(names: String): String = context.getString(R.string.share_report_not_taken, names)

    override fun failed(name: String, reason: String): String = context.getString(R.string.share_report_failed, name, reason)

    companion object {
        /** Files: "Sending 3 items to A and B." */
        fun files(context: Context, items: Int) = SharePhrases(context) { names ->
            context.resources.getQuantityString(R.plurals.share_report_sending, items, items, names)
        }

        /** The phone's clipboard: "Sent to A and B." */
        fun clipboard(context: Context) = SharePhrases(context) { names -> context.getString(R.string.share_report_sent, names) }

        /** Shared text: "Copied to A and B." */
        fun sharedText(context: Context) = SharePhrases(context) { names -> context.getString(R.string.share_report_copied, names) }
    }
}

/** Joins device labels with the localised "and". */
internal fun Context.joinNames(names: List<String>): String = NameList.join(names, getString(R.string.share_and))
