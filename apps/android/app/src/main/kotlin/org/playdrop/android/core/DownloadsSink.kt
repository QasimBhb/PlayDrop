package org.playdrop.android.core

import android.content.ContentValues
import android.content.Context
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.FileUtils
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.webkit.MimeTypeMap
import androidx.annotation.RequiresApi
import androidx.core.content.FileProvider
import org.playdrop.core.DeliverItem
import org.playdrop.core.PlayDropException
import org.playdrop.core.ReceiveSink
import java.io.File
import java.io.FileDescriptor
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.ConcurrentHashMap

/**
 * Hands verified files to `Download/PlayDrop/<dirs>`. The core stages and verifies each file
 * in its private staging directory first, then calls [deliver] with an open descriptor; the
 * core deletes the staging copy afterwards.
 *
 * Three paths, by Android version:
 * - Android 11 and later (API 30): plain file writes into the public `Download/PlayDrop`
 *   tree. Scoped storage lets an app create and write its own files there without any
 *   permission. This costs one create, one kernel copy and one close per file (about 13 ms
 *   on the Galaxy A54, nearly all of it MediaProvider adding the database row behind the
 *   create) instead of the MediaStore insert, open, fsync and `IS_PENDING` update (about
 *   35 ms), which is what made thousands of small files crawl. The written files are handed
 *   to the media scanner in batches ([ScanBatcher]) so they show in Downloads, and the
 *   returned location is a [FileProvider] URI. If the platform refuses a direct write the
 *   sink falls back to the MediaStore path for good.
 * - Android 10 (API 29): `MediaStore.Downloads` with `IS_PENDING`, since direct writes to
 *   shared storage are not allowed there.
 * - Android 8 and 9 (API 26 to 28) have no `MediaStore.Downloads`; there files go to the
 *   app's own external `Download/PlayDrop` folder and are shared through [FileProvider],
 *   which needs no storage permission. They are not visible in the system Downloads app.
 *
 * Received files are never executable by construction (SR-23): shared storage is mounted
 * `noexec`, files are created without an execute bit, and the notification's "open" action
 * for risky types (APKs, scripts) shows the Downloads list instead of launching the file.
 * Nothing is written outside `Download/PlayDrop`: every folder and name goes through the
 * same [FileNames] rules (SR-17), on top of the core's own path checks.
 */
internal class DownloadsSink(context: Context) : ReceiveSink {
    private val app = context.applicationContext
    private val timings = SinkTimings()
    private val scans = ScanBatcher()

    /** Folders already created on the direct path, so each costs one `mkdirs` per process. */
    private val madeDirs: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** Set once the platform refuses a direct write; every later file uses MediaStore. */
    @Volatile
    private var directRefused = false

    @Volatile
    private var fastUriChecked = false

    @Volatile
    private var fastUriOk = false

    /** Copies the verified file to its final place and returns its `content://` URI. */
    override fun deliver(item: DeliverItem, fd: Int): String {
        val start = System.nanoTime()
        // Adopt first so the descriptor is closed on every path, including failures below.
        ParcelFileDescriptor.adoptFd(fd).use { pfd ->
            val name = FileNames.receivedName(item.path)
            return try {
                val location = when {
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !directRefused ->
                        deliverDirect(item, name, pfd.fileDescriptor)
                            ?: deliverToMediaStore(item, name, FileInputStream(pfd.fileDescriptor))
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q ->
                        deliverToMediaStore(item, name, FileInputStream(pfd.fileDescriptor))
                    else -> deliverToAppFolder(item, name, FileInputStream(pfd.fileDescriptor))
                }
                timings.fileDone(item.transferId, item.size.toLong(), start, System.nanoTime())
                location
            } catch (e: PlayDropException) {
                throw e
            } catch (e: Exception) {
                HostLog.w("sink", "saving a received file failed", e)
                throw PlayDropException.Other("Could not save $name to Downloads (${e.javaClass.simpleName}: ${e.message})")
            }
        }
    }

