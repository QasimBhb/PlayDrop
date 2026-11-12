package org.playdrop.android.share

/**
 * The English wording of strings_share.xml for tests. [sent] is the "it went" sentence of the
 * kind of send ("Sent to A." for clips, "Sending 3 items to A." for files); [items] is the
 * item count of a file send, for "Sent 3 items to A.".
 */
internal class EnglishPhrases(
    private val items: Int = 0,
    private val sentLine: (String) -> String = { "Sent to $it." },
) : Phrases {
    override val and = "and"
    override fun sent(names: String) = sentLine(names)
    override fun delivered(names: String) =
        if (items == 0) "Sent to $names." else "Sent $items ${if (items == 1) "item" else "items"} to $names."
    override fun stopped(names: String) = "Stopped sending to $names."
    override fun notSent(names: String) = "Not sent to $names."
    override fun waiting(names: String, count: Int) =
        "$names ${if (count == 1) "is" else "are"} offline. PlayDrop keeps trying for 10 minutes."
    override fun offline(names: String, count: Int) = "$names ${if (count == 1) "is" else "are"} offline."
    override fun clipboardOff(names: String) = "Clipboard to $names is off."
    override fun noImages(names: String) = "$names cannot take copied images."
    override fun alreadySent(names: String) = "Already sent to $names."
    override fun notPaired(names: String, count: Int) = "$names ${if (count == 1) "is" else "are"} no longer paired."
    override fun notTaken(names: String) = "$names did not take it."
    override fun failed(name: String, reason: String) = "Could not send to $name: $reason"

    companion object {
        /** Files: "Sending 3 items to A." while going, "Sent 3 items to A." once arrived. */
        fun files(items: Int) = EnglishPhrases(items) { names -> "Sending $items ${if (items == 1) "item" else "items"} to $names." }
    }
}
