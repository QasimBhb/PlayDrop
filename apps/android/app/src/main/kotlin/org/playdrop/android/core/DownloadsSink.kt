package org.playdrop.android.core

import android.content.ContentProviderOperation
import android.content.ContentUris
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
import java.util.concurrent.ExecutorService
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/** File count and total size of a transfer, for [LandingPolicy]. */
internal data class TransferShape(val fileCount: Long, val totalBytes: Long)

/**
 * Hands verified files to `Download/PlayDrop/<dirs>`. The core stages and verifies each file
 * in its private staging directory first, then calls [deliver] with an open descriptor; the
 * core deletes the staging copy afterwards (a staging file this sink renamed away is simply
 * gone by then).
 *
 * Three paths, by Android version:
 * - Android 11 and later (API 30): plain file operations on the public `Download/PlayDrop`
 *   tree. Scoped storage lets an app create and write its own files there without any
 *   permission. Each file is one create, one kernel copy and one close (about 13 ms on the
 *   Galaxy A54, nearly all of it MediaProvider adding the database row behind the create)
 *   instead of the MediaStore insert, open, fsync and `IS_PENDING` update (about 35 ms). The
 *   staging file is never renamed into Downloads: measured on the A54 (2026-10-15),
 *   `rename(2)` from the app's own folder into `Download` succeeds, but MediaProvider serves
 *   it as a copy behind the scenes that is slower than the app's own copy (8.0 s against
 *   4.1 s for 1 GiB, 29 ms against 14 ms per small file) and not atomic: a process killed in
 *   the middle of one left the file both in place and at the target. See
 *   `docs/release/throughput.md` section 8. The written files are handed to the media scanner in
 *   batches ([ScanBatcher]) so they show in Downloads, and the returned location is a
 *   [FileProvider] URI. If the platform refuses a direct write the sink falls back to the
 *   MediaStore path for good.
 * - Android 10 (API 29): `MediaStore.Downloads` with `IS_PENDING`, since direct writes to
 *   shared storage are not allowed there.
 * - Android 8 and 9 (API 26 to 28) have no `MediaStore.Downloads`; there files go to the
 *   app's own external `Download/PlayDrop` folder and are shared through [FileProvider],
 *   which needs no storage permission. They are not visible in the system Downloads app.
 *
 * Landing (Android 11 and later only): the 13 ms insert is paid per file whatever its size,
 * so a flood of small files is bound by it (5,000 files of 4 to 64 KiB arrived at 1.9 MiB/s
 * against 39.8 MiB/s for 100 medium files). A transfer that [LandingPolicy] calls a bulk drop
 * is delivered into `getExternalFilesDir("landing")/<transfer id>/<dirs>/<name>` instead
 * (about 0.2 ms per file, no MediaProvider row), so the transfer runs at engine speed. The
 * staging file is renamed there (same folder tree, a real rename, 0.1 ms per file measured;
 * the staging path comes from `readlink` on the delivered descriptor and must lie inside the
 * staging folder) and copied if the rename fails; `EXDEV`, `EACCES` or `EPERM` turn the
 * rename off for the rest of the process ([RenameGate]), any other errno concerns that file
 * only. Each landed file is
 * promised its final name in Downloads when it lands ([NameClaims]), and [deliver] returns
 * the `content://` URI of that final place. When the transfer completes, [LandingMover]
 * moves the tree into `Download/PlayDrop` with [moveLandedBatch]: 100 MediaStore rows are
 * inserted at once and the files copied into them on [MOVE_THREADS] threads, journaled
 * ([LandedMove.moveBatch], about 5 ms a file on the A54 against 10 to 14 ms for one create
 * after another, `MoverProbe` in `androidTest`); a file whose promised name was
 * taken meanwhile goes through [moveLanded], the journaled copy through the same primitive
 * as direct delivery. The landing tree mirrors the
 * Downloads tree, so a process killed half way leaves nothing a fresh start cannot finish:
 * [rebuildClaims] re-promises every landed name once per process before the core first
 * delivers, and the mover sweeps what is left. Files a pass could not move are retried within
 * the process ([MoveRetry]) and when storage is mounted again. If the mover has to give a file
 * another name than the one promised (the name was taken meanwhile), the location the core
 * holds is stale: that is logged, and the transfer's "received" notification then opens the
 * Downloads folder instead of a single file. A transfer that fails or is cancelled has its landed files moved the same
 * way: each was verified against its root before it landed, and the direct path keeps what
 * it delivered when a later file fails or the user cancels (the core then only removes its
 * staging `.part` files), so landing keeps them too. A transfer that only lost its
 * connection keeps its landing folder until the core resumes and completes it.
 *
 * Name selection: every path that picks a name in a Downloads folder does it under that
 * folder's lock in [NameClaims], so two writes of this process never pick the same name, and
 * none takes a name promised to a landed file. The " (n)" numbering is [FileNames.numberedName] everywhere.
 *
 * Received files are never executable by construction (SR-23): shared storage is mounted
 * `noexec`, files are created without an execute bit, and the notification's "open" action
 * for risky types (APKs, scripts) shows the Downloads list instead of launching the file.
 * Nothing is written outside `Download/PlayDrop` and the app's own folders: every folder and
 * name goes through the same [FileNames] rules (SR-17), on top of the core's own path checks,
 * and a landed path is checked again ([LandingPaths.parse]) before it is moved.
 *
 * One instance lives for the whole process (see [CoreHostImpl]), across core restarts, so the
 * name claims and the single mover are never duplicated.
 */