    /**
     * On the direct path an empty manifest folder is created like any other. MediaStore
     * keeps no empty folders (a folder appears when its first file is inserted with that
     * `RELATIVE_PATH`), so on the other paths this is a deliberate no-op.
     */
    override fun makeDir(path: List<String>) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !directRefused) {
            // Every component is a folder here, so the whole path is cleaned like folder names.
            val dir = File(receiveRoot(), FileNames.receivedDirs(path + FileNames.FALLBACK_NAME).joinToString("/"))
            if (dir.isDirectory || dir.mkdirs()) {
                madeDirs.add(dir.path)
                return
            }
            HostLog.d("sink", "empty folder ${path.joinToString("/")} could not be created")
            return
        }
        HostLog.d("sink", "empty folder ${path.joinToString("/")} skipped (MediaStore has no empty folders)")
    }

    /**
     * Called when a transfer completes or fails: hands the last written files to the media
     * scanner and logs what the sink cost (debug level, one line per transfer).
     */
    fun transferEnded(transferId: String) {
        scan(transferId, scans.drain(transferId))
        timings.finish(transferId)?.let { HostLog.d("sink", "transfer ${transferId.take(8)}: ${it.line()}") }
    }

    private inline fun <T> timed(transfer: String, step: String, block: () -> T): T {
        val t0 = System.nanoTime()
        try {
            return block()
        } finally {
            timings.add(transfer, step, System.nanoTime() - t0)
        }
    }

    /** `<shared storage>/Download/PlayDrop`. */
    private fun receiveRoot(): File =
        File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), FileNames.RECEIVE_FOLDER)

    /**
     * Writes the file straight into `Download/PlayDrop/<dirs>`. Returns null, having read
     * nothing from [input], when the platform refuses the write, so the caller can use
     * MediaStore instead.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun deliverDirect(item: DeliverItem, name: String, input: FileDescriptor): String? {
        val t = item.transferId
        if (Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) return null
        val dirs = FileNames.receivedDirs(item.path)
        val dir = File(receiveRoot(), dirs.joinToString("/"))
        val created = timed(t, "create") { createUnique(dir, name) } ?: return null
        val (file, out) = created
        try {
            timed(t, "copy") { FileUtils.copy(input, out) }
            if (ReceivePolicy.syncAfterCopy(item.size.toLong())) timed(t, "sync") { Os.fdatasync(out) }
        } catch (e: Exception) {
            runCatching { Os.close(out) }
            if (!file.delete()) HostLog.w("sink", "removing a half-written file failed")
            throw e
        }
        timed(t, "close") { Os.close(out) }
        scans.add(t, file.path)?.let { scan(t, it) }
        return timed(t, "uri") { receivedUri(file, dirs) }
    }

    /**
     * The [FileProvider] URI of [file], written under [receiveRoot] in [dirs]: the
     * `received` root of `res/xml/file_paths.xml` plus one encoded segment per folder and
     * the name, which is the URI `FileProvider.getUriForFile` builds. Built here because
     * that call resolves the canonical path first, one FUSE lookup per path component (about
     * 0.5 ms a file measured). The first URI of the process is checked against FileProvider;
     * on any difference the sink logs it and uses FileProvider for every file.
     */
    private fun receivedUri(file: File, dirs: List<String>): String {
        val fast = Uri.Builder().scheme("content").authority(fileProviderAuthority(app)).appendPath(RECEIVED_ROOT)
            .apply { (dirs + file.name).forEach { appendPath(it) } }
            .build()
        if (!fastUriChecked) {
            val slow = FileProvider.getUriForFile(app, fileProviderAuthority(app), file)
            fastUriOk = slow == fast
            fastUriChecked = true
            if (!fastUriOk) HostLog.w("sink", "received-file URIs differ from FileProvider's; using FileProvider")
            return slow.toString()
        }
        return if (fastUriOk) fast.toString() else FileProvider.getUriForFile(app, fileProviderAuthority(app), file).toString()
    }

    /**
     * Creates `dir/name`, or `name (1)`, `name (2)`... with `O_EXCL`, so two transfers can
     * never pick the same name and an existing file is never overwritten. Returns the file
     * and its open descriptor, or null when the write is refused (see [refuse]).
     */
    private fun createUnique(dir: File, name: String): Pair<File, FileDescriptor>? {
        if (!ensureDir(dir)) return refuse("could not create folder ${dir.name}", null, permanent = false)
        var n = 0
        var retriedDir = false
        while (true) {
            val file = File(dir, FileNames.numberedName(name, n))
            try {
                // 0644: read and write for the owner, read for others, never execute (SR-23).
                val fd = Os.open(file.path, OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or OsConstants.O_CLOEXEC, FILE_MODE)
                return file to fd
            } catch (e: ErrnoException) {
                when (e.errno) {
                    OsConstants.EEXIST -> n++
                    // The folder went away since it was cached (the user deleted it): once more.
                    OsConstants.ENOENT -> {
                        madeDirs.remove(dir.path)
                        if (retriedDir || !ensureDir(dir)) return refuse("folder ${dir.name} vanished", e, permanent = false)
                        retriedDir = true
                    }
                    // Refused outright: no point trying the next files directly either.
                    OsConstants.EACCES, OsConstants.EPERM, OsConstants.EROFS ->
                        return refuse("direct write refused", e, permanent = true)
                    // Anything else (an odd name, a full disk) concerns this file only.
                    else -> return refuse("direct write of one file failed", e, permanent = false)
                }
            }
            if (n > MAX_NUMBERED) throw PlayDropException.Other("Could not save $name: too many files with that name")
        }
    }

    private fun ensureDir(dir: File): Boolean {
        if (dir.path in madeDirs) return true
        if (!dir.isDirectory && !dir.mkdirs() && !dir.isDirectory) return false
        madeDirs.add(dir.path)
        return true
    }

    /** Logs why a direct write was not possible; [permanent] turns direct writes off for good. */
    private fun refuse(why: String, e: Exception?, permanent: Boolean): Nothing? {
        if (permanent) directRefused = true
        HostLog.w("sink", if (permanent) "$why; saving through MediaStore from now on" else "$why; saving it through MediaStore", e)
        return null
    }

    /** Hands [paths] to the media scanner so they show in Downloads. Asynchronous. */
    private fun scan(transfer: String, paths: List<String>) {
        if (paths.isEmpty()) return
        val t0 = System.nanoTime()
        val left = java.util.concurrent.atomic.AtomicInteger(paths.size)
        MediaScannerConnection.scanFile(app, paths.toTypedArray(), null) { _, _ ->
            if (left.decrementAndGet() == 0) {
                HostLog.d("sink", "transfer ${transfer.take(8)}: scanned ${paths.size} files in ${(System.nanoTime() - t0) / 1_000_000} ms")
            }
        }
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun deliverToMediaStore(item: DeliverItem, name: String, input: InputStream): String {
        val mime = item.mime?.takeIf { it.isNotBlank() && it.contains('/') } ?: guessMime(name)
        val resolver = app.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, FileNames.relativePath(Environment.DIRECTORY_DOWNLOADS, item.path))
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val t = item.transferId
        val uri: Uri = timed(t, "insert") { resolver.insert(collection, values) }
            ?: throw PlayDropException.Other("Could not save $name: the Downloads collection refused a new entry")
        try {
            val out = timed(t, "open") { resolver.openFileDescriptor(uri, "w") }
                ?: throw PlayDropException.Other("Could not save $name: Downloads did not open the new file")
            out.use { opfd ->
                FileOutputStream(opfd.fileDescriptor).use { output ->
                    timed(t, "copy") { FileUtils.copy(input, output) }
                    timed(t, "sync") { output.fd.sync() }
                }
            }
            val done = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
            timed(t, "publish") { resolver.update(uri, done, null, null) }
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
        var n = 0
        var candidate = File(dir, name)
        while (candidate.exists()) {
            n++
            candidate = File(dir, FileNames.numberedName(name, n))
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

        /** Name of the `Download/PlayDrop` root in `res/xml/file_paths.xml`. */
        private const val RECEIVED_ROOT = "received"

        /** `rw-r--r--`. */
        private const val FILE_MODE = 0b110_100_100

        /** Gives up on a name after this many " (n)" tries. */
        private const val MAX_NUMBERED = 10_000

        /** MIME type from the file extension, or `application/octet-stream`. */
        fun guessMime(name: String): String =
            FileNames.extension(name)?.let { MimeTypeMap.getSingleton().getMimeTypeFromExtension(it) }
                ?: "application/octet-stream"

        /** Authority of the app's [FileProvider] (declared in the manifest). */
        fun fileProviderAuthority(context: Context): String = "${context.packageName}.files"
    }
}
