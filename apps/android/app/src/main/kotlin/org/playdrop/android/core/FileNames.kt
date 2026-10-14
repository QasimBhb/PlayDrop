package org.playdrop.android.core

/**
 * Pure name rules shared by the send and receive adapters. No Android types, so they are
 * unit-tested on the JVM.
 */
internal object FileNames {
    /** Used when a provider gives no usable name at all. */
    const val FALLBACK_NAME = "file"

    /** Folder under Downloads that received files go into. */
    const val RECEIVE_FOLDER = "PlayDrop"

    /** Longest name we pass on; ext4 and FAT both stop at 255 bytes. */
    private const val MAX_NAME_CHARS = 200

    /**
     * Extensions that run code or install something when opened. Tapping a received file of
     * this kind opens the Downloads list instead of the file itself (SR-23).
     */
    private val RISKY_EXTENSIONS = setOf(
        "apk", "apks", "xapk", "aab", "dex", "jar", "sh", "bash", "zsh", "py", "pl", "rb",
        "exe", "msi", "bat", "cmd", "com", "scr", "ps1", "vbs", "js", "lnk", "desktop",
        "appimage", "deb", "rpm", "run", "bin",
    )

    /**
     * Picks the name to send for a shared item: the provider's display name, else the URI's
     * last path segment, else [FALLBACK_NAME]. The result is one plain path component (SR-17):
     * separators, control characters and bidi overrides are replaced, and `.`/`..` refused.
     */
    fun sendName(displayName: String?, lastPathSegment: String?): String =
        sanitize(displayName) ?: sanitize(lastPathSegment?.substringAfterLast('/')) ?: FALLBACK_NAME

    /** Cleans one name, or null if nothing usable is left. */
    fun sanitize(raw: String?): String? {
        if (raw == null) return null
        val cleaned = buildString {
            for (ch in raw) {
                append(
                    when {
                        ch == '/' || ch == '\\' -> '_'
                        ch.isISOControl() -> '_'
                        ch in '‪'..'‮' || ch in '⁦'..'⁩' || ch == '‎' || ch == '‏' -> '_'
                        else -> ch
                    },
                )
            }
        }.trim().take(MAX_NAME_CHARS)
        return cleaned.takeUnless { it.isEmpty() || it == "." || it == ".." }
    }

    /** Lower-case extension without the dot, or null when the name has none. */
    fun extension(name: String): String? {
        val dot = name.lastIndexOf('.')
        if (dot <= 0 || dot == name.length - 1) return null
        return name.substring(dot + 1).lowercase()
    }

    /**
     * MediaStore `RELATIVE_PATH` for a received file whose manifest path is [path]:
     * `Download/PlayDrop/<dirs>/`. The last component is the file name and is left out.
     */
    fun relativePath(downloadsDir: String, path: List<String>): String =
        (listOf(downloadsDir, RECEIVE_FOLDER) + receivedDirs(path)).joinToString("/", postfix = "/")

    /**
     * Folders under `Download/PlayDrop` for a received file whose manifest path is [path]:
     * every component but the last, each cleaned like a name (SR-17), empty ones dropped.
     * The result never holds a separator, `.` or `..`, so joining it under the receive folder
     * cannot leave that folder.
     */
    fun receivedDirs(path: List<String>): List<String> = path.dropLast(1).mapNotNull { sanitize(it) }

    /**
     * The [n]th name tried when [name] is taken: `name`, `name (1)`, `name (2)`... with the
     * number before the extension (`a.txt` -> `a (1).txt`). [n] 0 is the name itself.
     */
    fun numberedName(name: String, n: Int): String {
        if (n <= 0) return name
        val ext = extension(name) ?: return "$name ($n)"
        return "${name.dropLast(ext.length + 1)} ($n).${name.takeLast(ext.length)}"
    }

    /** Display name for a received file: the last manifest component, cleaned. */
    fun receivedName(path: List<String>): String = sanitize(path.lastOrNull()) ?: FALLBACK_NAME

    /** True when opening a file with this name or MIME type could run or install code (SR-23). */
    fun isRiskyToOpen(name: String, mime: String?): Boolean {
        val ext = extension(name)
        if (ext != null && ext in RISKY_EXTENSIONS) return true
        val m = mime?.lowercase() ?: return false
        return m == "application/vnd.android.package-archive" ||
            m.startsWith("application/x-sh") ||
            m == "application/x-executable" ||
            m == "application/x-msdownload" ||
            m == "text/x-shellscript"
    }
}
