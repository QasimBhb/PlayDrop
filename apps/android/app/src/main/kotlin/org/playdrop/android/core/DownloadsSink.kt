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
 *   permission. The sink first tries to `rename(2)` the staging file into place: staging
 *   (`getExternalFilesDir("staging")`) and Downloads are on the same shared-storage volume,
 *   so if MediaProvider's FUSE layer allows it the file is moved with no copy at all, which
 *   is what a big file needs (a 1 GiB copy took 4.7 s of a 24.7 s receive). The staging path
 *   comes from `readlink` on the delivered descriptor and must lie inside the staging folder.
 *   If the rename fails the file is copied instead; `EXDEV`, `EACCES` or `EPERM` mean the
 *   platform refuses this route, so rename is not tried again in this process ([RenameGate]),
 *   any other errno concerns that file only. The copy is one create, one kernel copy and one
 *   close per file (about 13 ms on the Galaxy A54, nearly all of it MediaProvider adding the
 *   database row behind the create) instead of the MediaStore insert, open, fsync and
 *   `IS_PENDING` update (about 35 ms). The written files are handed to the media scanner in
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
 * (about 0.2 ms per file, no MediaProvider row), so the transfer runs at engine speed; the
 * same rename-first-then-copy rule applies from staging to there. Each landed file is
 * promised its final name in Downloads when it lands ([NameClaims]), and [deliver] returns
 * the `content://` URI of that final place. When the transfer completes, [LandingMover]
 * moves the tree into `Download/PlayDrop` with [moveLanded], which uses the same
 * rename-first-then-copy primitive as direct delivery. The landing tree mirrors the
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
 * folder's lock in [NameClaims], so a rename (which would replace an existing file silently)
 * never races another write of this process, and never takes a name promised to a landed
 * file. The " (n)" numbering is [FileNames.numberedName] everywhere.
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
) : ReceiveSink {
    private val app = context.applicationContext
    private val timings = SinkTimings()
    private val scans = ScanBatcher()
    private val claims = NameClaims()

    /** `<staging>/`, the only place a staging file may be renamed from. */
    private val stagingPrefix = stagingDir.absolutePath.trimEnd('/') + "/"

    private val stagingToDownloads = RenameGate("from staging into Downloads")
    private val stagingToLanding = RenameGate("from staging into the landing folder")
    private val landingToDownloads = RenameGate("from the landing folder into Downloads")

    /** Journal of the mover's copy in progress, on internal storage. */
    private val journal = MoveJournal(File(app.filesDir, "landing-move.journal"))

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
                            ?: deliverDirect(item, name, input, fd)
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
    fun landingRoot(): File? = app.getExternalFilesDir(LANDING_DIR)

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
     * Settles the copy a stopped pass or a killed process left in the [MoveJournal]: removes
     * the copy in Downloads (a file or a MediaStore row) when the landed file is still there,
     * so the move redoes it. The mover calls this from its serial queue before every pass, so
     * no copy is running meanwhile. Returns false, keeping the entry, when it cannot be settled
     * now (storage not available, or the copy could not be removed); the pass must then wait,
     * since its next copy would overwrite the entry.
     */
    fun recoverJournal(): Boolean {
        val e = journal.read() ?: return true
        val root = landingRoot()
        if (root == null || Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) {
            HostLog.i("landing", "storage is not available; an unfinished copy is settled later")
            return false
        }
        val landingOk = e.landing.startsWith(root.path + "/")
        val targetOk = if (e.targetIsUri) {
            Uri.parse(e.target).authority == MediaStore.AUTHORITY
        } else {
            e.target.startsWith(receiveRoot().path + "/")
        }
        if (!landingOk || !targetOk) {
            HostLog.w("landing", "ignored a move journal that points outside the landing and Downloads folders")
            journal.clear()
            return true
        }
        val target = File(e.target)
        val decision = MoveJournal.recovery(File(e.landing).exists(), e.targetIsUri || target.exists())
        if (decision == MoveJournal.Recovery.DELETE_TARGET) {
            val removed = if (e.targetIsUri) {
                runCatching { app.contentResolver.delete(Uri.parse(e.target), null, null); true }
                    .onFailure { HostLog.w("landing", "could not remove an unfinished copy from Downloads", it) }
                    .getOrDefault(false)
            } else {
                target.delete() || !target.exists()
            }
            if (!removed) {
                HostLog.w("landing", "could not remove an unfinished copy in Downloads; moving waits")
                return false
            }
            HostLog.i("landing", "removed an unfinished copy left by a stopped move; it is moved again")
        }
        journal.clear()
        return true
    }

    /** Every file in the landing folder of [transferId]. */
    fun landedFiles(transferId: String): List<File> = landingDir(transferId)?.let(LandingTree::files).orEmpty()

    /**
     * Moves one landed file of [transferId] into `Download/PlayDrop`: rename first, else copy
     * and delete, else MediaStore when direct writes are refused; both copies are journaled.
     * Blocking. A file that fails stays landed, with its name still promised, for a retry;
     * [MoveOutcome.GONE] when it is no longer there. See [MoveResult] for the flags.
     */
    fun moveLanded(transferId: String, file: File): MoveResult {
        val root = landingRoot() ?: return MoveResult(MoveOutcome.FAILED)
        val landed = LandingTree.relative(root, file)?.let(LandingPaths::parse)
        if (landed == null || landed.transferId != transferId) {
            HostLog.w("landing", "a file in the landing folder has a path PlayDrop would not write; left in place")
            return MoveResult(MoveOutcome.FAILED)
        }
        val target = downloadsFile(landed)
        if (!file.exists()) {
            claims.release(target.path, file.path)
            return MoveResult(MoveOutcome.GONE)
        }
        val key = moveKey(transferId)
        var input: ParcelFileDescriptor? = null
        val openInput: () -> FileDescriptor = {
            (input ?: ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).also { input = it }).fileDescriptor
        }
        try {
            val placed = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !directRefused) {
                placeInDownloads(key, landed.dirs, landed.name, file.path, landingToDownloads, file.path, file.length(), openInput, journaled = true)
            } else {
                null
            }
            if (placed == null) {
                // Direct writes were refused (now or earlier): MediaStore picks the name. The
                // row is journaled before it is written and published, like a direct copy.
                deliverToMediaStore(key, landed.downloadsRel, landed.name, guessMime(landed.name), rewound(openInput())) { uri ->
                    journal.write(MoveJournal.Entry(file.path, uri.toString()))
                }
            }
            if (placed == null || !placed.renamed) {
                input?.close()
                input = null
                // Delete before forgetting the journal. If the delete fails the entry stays and
                // the pass stops, so the next pass removes this copy and moves the file again
                // instead of leaving it twice.
                if (!file.delete() && file.exists()) {
                    HostLog.w("landing", "a copied file could not be removed from the landing folder; the move is redone later")
                    return MoveResult(MoveOutcome.FAILED, stopPass = true)
                }
                journal.clear()
            }
            claims.release(target.path, file.path)
            // MediaStore may also rename on a clash; its URI is new either way, so count it.
            val renumbered = placed == null || placed.file.name != landed.name
            if (renumbered) {
                HostLog.i("landing", "transfer ${transferId.take(8)}: a landed file reached Downloads under another name than promised")
            }
            val outcome = if (placed != null && placed.renamed) MoveOutcome.RENAMED else MoveOutcome.COPIED
            return MoveResult(outcome, renumbered)
        } catch (e: Exception) {
            // A journal entry left by a failed copy points at a copy that was already removed
            // (or a pending MediaStore row); settling it before the next pass is harmless.
            HostLog.w("landing", "moving a landed file into Downloads failed; it stays in the landing folder", e)
            return MoveResult(MoveOutcome.FAILED)
        } finally {
            runCatching { input?.close() }
        }
    }

    /** Called by the mover after a pass: flushes the scans and logs the cost of the pass. */
    fun moveEnded(transferId: String) {
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
     * Places the file in `Download/PlayDrop/<dirs>`: rename first, else create and copy.
     * Returns null, having read nothing from the input, when the platform refuses the write,
     * so the caller can use MediaStore instead.
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun deliverDirect(item: DeliverItem, name: String, input: FileDescriptor, fd: Int): String? {
        val t = item.transferId
        val dirs = FileNames.receivedDirs(item.path)
        val src = if (stagingToDownloads.open) stagingPathOf(fd) else null
        val placed = placeInDownloads(t, dirs, name, src, stagingToDownloads, null, item.size.toLong(), { input }, journaled = false)
            ?: return null
        return timed(t, "uri") { receivedUri(placed.file, dirs) }
    }

    private class Placed(val file: File, val renamed: Boolean)

    private class Claim(val file: File, val out: FileDescriptor?)

    /**
     * The one primitive behind direct delivery and the mover. Under the folder's lock it picks
     * the first free numbered name that is not promised to anyone but [owner], then renames
     * [src] there if [src] is given and [gate] is open, else creates the file with `O_EXCL`.
     * The copy runs outside the lock. With [journaled], the copy is recorded in the
     * [MoveJournal] between the create and the copy. Returns null when direct writes are
     * refused (see [refuse]).
     */
    @RequiresApi(Build.VERSION_CODES.R)
    private fun placeInDownloads(
        t: String,
        dirs: List<String>,
        name: String,
        src: String?,
        gate: RenameGate,
        owner: String?,
        size: Long,
        input: () -> FileDescriptor,
        journaled: Boolean,
    ): Placed? {
        if (Environment.getExternalStorageState() != Environment.MEDIA_MOUNTED) return null
        val dir = File(receiveRoot(), dirs.joinToString("/"))
        val claim = claims.inFolder(dir.path) { claimInFolder(t, dir, name, src, gate, owner) } ?: return null
        val file = claim.file
        val out = claim.out
        if (out == null) {
            scans.add(t, file.path)?.let { scan(t, it) }
            return Placed(file, renamed = true)
        }
        try {
            if (journaled && src != null) timed(t, "journal") { journal.write(MoveJournal.Entry(src, file.path)) }
            timed(t, "copy") { copyFromStart(input(), out) }
            if (ReceivePolicy.syncAfterCopy(size)) timed(t, "sync") { Os.fdatasync(out) }
        } catch (e: Exception) {
            runCatching { Os.close(out) }
            if (!file.delete()) HostLog.w("sink", "removing a half-written file failed")
            if (journaled) journal.clear()
            throw e
        }
        timed(t, "close") { Os.close(out) }
        scans.add(t, file.path)?.let { scan(t, it) }
        return Placed(file, renamed = false)
    }

    /**
     * Picks the name and renames or creates the file; called holding the folder lock. A
     * rename is only tried on a name that does not exist (it would replace it); a create uses
     * `O_EXCL`, so an existing file is never overwritten either way.
     */
    private fun claimInFolder(t: String, dir: File, name: String, src: String?, gate: RenameGate, owner: String?): Claim? {
        if (!ensureDir(dir)) return refuse("could not create folder ${dir.name}", null, permanent = false)
        var n = 0
        var retriedDir = false
        var tryRename = src != null
        while (n <= MAX_NUMBERED) {
            val file = File(dir, FileNames.numberedName(name, n))
            if (claims.heldByOther(file.path, owner)) {
                n++
                continue
            }
            if (tryRename && gate.open && src != null) {
                if (timed(t, "stat") { file.exists() }) {
                    n++
                    continue
                }
                try {
                    timed(t, "rename") { Os.rename(src, file.path) }
                    if (gate.succeeded()) HostLog.i("sink", "rename ${gate.route} works; files are moved without a copy")
                    return Claim(file, null)
                } catch (e: ErrnoException) {
                    tryRename = false
                    renameFailed(gate, e)
                    // Fall through: create this same name and copy instead.
                }
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
        afterInsert: ((Uri) -> Unit)? = null,
    ): String {
        val resolver = app.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, FileNames.relativePath(Environment.DIRECTORY_DOWNLOADS, path))
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri: Uri = timed(t, "insert") { resolver.insert(collection, values) }
            ?: throw PlayDropException.Other("Could not save $name: the Downloads collection refused a new entry")
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

        /** `getExternalFilesDir` type of the landing area. */
        private const val LANDING_DIR = "landing"

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
