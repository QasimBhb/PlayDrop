package org.playdrop.android.share

import android.content.ContentResolver
import android.net.Uri
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream

/**
 * Reads a copied image (the `content://` item of an image clip) into memory for
 * `push_clipboard_image` and friends. One reader for the notification action and the device
 * page, so both refuse the same sizes. The bytes are never logged (SR-32).
 */
internal object ClipImage {
    /** Largest copied image the phone sends; the core refuses larger ones as well. */
    const val MAX_BYTES = 16 * 1024 * 1024

    /** The image behind [uri]; throws [IOException] when it is unreadable or over [MAX_BYTES]. Blocks. */
    fun read(resolver: ContentResolver, uri: Uri): ByteArray =
        resolver.openInputStream(uri)?.use(::readLimited)
            ?: throw IOException("the copied image could not be read")

    /** Reads [input] whole, failing once it passes [limit] bytes. */
    fun readLimited(input: InputStream, limit: Int = MAX_BYTES): ByteArray {
        val out = ByteArrayOutputStream()
        val buf = ByteArray(64 * 1024)
        var total = 0
        while (true) {
            val n = input.read(buf)
            if (n < 0) break
            total += n
            if (total > limit) throw IOException("the image is larger than ${limit / (1024 * 1024)} MiB")
            out.write(buf, 0, n)
        }
        return out.toByteArray()
    }
}
