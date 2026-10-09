package org.playdrop.android.core

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.FileUtils
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.annotation.RequiresApi
import androidx.core.content.FileProvider
import org.playdrop.core.DeliverItem
import org.playdrop.core.PlayDropException
import org.playdrop.core.ReceiveSink
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.InputStream
import java.io.OutputStream

/**
 * Hands verified files to the shared Downloads collection, under `Download/PlayDrop/<dirs>`.
 * The core stages and verifies each file in its private staging directory first, then calls
 * [deliver] with an open descriptor; the core deletes the staging copy afterwards.
 *
 * Received files are never executable by construction (SR-23): shared storage is mounted
 * `noexec`, and the notification's "open" action for risky types (APKs, scripts) shows the
 * Downloads list instead of launching the file.
 *
 * Android 8 and 9 (API 26 to 28) have no `MediaStore.Downloads`; there files go to the app's
 * own external `Download/PlayDrop` folder and are shared through [FileProvider], which needs
 * no storage permission. They are not visible in the system Downloads app on those versions.
 */
internal class MediaStoreSink(context: Context) : ReceiveSink {
    private val app = context.applicationContext

    /** Copies the verified file to its final place and returns its `content://` URI. */
    override fun deliver(item: DeliverItem, fd: Int): String {
        // Adopt first so the descriptor is closed on every path, including failures below.
        ParcelFileDescriptor.adoptFd(fd).use { pfd ->
            val name = FileNames.receivedName(item.path)
            val mime = item.mime?.takeIf { it.isNotBlank() && it.contains('/') } ?: guessMime(name)
            return try {
                FileInputStream(pfd.fileDescriptor).use { input ->
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        deliverToMediaStore(item, name, mime, input)
                    } else {
                        deliverToAppFolder(item, name, input)
                    }
                }
            } catch (e: PlayDropException) {
                throw e
            } catch (e: Exception) {
                HostLog.w("sink", "saving a received file failed", e)
                throw PlayDropException.Other("Could not save $name to Downloads (${e.javaClass.simpleName}: ${e.message})")
            }
        }
    }

    /**
     * MediaStore keeps no empty folders: a folder appears when its first file is inserted with
     * that `RELATIVE_PATH`. An empty manifest directory is therefore a deliberate no-op here.
     */
    override fun makeDir(path: List<String>) {
        HostLog.d("sink", "empty folder ${path.joinToString("/")} skipped (MediaStore has no empty folders)")
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun deliverToMediaStore(item: DeliverItem, name: String, mime: String, input: InputStream): String {
        val resolver = app.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, FileNames.relativePath(Environment.DIRECTORY_DOWNLOADS, item.path))
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri: Uri = resolver.insert(collection, values)
            ?: throw PlayDropException.Other("Could not save $name: the Downloads collection refused a new entry")
        try {
            val out = resolver.openFileDescriptor(uri, "w")
                ?: throw PlayDropException.Other("Could not save $name: Downloads did not open the new file")
            out.use { opfd ->
                FileOutputStream(opfd.fileDescriptor).use { output ->
                    FileUtils.copy(input, output)
                    output.fd.sync()
                }
            }
            val done = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
            resolver.update(uri, done, null, null)
        } catch (e: Exception) {
            // Remove the half-written entry so Downloads never shows a broken file.
            runCatching { resolver.delete(uri, null, null) }
                .onFailure { HostLog.w("sink", "removing a failed download entry failed", it) }
            throw e
        }
        return uri.toString()
    }

    private fun deliverToAppFolder(item: DeliverItem, name: String, input: InputStream): String {
        val base = app.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: File(app.filesDir, "downloads")
        val rel = FileNames.relativePath(".", item.path).removePrefix("./")
        val dir = File(base, rel).apply { mkdirs() }
        val target = uniqueFile(dir, name)
        FileOutputStream(target).use { output ->
            copyStream(input, output)
            output.fd.sync()
        }
        return FileProvider.getUriForFile(app, fileProviderAuthority(app), target).toString()
    }

    /** `name`, `name (1)`, `name (2)`... the first that does not exist yet. */
    private fun uniqueFile(dir: File, name: String): File {
        var candidate = File(dir, name)
        val ext = FileNames.extension(name)
        val stem = if (ext == null) name else name.dropLast(ext.length + 1)
        var n = 1
        while (candidate.exists()) {
            candidate = File(dir, if (ext == null) "$stem ($n)" else "$stem ($n).$ext")
            n++
        }
        return candidate
    }

    private fun copyStream(input: InputStream, output: OutputStream) {
        val buf = ByteArray(COPY_BUFFER)
        while (true) {
            val n = input.read(buf)
            if (n < 0) break
            output.write(buf, 0, n)
        }
    }

    companion object {
        private const val COPY_BUFFER = 256 * 1024

        /** MIME type from the file extension, or `application/octet-stream`. */
        fun guessMime(name: String): String =
            FileNames.extension(name)?.let { MimeTypeMap.getSingleton().getMimeTypeFromExtension(it) }
                ?: "application/octet-stream"

        /** Authority of the app's [FileProvider] (declared in the manifest). */
        fun fileProviderAuthority(context: Context): String = "${context.packageName}.files"
    }
}
