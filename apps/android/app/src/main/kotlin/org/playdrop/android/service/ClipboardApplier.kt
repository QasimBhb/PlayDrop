package org.playdrop.android.service

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.playdrop.android.core.MediaStoreSink
import java.io.File

/**
 * Writes clips received from peers to the system clipboard. A clip the sender marked
 * sensitive carries `EXTRA_IS_SENSITIVE`, so keyboards and the paste preview hide it (SR-24).
 * Clip contents are never logged or shown in a notification (SR-32).
 */
internal object ClipboardApplier {
    /** Label on our clips; the system shows it in some paste previews. */
    private const val LABEL = "PlayDrop"

    /** `ClipDescription.EXTRA_IS_SENSITIVE`, spelled out for API levels below 33. */
    const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"

    /** Sets clipboard text. */
    suspend fun applyText(context: Context, text: String, sensitive: Boolean) {
        val clip = ClipData.newPlainText(LABEL, text)
        set(context, clip, sensitive)
    }

    /**
     * Sets a clipboard image. The bytes go to one file in the cache, shared through the app's
     * FileProvider; the clipboard grants readers access to that URI. The previous image is
     * replaced, so at most one clipboard image is kept.
     */
    suspend fun applyImage(context: Context, bytes: ByteArray, mime: String, sensitive: Boolean) {
        val app = context.applicationContext
        val ext = when (mime.lowercase()) {
            "image/png" -> "png"
            "image/jpeg", "image/jpg" -> "jpg"
            "image/gif" -> "gif"
            "image/webp" -> "webp"
            else -> "img"
        }
        val uri = withContext(Dispatchers.IO) {
            val dir = File(app.cacheDir, "clipboard").apply { mkdirs() }
            dir.listFiles()?.forEach { it.delete() }
            val file = File(dir, "clip-${System.currentTimeMillis()}.$ext")
            file.writeBytes(bytes)
            FileProvider.getUriForFile(app, MediaStoreSink.fileProviderAuthority(app), file)
        }
        val clip = ClipData(ClipDescription(LABEL, arrayOf(mime)), ClipData.Item(uri))
        set(app, clip, sensitive)
    }

    private suspend fun set(context: Context, clip: ClipData, sensitive: Boolean) {
        if (sensitive) {
            clip.description.extras = PersistableBundle().apply {
                putBoolean(if (Build.VERSION.SDK_INT >= 33) ClipDescription.EXTRA_IS_SENSITIVE else EXTRA_IS_SENSITIVE, true)
            }
        }
        withContext(Dispatchers.Main) {
            val cm = context.applicationContext.getSystemService(ClipboardManager::class.java)
            cm.setPrimaryClip(clip)
        }
    }
}