internal class DownloadsSink(
    context: Context,
    stagingDir: File,
    /** Looks up a transfer's file count and size when no offer event carried them. */
    private val shapeOf: (String) -> TransferShape?,
    /**
     * Name of the landing area: the `getExternalFilesDir` folder and the prefix of the journal
     * and relocation files. Only the device tests change it, so the app's own mover (which may
     * run in the same process) never sees their files.
     */
    private val area: String = LANDING_DIR,
    /** Called when the relocation table gained or lost entries, so screens showing locations refresh. */
    private val onRelocationsChanged: () -> Unit = {},
) : ReceiveSink {
    private val app = context.applicationContext
    private val timings = SinkTimings()
    private val scans = ScanBatcher()
    private val claims = NameClaims()

    /** `<staging>/`, the only place a staging file may be renamed from. */
    private val stagingPrefix = stagingDir.absolutePath.trimEnd('/') + "/"

    private val stagingToLanding = RenameGate("from staging into the landing folder")

    /**
     * Where renamed landed files really are, on internal storage; see [Relocations]. The core's
     * stored location of such a file is stale, so every reader goes through [relocated].
     */
    private val relocations = Relocations(File(app.filesDir, "$area-relocations.txt"))

    /**
     * Set when a move changed [relocations]; [moveEnded] then flushes the table and tells the
     * screens once, so a renamed 5,000-file drop re-derives the transfer rows once, not 5,000 times.
     */
    private val relocationsDirty = java.util.concurrent.atomic.AtomicBoolean(false)

    /** Journal of the mover's copies in progress, on internal storage. */
    private val journal = MoveJournal(File(app.filesDir, "$area-move.journal"))

    /**
     * Copies a batch of landed files into their MediaStore rows side by side. Idle threads
     * end after 30 s, so the pool costs nothing between moves.
     */
    private val movePool: ExecutorService =
        ThreadPoolExecutor(MOVE_THREADS, MOVE_THREADS, 30, TimeUnit.SECONDS, LinkedBlockingQueue()) { r ->
            Thread(r, "pd-move").apply { isDaemon = true }
        }.apply { allowCoreThreadTimeOut(true) }

    /** Folders already created on the direct path, so each costs one `mkdirs` per process. */
    private val madeDirs: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** Landing folders already created; dropped again when a rename finds one gone. */
    private val madeLandingDirs: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** Per transfer: true when it lands first. Decided once, on the offer or the first file. */
    private val landingChoice = ConcurrentHashMap<String, Boolean>()

    /** Transfers that landed at least one file in this process and have not been moved yet. */
    private val landedTransfers: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** Set once the platform refuses a direct write; every later file uses MediaStore. */
    @Volatile
    private var directRefused = false

    @Volatile
    private var fastUriChecked = false

    @Volatile
    private var fastUriOk = false

    /** Moves the verified file to its final place (or the landing folder) and returns its `content://` URI. */
    override fun deliver(item: DeliverItem, fd: Int): String {
        val start = System.nanoTime()
        // Adopt first so the descriptor is closed on every path, including failures below.
        ParcelFileDescriptor.adoptFd(fd).use { pfd ->
            val name = FileNames.receivedName(item.path)
            val input = pfd.fileDescriptor
            return try {
                val location = when {
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !directRefused ->
                        (if (landsFirst(item.transferId)) deliverToLanding(item, name, input, fd) else null)
                            ?: deliverDirect(item, name, input)
                            ?: deliverToMediaStore(item.transferId, item.path, name, mimeOf(item, name), rewound(input))
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q ->
                        deliverToMediaStore(item.transferId, item.path, name, mimeOf(item, name), rewound(input))
                    else -> deliverToAppFolder(item, name, rewound(input))
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
     * On the direct path an empty manifest folder is created like any other (also for landing
     * transfers: an empty folder has no file to wait for). MediaStore keeps no empty folders
     * (a folder appears when its first file is inserted with that `RELATIVE_PATH`), so on the
     * other paths this is a deliberate no-op.
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

    /** Records the shape of an offered transfer, so its first file needs no lookup. */
    fun noteOffer(transferId: String, fileCount: Long, totalBytes: Long) {
        landingChoice.putIfAbsent(transferId, choose(transferId, TransferShape(fileCount, totalBytes)))
    }

    /** True when [transferId] landed files in this process that the mover has not taken yet. */
    fun hasLanded(transferId: String): Boolean = transferId in landedTransfers

    /**
     * Called when a transfer completes or fails: hands the last written files to the media
     * scanner and logs what the sink cost (debug level, one line per transfer).
     */
    fun transferEnded(transferId: String) {
        landingChoice.remove(transferId)
        scan(transferId, scans.drain(transferId))
        timings.finish(transferId)?.let { HostLog.d("sink", "transfer ${transferId.take(8)}: ${it.line()}") }
    }

    // ---- Landing area, used by LandingMover -------------------------------------------

    /** `getExternalFilesDir("landing")`, or null when shared storage is not available. */
    fun landingRoot(): File? = app.getExternalFilesDir(area)

    /** The landing folder of [transferId] (whether or not it exists). */
    fun landingDir(transferId: String): File? = landingRoot()?.let { File(it, transferId) }

    /**
     * Re-promises the Downloads name of every file an earlier process left in the landing
     * area, so nothing delivered from now on can take it. Called once per process, when the
     * sink is made and before the host first delivers: later in the process the claims in
     * memory are already right, and rebuilding while a mover pass runs could put back a name
     * the pass just released. Returns the transfers that still have files.
     */
    fun rebuildClaims(): List<String> {
        val tids = landedTransferIds()
        val root = landingRoot() ?: return tids
        for (tid in tids) {
            for (f in LandingTree.files(File(root, tid))) {
                val landed = LandingTree.relative(root, f)?.let(LandingPaths::parse) ?: continue
                claims.reserve(downloadsFile(landed).path, f.path)
            }
        }
        if (tids.isNotEmpty()) HostLog.i("landing", "${claims.size} landed files of ${tids.size} transfers wait to be moved")
        return tids
    }

    /** Transfers that have at least one file in the landing area. */
    fun landedTransferIds(): List<String> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return emptyList()
        val root = landingRoot() ?: return emptyList()
        return root.listFiles()
            ?.filter { it.isDirectory && LandingPaths.isTransferId(it.name) && LandingTree.files(it).isNotEmpty() }
            ?.map { it.name }
            .orEmpty()
    }

    /**
     * Settles the copies a stopped pass or a killed process left in the [MoveJournal]
     * ([JournalRecovery]): removes each copy in Downloads (a file, a MediaStore row, or this
     * app's rows carrying a batch tag) whose landed file is still there, so the move redoes it.
     * The mover calls this from its serial queue before every pass, so no copy is running
     * meanwhile. Returns false, keeping the journal, when it cannot be settled now (storage not
     * available, or a copy could not be removed); the pass must then wait, since its next copy
     * would overwrite the entries.
     */
    fun recoverJournal(): Boolean {
        if (journal.readAll().isEmpty()) {
            journal.clear()
            return true
        }
        val root = landingRoot()
        if (root == null || Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) {
            HostLog.i("landing", "storage is not available; unfinished copies are settled later")
            return false
        }
        val outcome = JournalRecovery(
            journal, remover, root.path, receiveRoot().path,
            isRowUri = { Uri.parse(it).authority == MediaStore.AUTHORITY },
        ).settle()
        if (outcome.ignored > 0) HostLog.w("landing", "ignored ${outcome.ignored} move journal entries that point outside the landing and Downloads folders")
        if (outcome.removed > 0) HostLog.i("landing", "removed ${outcome.removed} unfinished copies left by a stopped move; they are moved again")
        if (!outcome.settled) HostLog.w("landing", "could not remove an unfinished copy in Downloads; moving waits")
        return outcome.settled
    }

    /** Removes unfinished copies for [recoverJournal] and the batch half. */
    private val remover = object : CopyRemover {
        /**
         * Deletes the row, and for a published row of this app whose file is exactly its place
         * ([RowUnlink]) unlinks that file through the file system first. A delete by URI alone
         * removes the file behind the FUSE layer's back, and FUSE keeps answering "exists" for
         * that name for a moment (seen on the A54): the move redone at once would then find its
         * promised name taken and number it. A pending row (a MediaStore delivery) keeps its
         * file at a hidden `.pending-...` path while its place may be another, finished file of
         * the same name, so it is deleted by URI only, as is any row whose file is elsewhere.
         */
        override fun removeRow(uri: String, maxSize: Long?): Boolean = runCatching {
            val u = Uri.parse(uri)
            @Suppress("DEPRECATION") // DATA is still the only column that says where the file is.
            val projection = arrayOf(
                MediaStore.MediaColumns.RELATIVE_PATH, MediaStore.MediaColumns.DISPLAY_NAME, MediaStore.MediaColumns.IS_PENDING,
                MediaStore.MediaColumns.OWNER_PACKAGE_NAME, MediaStore.MediaColumns.DATA,
            )
            var leave = false
            val unlink = app.contentResolver.query(u, projection, null, null, null)?.use { c ->
                if (!c.moveToFirst()) return@use null
                val data = c.getString(4)
                if (maxSize != null && data != null && File(data).length() > maxSize) {
                    leave = true
                    return@use null
                }
                val place = placeFile(c.getString(0), c.getString(1))
                place.takeIf { RowUnlink.throughFileSystem(c.getInt(2) != 0, c.getString(3), app.packageName, data, it?.path) }
            }
            if (leave) {
                HostLog.w("landing", "a Downloads entry holds a bigger file than PlayDrop wrote there; it is left alone")
                return@runCatching true
            }
            unlink?.delete()
            app.contentResolver.delete(u, null, null)
            true
        }.onFailure { HostLog.w("landing", "could not remove an unfinished copy from Downloads", it) }.getOrDefault(false)

        /**
         * Only rows this app owns (`OWNER_PACKAGE_NAME`) that carry [tag] in `TITLE` match. A tag
         * is made for one move alone ([MoveJournal.tag]), so nothing else can be removed, not
         * even a finished file of the same name. (`DOWNLOAD_URI` would be the natural column,
         * but MediaProvider refuses it to apps: "Invalid column download_uri" on the A54.)
         */
        override fun removeTagged(tag: String, maxSize: Long?): Boolean {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return true
            val where = "${MediaStore.MediaColumns.OWNER_PACKAGE_NAME} = ? AND ${MediaStore.MediaColumns.TITLE} = ?"
            return runCatching {
                @Suppress("DEPRECATION")
                val found = app.contentResolver.query(
                    downloadsCollection(), arrayOf(MediaStore.MediaColumns._ID, MediaStore.MediaColumns.DATA), where, arrayOf(app.packageName, tag), null,
                )?.use { c -> buildList { while (c.moveToNext()) add(c.getLong(0) to c.getString(1)) } }.orEmpty()
                for ((id, data) in found) {
                    if (maxSize != null && data != null && File(data).length() > maxSize) {
                        HostLog.w("landing", "a Downloads entry holds a bigger file than PlayDrop wrote there; it is left alone")
                        continue
                    }
                    app.contentResolver.delete(ContentUris.withAppendedId(downloadsCollection(), id), null, null)
                }
                true
            }.onFailure { HostLog.w("landing", "could not remove an unfinished copy from Downloads", it) }.getOrDefault(false)
        }

        override fun removeFile(path: String): Boolean = File(path).let { it.delete() || !it.exists() }
    }

    /**
     * The real location of a delivered file: [location] as the core stored it, unless the
     * mover gave the file another name (see [Relocations]). Cheap; safe on any thread.
     */
    fun relocated(transferId: String, location: String): String = relocations.resolve(transferId, location)

    /** True when [location] of [transferId] was moved to another name. */
    fun isRelocated(transferId: String, location: String): Boolean = relocations.lookup(transferId, location) != null

    /** Forgets the relocations of transfers the core no longer lists ([live] are the IDs it does). */
    fun pruneRelocations(live: Set<String>) {
        if (relocations.prune(live) > 0) onRelocationsChanged()
    }

    /** Every file in the landing folder of [transferId]. */
    fun landedFiles(transferId: String): List<File> = landingDir(transferId)?.let(LandingTree::files).orEmpty()

    /**
     * Moves one landed file of [transferId] into `Download/PlayDrop`: a copy and delete, or
     * MediaStore when direct writes are refused; both copies are journaled.
     * Blocking. A file that fails stays landed, with its name still promised, for a retry;
     * [MoveOutcome.GONE] when it is no longer there. See [MoveResult] for the flags.
     */
    fun moveLanded(transferId: String, file: File): MoveResult = landedMove.move(transferId, file)

    /**
     * Moves [files] of [transferId] into `Download/PlayDrop` as one batch ([LandedMove.moveBatch]):
     * one result per file handled, cut short after a [MoveResult.stopPass]. Blocking.
     */
    fun moveLandedBatch(transferId: String, files: List<File>): List<MoveResult> = landedMove.moveBatch(transferId, files)

    /** Called by the mover before each pass ([LandedMove.beginPass]). */
    fun beginMovePass() = landedMove.beginPass()

    /**
     * The platform half of [moveLanded] ([LandedMove] holds the order of the steps and exists so
     * that order runs in JVM tests without a ContentResolver): the direct copy, or MediaStore
     * when direct writes are refused, both journaled.
     */
    private val placer = DownloadsPlacer { landed, file, avoidRows ->
        val key = moveKey(landed.transferId)
        var input: ParcelFileDescriptor? = null
        val openInput: () -> FileDescriptor = {
            (input ?: ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).also { input = it }).fileDescriptor
        }
        var mediaStoreUri: String? = null
        try {
            val placed = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !directRefused) {
                placeInDownloads(key, landed.dirs, landed.name, file.path, file.length(), openInput, journalFrom = file.path, avoidRows = avoidRows)
            } else {
                null
            }
            if (placed == null) {
                // Direct writes were refused (now or earlier): MediaStore picks the name. The
                // row is journaled before it is written and published, like a direct copy.
                mediaStoreUri = deliverToMediaStore(key, landed.downloadsRel, landed.name, guessMime(landed.name), rewound(openInput()), owner = file.path) { uri ->
                    journal.write(MoveJournal.Entry(file.path, uri.toString()))
                }
            }
            input?.close()
            input = null
            // MediaStore may also rename on a clash; its URI is new either way, so count it.
            val renumbered = placed == null || placed.name != landed.name
            val promisedUri = receivedUri(downloadsFile(landed), landed.dirs)
            val actual = placed?.let { receivedUri(it, landed.dirs) } ?: mediaStoreUri
            Placement(promisedUri, actual, renumbered)
        } finally {
            runCatching { input?.close() }
        }
    }

    /**
     * The batch half of [moveLandedBatch]: MediaStore rows inserted 100 at a time, then each
     * row's file opened and written. Measured on the A54 (2026-10-28, 1,000 files of 4 to 64
     * KiB, `MoverProbe` in `androidTest`): 2 ms a file for the insert and 5 ms in all with the
     * copies on 4 threads, against 10 to 14 ms for one `O_EXCL` create after another. What does
     * not work: creates on several threads (MediaProvider serialises them, 13 to 16 ms), and
     * rows inserted pending and published afterwards (the publish renames each file, 10 ms).
     * A published row has no file until it is opened for writing; see [BatchPlacer.reserve]
     * for what that shows meanwhile.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private inner class MediaStoreBatch : BatchPlacer, CopyRemover by remover {
        override fun relativePath(landed: LandingPaths.Landed): String =
            (listOf(Environment.DIRECTORY_DOWNLOADS, FileNames.RECEIVE_FOLDER) + landed.dirs).joinToString("/", postfix = "/")

        override fun promisedUri(landed: LandingPaths.Landed): String = receivedUri(downloadsFile(landed), landed.dirs)

        override fun placesWithRows(items: List<BatchItem>): Set<Int> {
            val key = moveKey(items.first().landed.transferId)
            val asked = items.groupBy({ relativePath(it.landed) }, { it.landed.name }).mapValues { it.value.toSet() }
            val held = timed(key, "lookup") { rowsAt(asked) }
            return items.indices.filter { placeKey(relativePath(items[it].landed), items[it].landed.name) in held }.toSet()
        }

        override fun reserve(items: List<BatchItem>, tags: List<String>): List<Row> {
            if (Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) throw java.io.IOException("shared storage is not mounted")
            val key = moveKey(items.first().landed.transferId)
            val resolver = app.contentResolver
            val collection = downloadsCollection()
            val ops = items.mapIndexed { k, it ->
                ContentProviderOperation.newInsert(collection).withValues(
                    ContentValues().apply {
                        put(MediaStore.Downloads.DISPLAY_NAME, it.landed.name)
                        put(MediaStore.Downloads.MIME_TYPE, guessMime(it.landed.name))
                        put(MediaStore.Downloads.RELATIVE_PATH, relativePath(it.landed))
                        put(MediaStore.MediaColumns.TITLE, tags[k])
                    },
                ).build()
            }
            val uris = timed(key, "insert") { resolver.applyBatch(MediaStore.AUTHORITY, ArrayList(ops)) }.map {
                it.uri ?: throw java.io.IOException("the Downloads collection refused a new entry")
            }
            // What each row became: MediaProvider cleans names and folders and numbers a name
            // whose file exists. The caller removes every row that is not exactly as asked.
            val ids = uris.map(ContentUris::parseId)
            data class Placed(val rel: String, val name: String, val title: String?)
            val placed = HashMap<Long, Placed>()
            timed(key, "readback") {
                resolver.query(
                    collection,
                    arrayOf(MediaStore.MediaColumns._ID, MediaStore.MediaColumns.RELATIVE_PATH, MediaStore.MediaColumns.DISPLAY_NAME, MediaStore.MediaColumns.TITLE),
                    "${MediaStore.MediaColumns._ID} IN (${ids.joinToString(",")})", null, null,
                )?.use { c -> while (c.moveToNext()) placed[c.getLong(0)] = Placed(c.getString(1).orEmpty(), c.getString(2).orEmpty(), c.getString(3)) }
            }
            // No second count of rows per place here: places that had a row went the one-file
            // way before the insert ([placesWithRows]), so a shared place can only come from
            // another writer inserting in the same moment. Deleting our row then could unlink
            // their file once they write it; leaving it is safer, and the copy's size check
            // ([PlaceTakenException]) refuses to write over a file that is not empty. The count
            // cost a whole-table LIKE scan (about 45 ms) per batch on the A54.
            return items.mapIndexed { k, item ->
                val p = placed[ids[k]] ?: Placed("", "", null)
                Row(uris[k].toString(), p.rel, p.name, p.title, shared = false)
            }
        }

        override fun fill(item: BatchItem, row: Row) {
            val key = moveKey(item.landed.transferId)
            // "rw", not "w": "w" truncates, and between the insert and this open another writer
            // may have put a file at this place (another app through MediaStore). A file that is
            // not empty is theirs: leave it. Seen on the A54 (`MoverProbe.caseVariant`,
            // 2026-10-28): a direct create of `case.bin` beside the unwritten row `Case.bin`
            // fails with EEXIST, so the file API cannot take the place; "rw" creates the row's
            // file at size 0 like "w".
            val out = timed(key, "open") { app.contentResolver.openFileDescriptor(Uri.parse(row.ref), "rw") }
                ?: throw java.io.IOException("Downloads did not open a new file")
            out.use { o ->
                val existing = Os.fstat(o.fileDescriptor).st_size
                if (existing != 0L) throw PlaceTakenException("the place of a Downloads entry already holds $existing bytes")
                ParcelFileDescriptor.open(item.file, ParcelFileDescriptor.MODE_READ_ONLY).use { input ->
                    timed(key, "copy") { FileUtils.copy(input.fileDescriptor, o.fileDescriptor) }
                    if (ReceivePolicy.syncAfterCopy(item.file.length())) timed(key, "sync") { Os.fdatasync(o.fileDescriptor) }
                }
                // Closed here so an error the FUSE layer reports at close (a full disk) fails the
                // copy; the descriptor is then invalid and use() only tells the provider it closed.
                timed(key, "close") { Os.close(o.fileDescriptor) }
            }
            // No media scan here: MediaProvider scans a file written through its own descriptor
            // when that descriptor closes (the tag in TITLE is replaced within half a second, seen
            // on the A54). A second scan of every file kept MediaProvider busy long after the
            // move and slowed the next receive's landing to twice its time (Phase 10 bench).
        }

        override fun discard(row: Row, written: Boolean): Boolean {
            if (written) return removeRow(row.ref)
            // Nothing was written to it: no file system unlink first. MediaProvider's delete
            // still removes whatever file sits at the row's DATA path, so this relies on the
            // place holding no one else's file: places with rows went the one-file way before
            // the insert, and a row sharing its place can only come from an insert race in
            // the moment between the insert and this discard.
            return runCatching { app.contentResolver.delete(Uri.parse(row.ref), null, null); true }
                .onFailure { HostLog.w("landing", "could not remove a Downloads entry", it) }
                .getOrDefault(false)
        }
    }

    // Must stay declared after placer, relocations, journal and claims: it captures them at construction.
    private val landedMove = LandedMove(
        landingRoot = ::landingRoot,
        downloadsFile = ::downloadsFile,
        placer = placer,
        relocations = relocations,
        journal = journal,
        claims = claims,
        onRelocationsChanged = { relocationsDirty.set(true) },
        info = { HostLog.i("landing", it) },
        warn = { message, error -> HostLog.w("landing", message, error) },
        batchPlacer = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) MediaStoreBatch() else null,
        fillPool = movePool,
        settleJournal = ::recoverJournal,
    )

    /** Called by the mover after a pass: flushes the scans and logs the cost of the pass. */
    fun moveEnded(transferId: String) {
        if (relocationsDirty.getAndSet(false)) {
            relocations.sync()
            onRelocationsChanged()
        }
        val key = moveKey(transferId)
        scan(transferId, scans.drain(key))
        timings.finish(key)?.let { HostLog.d("landing", "transfer ${transferId.take(8)} move: ${it.line()}") }
        if (landedFiles(transferId).isEmpty()) landedTransfers.remove(transferId)
    }

    /** Removes the empty folders of [transferId]'s landing tree, the tree itself included when empty. */
    fun pruneLanding(transferId: String) {
        val dir = landingDir(transferId) ?: return
        LandingTree.pruneEmpty(dir)
        madeLandingDirs.removeIf { it == dir.path || it.startsWith(dir.path + "/") }
    }

    // ---- Delivery paths ---------------------------------------------------------------

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

    private fun downloadsFile(landed: LandingPaths.Landed): File = File(receiveRoot(), landed.downloadsRel.joinToString("/"))

    private fun moveKey(transferId: String) = "move $transferId"

    private fun mimeOf(item: DeliverItem, name: String): String =
        item.mime?.takeIf { it.isNotBlank() && it.contains('/') } ?: guessMime(name)

    /** True when [transferId] lands first; decided on the first call and kept for the transfer. */
    private fun landsFirst(transferId: String): Boolean = landingChoice.computeIfAbsent(transferId) { id ->
        val shape = try {
            shapeOf(id)
        } catch (e: Exception) {
            HostLog.w("sink", "looking up the size of transfer ${id.take(8)} failed; delivering straight to Downloads", e)
            null
        }
        shape != null && choose(id, shape)
    }

    private fun choose(transferId: String, shape: TransferShape): Boolean {
        val land = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && LandingPolicy.shouldLand(shape.fileCount, shape.totalBytes)
        if (land) HostLog.d("sink", "transfer ${transferId.take(8)}: ${shape.fileCount} files, ${shape.totalBytes} bytes; landing in the app folder first")
        return land
    }

    /**
     * The path of the staging file behind descriptor [fd], from `/proc/self/fd`, or null when
     * it cannot be read or does not lie inside the staging folder (then nothing is renamed).
     */
    private fun stagingPathOf(fd: Int): String? {
        val p = try {
            Os.readlink("/proc/self/fd/$fd")
        } catch (e: ErrnoException) {
            HostLog.d("sink", "readlink of a delivered file failed (${OsConstants.errnoName(e.errno)}); copying it")
            return null
        }
        val ok = p.startsWith(stagingPrefix) && !p.endsWith(" (deleted)") && "/../" !in p && "/./" !in p
        if (!ok) HostLog.d("sink", "a delivered file is not where staging files live; copying it")
        return p.takeIf { ok }
    }

    /**
     * Places the file in `Download/PlayDrop/<dirs>`: create and copy.
     * Returns null, having read nothing from the input, when the platform refuses the write,
     * so the caller can use MediaStore instead.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun deliverDirect(item: DeliverItem, name: String, input: FileDescriptor): String? {
        val t = item.transferId
        val dirs = FileNames.receivedDirs(item.path)
        val placed = placeInDownloads(t, dirs, name, null, item.size.toLong(), { input }, journalFrom = null) ?: return null
        return timed(t, "uri") { receivedUri(placed, dirs) }
    }

    private class Claim(val file: File, val out: FileDescriptor)

    /**
     * The one primitive behind direct delivery and the mover. Under the folder's lock it picks
     * the first free numbered name that is not promised to anyone but [owner] and creates the
     * file there with `O_EXCL`; the copy runs outside the lock. With [journalFrom] (the mover:
     * the landed file's path) the copy is recorded in the [MoveJournal] between the create
     * and the copy. Returns the file, or null when direct writes are refused (see [refuse]).
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun placeInDownloads(
        t: String,
        dirs: List<String>,
        name: String,
        owner: String?,
        size: Long,
        input: () -> FileDescriptor,
        journalFrom: String?,
        avoidRows: Boolean = false,
    ): File? {
        if (Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) return null
        val dir = File(receiveRoot(), dirs.joinToString("/"))
        val claim = claims.inFolder(dir.path) { claimInFolder(t, dir, name, owner, avoidRows) } ?: return null
        val file = claim.file
        val out = claim.out
        try {
            if (journalFrom != null) timed(t, "journal") { journal.write(MoveJournal.Entry(journalFrom, file.path)) }
            timed(t, "copy") { copyFromStart(input(), out) }
            if (ReceivePolicy.syncAfterCopy(size)) timed(t, "sync") { Os.fdatasync(out) }
        } catch (e: Exception) {
            runCatching { Os.close(out) }
            if (!file.delete()) HostLog.w("sink", "removing a half-written file failed")
            if (journalFrom != null) journal.clear()
            throw e
        }
        timed(t, "close") { Os.close(out) }
        scans.add(t, file.path)?.let { scan(t, it) }
        return file
    }

    /**
     * Picks the name and creates the file; called holding the folder lock. The create uses
     * `O_EXCL`, so an existing file is never overwritten.
     */
    private fun claimInFolder(t: String, dir: File, name: String, owner: String?, avoidRows: Boolean = false): Claim? {
        if (!ensureDir(dir)) return refuse("could not create folder ${dir.name}", null, permanent = false)
        var n = 0
        var retriedDir = false
        while (n <= MAX_NUMBERED) {
            val file = File(dir, FileNames.numberedName(name, n))
            // With avoidRows (a batch found another row at this file's place): a row that has no
            // file yet is invisible to the create, so ask MediaStore as well.
            if (claims.heldByOther(file.path, owner) || (avoidRows && hasRow(dir, file.name))) {
                n++
                continue
            }
            try {
                // 0644: read and write for the owner, read for others, never execute (SR-23).
                val fd = timed(t, "create") {
                    Os.open(file.path, OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or OsConstants.O_CLOEXEC, FILE_MODE)
                }
                return Claim(file, fd)
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
        }
        throw PlayDropException.Other("Could not save $name: too many files with that name")
    }

    /**
     * Delivers into the landing folder and returns the URI of the file's promised place in
     * Downloads. Returns null (nothing promised, nothing written) when landing is not possible,
     * so the caller delivers straight to Downloads.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun deliverToLanding(item: DeliverItem, name: String, input: FileDescriptor, fd: Int): String? {
        val t = item.transferId
        if (Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) return null
        val root = landingRoot() ?: return null
        val dirs = FileNames.receivedDirs(item.path)
        val downloadsDir = File(receiveRoot(), dirs.joinToString("/"))
        val landingDir = File(root, LandingPaths.landingRel(t, dirs, name).dropLast(1).joinToString("/"))
        // Promise a Downloads name that exists nowhere and is promised to nobody else.
        val promised = timed(t, "claim") {
            claims.inFolder(downloadsDir.path) {
                FileNames.firstFree(name, MAX_NUMBERED) { cand ->
                    val target = File(downloadsDir, cand).path
                    claims.heldByOther(target, null) || File(target).exists()
                }?.also { cand -> claims.reserve(File(downloadsDir, cand).path, File(landingDir, cand).path) }
            }
        } ?: throw PlayDropException.Other("Could not save $name: too many files with that name")
        val target = File(downloadsDir, promised)
        val landed = File(landingDir, promised)
        try {
            land(t, item.size.toLong(), input, fd, landingDir, landed)
        } catch (e: Exception) {
            claims.release(target.path, landed.path)
            HostLog.w("sink", "landing a file failed; writing it to Downloads directly", e)
            return null
        }
        landedTransfers.add(t)
        return timed(t, "uri") { receivedUri(target, dirs) }
    }

    /** Renames the staging file to [file] in [dir], else copies it there. Throws on failure. */
    private fun land(t: String, size: Long, input: FileDescriptor, fd: Int, dir: File, file: File) {
        val src = if (stagingToLanding.open) stagingPathOf(fd) else null
        if (src != null) {
            for (attempt in 0..1) {
                ensureLandingDir(dir)
                try {
                    timed(t, "land-rename") { Os.rename(src, file.path) }
                    if (stagingToLanding.succeeded()) HostLog.i("sink", "rename ${stagingToLanding.route} works")
                    return
                } catch (e: ErrnoException) {
                    // The mover may have pruned the folder a moment ago: recreate it once.
                    if (e.errno == OsConstants.ENOENT && attempt == 0) {
                        madeLandingDirs.remove(dir.path)
                        continue
                    }
                    renameFailed(stagingToLanding, e)
                    break
                }
            }
        }
        var out: FileDescriptor? = null
        for (attempt in 0..1) {
            ensureLandingDir(dir)
            try {
                out = timed(t, "land-create") {
                    Os.open(file.path, OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or OsConstants.O_CLOEXEC, FILE_MODE)
                }
                break
            } catch (e: ErrnoException) {
                if (e.errno == OsConstants.ENOENT && attempt == 0) {
                    madeLandingDirs.remove(dir.path)
                    continue
                }
                throw e
            }
        }
        val o = out ?: throw IllegalStateException("landing folder ${dir.name} keeps vanishing")
        try {
            timed(t, "land-copy") { copyFromStart(input, o) }
            if (ReceivePolicy.syncAfterCopy(size)) timed(t, "sync") { Os.fdatasync(o) }
        } catch (e: Exception) {
            runCatching { Os.close(o) }
            file.delete()
            throw e
        }
        Os.close(o)
    }

    private fun ensureLandingDir(dir: File) {
        if (dir.path in madeLandingDirs) return
        if (!dir.isDirectory && !dir.mkdirs() && !dir.isDirectory) throw java.io.IOException("could not create landing folder ${dir.name}")
        madeLandingDirs.add(dir.path)
    }

    private fun renameFailed(gate: RenameGate, e: ErrnoException) {
        val (decision, closedNow) = gate.failed(errnoKind(e.errno))
        val why = OsConstants.errnoName(e.errno) ?: e.errno.toString()
        when {
            closedNow -> HostLog.i("sink", "rename ${gate.route} refused ($why) after ${gate.renamed} renames; copying from now on")
            decision == RenameDecision.COPY_THIS_FILE -> HostLog.d("sink", "rename ${gate.route} failed ($why); copying this file")
        }
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

    /**
     * Inserts into `MediaStore.Downloads` under `Download/PlayDrop/<dirs of path>`. [path] is
     * the manifest path (its last component, the name, is left out of the folder).
     */
    @RequiresApi(Build.VERSION_CODES.Q)
    private fun deliverToMediaStore(
        t: String,
        path: List<String>,
        name: String,
        mime: String,
        input: InputStream,
        /** The landed file whose promised name this may take (the mover), or null. */
        owner: String? = null,
        afterInsert: ((Uri) -> Unit)? = null,
    ): String {
        val resolver = app.contentResolver
        val dir = File(receiveRoot(), FileNames.receivedDirs(path).joinToString("/"))
        // Like claimInFolder: under the folder's lock, a name that exists nowhere and is not
        // promised to anyone else, held for this write until the row is published.
        val token = "mediastore:${java.util.UUID.randomUUID()}"
        val (chosen, uri) = claims.inFolder(dir.path) {
            val pick = FileNames.firstFree(name, MAX_NUMBERED) { cand ->
                val target = File(dir, cand).path
                claims.heldByOther(target, owner) || File(target).exists()
            } ?: throw PlayDropException.Other("Could not save $name: too many files with that name")
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, pick)
                put(MediaStore.Downloads.MIME_TYPE, mime)
                put(MediaStore.Downloads.RELATIVE_PATH, FileNames.relativePath(Environment.DIRECTORY_DOWNLOADS, path))
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val inserted: Uri = timed(t, "insert") { resolver.insert(downloadsCollection(), values) }
                ?: throw PlayDropException.Other("Could not save $name: the Downloads collection refused a new entry")
            claims.reserve(File(dir, pick).path, token)
            pick to inserted
        }
        try {
            afterInsert?.invoke(uri)
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
        } finally {
            claims.release(File(dir, chosen).path, token)
        }
        return uri.toString()
    }

    /**
     * The file of a row at [rel] (`Download/...`, ending in `/`) named [name], or null when the
     * row does not lie under `Download/PlayDrop` with clean components (nothing else is touched).
     */
    private fun placeFile(rel: String?, name: String?): File? {
        if (rel == null || name == null || !FileNames.isCleanName(name)) return null
        val parts = rel.trimEnd('/').split('/')
        if (parts.size < 2 || parts[0] != Environment.DIRECTORY_DOWNLOADS || parts[1] != FileNames.RECEIVE_FOLDER) return null
        if (!parts.drop(2).all(FileNames::isCleanName)) return null
        return File(receiveRoot(), (parts.drop(2) + name).joinToString("/"))
    }

    /** True when MediaStore has a row (written or not) for [name] in [dir] under `Download/PlayDrop`, whatever its case. */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun hasRow(dir: File, name: String): Boolean {
        val rel = dir.relativeTo(receiveRoot()).path.let { sub ->
            (listOf(Environment.DIRECTORY_DOWNLOADS, FileNames.RECEIVE_FOLDER) + sub.split('/').filter { it.isNotEmpty() }).joinToString("/", postfix = "/")
        }
        return rowsAt(mapOf(rel to setOf(name))).isNotEmpty()
    }

    /** The key [rowsAt] counts a place under: shared storage treats names that differ only in case as one ([CaseFold]). */
    private fun placeKey(rel: String, name: String): String = CaseFold.key(rel + name)

    /**
     * How many rows hold each of the places in [places] (relative path to names), counted by
     * [placeKey]; places with no row are left out. One query of `LIKE` patterns (MediaProvider
     * takes `LIKE ... ESCAPE`, where a `COLLATE` clause may be refused): `LIKE` ignores ASCII
     * case only, so every character outside ASCII becomes the one-character wildcard `_`, and
     * `\`, `%` and `_` in names are escaped. Every row returned is then compared exactly by
     * [placeKey], so the wildcards only widen the search.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun rowsAt(places: Map<String, Set<String>>): Map<String, Int> {
        val asked = places.flatMap { (rel, names) -> names.map { placeKey(rel, it) } }.toSet()
        if (asked.isEmpty()) return emptyMap()
        val args = ArrayList<String>()
        val like = "LIKE ? ESCAPE '\\'"
        val where = places.entries.joinToString(" OR ") { (rel, names) ->
            args += MediaStoreNames.likePattern(rel)
            names.forEach { args += MediaStoreNames.likePattern(it) }
            "(${MediaStore.MediaColumns.RELATIVE_PATH} $like AND (" +
                names.joinToString(" OR ") { "${MediaStore.MediaColumns.DISPLAY_NAME} $like" } + "))"
        }
        val seen = HashMap<String, Int>()
        app.contentResolver.query(
            downloadsCollection(), arrayOf(MediaStore.MediaColumns.RELATIVE_PATH, MediaStore.MediaColumns.DISPLAY_NAME),
            where, args.toTypedArray(), null,
        )?.use { c ->
            while (c.moveToNext()) {
                val k = placeKey(c.getString(0).orEmpty(), c.getString(1).orEmpty())
                if (k in asked) seen.merge(k, 1, Int::plus)
            }
        }
        return seen
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun downloadsCollection(): Uri = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)

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

        /** `getExternalFilesDir` type of the landing area. */
        private const val LANDING_DIR = "landing"

        /** `rw-r--r--`. */
        private const val FILE_MODE = 0b110_100_100

        /**
         * Threads that copy a batch of landed files into their rows. On the A54 4 threads took
         * the batch from 8.3 to 5.0 ms a file; 8 gained under 10% more, so the mover leaves
         * MediaProvider room for a receive running at the same time.
         */
        const val MOVE_THREADS = 4

        /** Gives up on a name after this many " (n)" tries. */
        private const val MAX_NUMBERED = 10_000

        /** MIME type from the file extension, or `application/octet-stream`. */
        fun guessMime(name: String): String =
            FileNames.extension(name)?.let { MimeTypeMap.getSingleton().getMimeTypeFromExtension(it) }
                ?: "application/octet-stream"

        /** Authority of the app's [FileProvider] (declared in the manifest). */
        fun fileProviderAuthority(context: Context): String = "${context.packageName}.files"

        /** Maps a platform errno onto the kinds [RenameGate] decides on. */
        private fun errnoKind(errno: Int): ErrnoKind = when (errno) {
            OsConstants.EXDEV -> ErrnoKind.CROSS_DEVICE
            OsConstants.EACCES -> ErrnoKind.ACCESS
            OsConstants.EPERM -> ErrnoKind.PERMISSION
            OsConstants.ENOENT -> ErrnoKind.NOT_FOUND
            else -> ErrnoKind.OTHER
        }

        /** Copies [input] from its start: an earlier attempt may have moved its offset. */
        private fun copyFromStart(input: FileDescriptor, out: FileDescriptor) {
            Os.lseek(input, 0, OsConstants.SEEK_SET)
            FileUtils.copy(input, out)
        }

        /** A stream over [fd] from its start, for the paths that copy through a stream. */
        private fun rewound(fd: FileDescriptor): InputStream {
            Os.lseek(fd, 0, OsConstants.SEEK_SET)
            return FileInputStream(fd)
        }
    }
}
