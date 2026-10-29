package org.playdrop.android.core

import android.content.ContentProviderOperation
import android.content.ContentValues
import android.net.Uri
import android.os.Environment
import android.os.FileUtils
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.system.Os
import android.system.OsConstants
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.FileDescriptor
import java.io.FileOutputStream
import java.util.concurrent.Callable
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import kotlin.random.Random

/**
 * Timing probe behind the mover's design (Phase 10, 2026-10-28): moves [count] files of 4 to
 * 64 KiB from the app's own external folder into `Download/PlayDrop/probe-<n>/` in several
 * ways, interleaved over [rounds], and logs ms per file and the median of each way under the
 * tag `MoverProbe`. Runs in the app's process, so MediaProvider sees the app as the owner, as
 * it does for the real mover; a debug APK is fine because the cost is in MediaProvider's process.
 *
 * Ways: A one `O_EXCL` create after another with a one-entry fsync'd journal (the mover until
 * Phase 10), B the same without the journal, C2/C4/C8 creates on several threads, E1/E4/E8
 * creates in batches of 100 with one journal write per batch, D1/D4 MediaStore rows inserted
 * pending in batches and published in batches, G1/G4/G8 published rows inserted in batches
 * then each row's file opened and written (G4 is what [DownloadsSink] does now), H4 and I4
 * the G4 way with batches of 50 and 250. A54 medians: A 10 to 22, C 13 to 16, E 16 to 17,
 * D4 14, G4 5.0, G8 4.6, I4 4.7 ms a file. `collision` shows how MediaStore names a row whose
 * name is taken.
 *
 * Skipped unless asked for, since a run takes minutes: `adb shell am instrument -w -e probe true
 * -e class org.playdrop.android.core.MoverProbe -e files 1000 -e rounds 3 [-e variants A,G4]
 * org.playdrop.android.test/androidx.test.runner.AndroidJUnitRunner`. Every probe file is
 * removed afterwards (and what a failed run left, on the next run).
 */
@RunWith(AndroidJUnit4::class)
class MoverProbe {
    private val ctx = InstrumentationRegistry.getInstrumentation().targetContext
    private val args = InstrumentationRegistry.getArguments()
    private val count = args.getString("files")?.toInt() ?: 1000
    private val rounds = args.getString("rounds")?.toInt() ?: 3
    private val only = args.getString("variants")?.split(',')?.toSet()
    private val journalFile = File(ctx.filesDir, "probe-move.journal")
    private val downloads = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "PlayDrop")
    private val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
    private val phases = ConcurrentHashMap<String, AtomicLong>()

    @Before
    fun onlyWhenAsked() {
        assumeTrue("a timing probe; run it with -e probe true", args.getString("probe") == "true")
    }

    private inline fun <T> timed(phase: String, block: () -> T): T {
        val t0 = System.nanoTime()
        try {
            return block()
        } finally {
            phases.computeIfAbsent(phase) { AtomicLong() }.addAndGet(System.nanoTime() - t0)
        }
    }

    private class Run(val name: String, val body: (List<File>, File) -> List<Uri>)

    /** What an insert does when its name is taken: by a direct file, and by another (empty) row. */
    @Test
    fun collision() {
        clearOldProbes()
        val resolver = ctx.contentResolver
        val dir = File(downloads, "probe-collision").apply { mkdirs() }
        val rel = "${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/${dir.name}/"
        File(dir, "a.txt").writeText("someone")
        fun insert(name: String): Uri = resolver.insert(
            collection,
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, "text/plain")
                put(MediaStore.Downloads.RELATIVE_PATH, rel)
            },
        )!!
        fun nameOf(u: Uri): String? = resolver.query(u, arrayOf(MediaStore.MediaColumns.DISPLAY_NAME, MediaStore.MediaColumns.OWNER_PACKAGE_NAME, MediaStore.MediaColumns.SIZE), null, null, null)?.use {
            if (it.moveToFirst()) "${it.getString(0)} owner=${it.getString(1)} size=${it.getLong(2)}" else null
        }
        val u1 = insert("a.txt")
        Log.i(TAG, "collision with a file: ${nameOf(u1)}, a.txt still '${File(dir, "a.txt").readText()}'")
        val u2 = insert("b.txt")
        val u3 = insert("b.txt")
        Log.i(TAG, "collision with an unwritten row: ${nameOf(u2)} / ${nameOf(u3)}; b.txt exists=${File(dir, "b.txt").exists()}")
        resolver.openFileDescriptor(u2, "w")!!.use { FileOutputStream(it.fileDescriptor).write("hello".toByteArray()) }
        Log.i(TAG, "after writing u2: ${nameOf(u2)} file=${File(dir, "b.txt").length()}")
        val q = resolver.query(collection, arrayOf(MediaStore.MediaColumns._ID, MediaStore.MediaColumns.DISPLAY_NAME), "${MediaStore.MediaColumns.RELATIVE_PATH} = ? AND ${MediaStore.MediaColumns.DISPLAY_NAME} = ? AND ${MediaStore.MediaColumns.OWNER_PACKAGE_NAME} = ?", arrayOf(rel, "b.txt", ctx.packageName), null)?.use { it.count }
        Log.i(TAG, "query by path, name and owner found $q rows")
        // A row whose file is missing: what does a direct create of that name do?
        val u4 = insert("c.txt")
        val r = runCatching { Os.close(create(File(dir, "c.txt"))) }
        Log.i(TAG, "direct create over an unwritten row: $r; ${nameOf(u4)}")
        listOf(u1, u2, u3, u4).forEach { resolver.delete(it, null, null) }
        dir.listFiles()?.forEach { it.delete() }
        Log.i(TAG, "cleanup left ${dir.list()?.toList()}; dir deleted=${dir.delete()}")
    }

    /**
     * How MediaProvider cleans the name and folders of an inserted row, and whether this app
     * may tag a row through `DOWNLOAD_URI` and delete by that tag.
     */
    @Test
    fun cleaning() {
        clearOldProbes()
        val resolver = ctx.contentResolver
        val base = "${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/probe-clean"
        val names = listOf(
            ".gitignore", "..x", "a:b.txt", "q?.txt", "s*.txt", "lt<gt>.txt", "pipe|.txt", "quote\".txt", "back\\slash.txt",
            "trail.", "trail..", "trail ", " lead", "sp ace.txt", "tab\tx", "emoji\uD83D\uDE00.txt", "CON", "nul.txt",
            "x".repeat(200) + ".txt", "\u00e9".repeat(130) + ".txt", "noext", "a.jpeg", "a.JPG", "a.weird", "a.txt.", "~tilde",
            "a#b.txt", "a%b.txt", "a&b.txt", "a'b.txt", "a;b.txt", "a=b.txt", "a@b.txt", "a[b].txt", "a{b}.txt", "a+b.txt", "a,b.txt",
        )
        val folders = listOf("plain", ".git", "dir.", "dir ", " lead", "a:b", "d\uD83D\uDE00", "x/.hidden", "_ok")
        fun row(rel: String, name: String, tag: String?): String {
            val uri = runCatching {
                resolver.insert(
                    collection,
                    ContentValues().apply {
                        put(MediaStore.Downloads.DISPLAY_NAME, name)
                        put(MediaStore.Downloads.MIME_TYPE, DownloadsSink.guessMime(name))
                        put(MediaStore.Downloads.RELATIVE_PATH, rel)
                        if (tag != null) put(MediaStore.MediaColumns.TITLE, tag)
                    },
                )
            }.getOrElse { return "insert threw ${it.javaClass.simpleName}: ${it.message}" } ?: return "insert returned null"
            return resolver.query(uri, arrayOf(MediaStore.MediaColumns.RELATIVE_PATH, MediaStore.MediaColumns.DISPLAY_NAME, MediaStore.MediaColumns.TITLE), null, null, null)!!.use {
                it.moveToFirst()
                "rel='${it.getString(0)}' name='${it.getString(1)}' tag='${it.getString(2)}'"
            }
        }
        for (n in names) Log.i(TAG, "CLEAN name '$n' (mime ${DownloadsSink.guessMime(n)}) -> ${row("$base/", n, null)}")
        for (f in folders) Log.i(TAG, "CLEAN folder '$f' -> ${row("$base/$f/", "f.txt", null)}")
        val direct = File(downloads, "probe-clean/direct").apply { mkdirs() }
        for (n in names + listOf("plain.txt")) {
            val r = runCatching { Os.close(create(File(direct, n))) }
            Log.i(TAG, "CLEAN direct create '$n' -> ${r.exceptionOrNull()?.message ?: "ok"}; listed as ${direct.list()?.filter { it.length > 2 && n.startsWith(it.take(2)) }}")
        }
        Log.i(TAG, "CLEAN tagged -> ${row("$base/", "tagged.txt", "playdrop-move:1234:7")}")
        // Does writing the file (and the scan after it) keep the tag?
        val written = resolver.insert(
            collection,
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, "written.txt")
                put(MediaStore.Downloads.MIME_TYPE, "text/plain")
                put(MediaStore.Downloads.RELATIVE_PATH, "$base/")
                put(MediaStore.MediaColumns.TITLE, "playdrop-move:1234:8")
            },
        )!!
        resolver.openFileDescriptor(written, "w")!!.use { FileOutputStream(it.fileDescriptor).write("hello".toByteArray()) }
        for (wait in listOf(0L, 500L, 2000L)) {
            Thread.sleep(wait)
            resolver.query(written, arrayOf(MediaStore.MediaColumns.TITLE, MediaStore.MediaColumns.SIZE), null, null, null)!!.use {
                it.moveToFirst()
                Log.i(TAG, "CLEAN written row after +$wait ms: title='${it.getString(0)}' size=${it.getLong(1)}")
            }
        }
        val del = resolver.delete(
            collection,
            "${MediaStore.MediaColumns.OWNER_PACKAGE_NAME} = ? AND ${MediaStore.MediaColumns.TITLE} = ?",
            arrayOf(ctx.packageName, "playdrop-move:1234:7"),
        )
        Log.i(TAG, "CLEAN delete by tag removed $del rows")
        val n = resolver.delete(collection, "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf("$base/%"))
        Log.i(TAG, "CLEAN cleanup removed $n rows")
        File(downloads, "probe-clean").deleteRecursively()
    }

    /**
     * Case behaviour of a published row that has no file yet (round-3 review, M1): does a
     * direct create of a case variant succeed, does a "rw" open of the row then see that
     * file, and does deleting the row by URI remove it? Also: does "rw" create the file of an
     * unwritten row, as "w" does?
     */
    @Test
    fun caseVariant() {
        clearOldProbes()
        val resolver = ctx.contentResolver
        val dir = File(downloads, "probe-case").apply { mkdirs() }
        val rel = "${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/probe-case/"
        fun insert(name: String) = resolver.insert(
            collection,
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                put(MediaStore.Downloads.RELATIVE_PATH, rel)
            },
        )!!
        val row = insert("Case.bin")
        val created = runCatching { Os.close(create(File(dir, "case.bin"))) }
        runCatching { File(dir, "case.bin").writeText("theirs") }
        Log.i(TAG, "CASE create case.bin beside unwritten row Case.bin: ${created.exceptionOrNull()?.message ?: "ok"}; listed ${dir.list()?.toList()}")
        val size = runCatching { resolver.openFileDescriptor(row, "rw")!!.use { Os.fstat(it.fileDescriptor).st_size } }
        Log.i(TAG, "CASE rw open of the row then sees size ${size.getOrNull()} (${size.exceptionOrNull()?.message})")
        resolver.delete(row, null, null)
        Log.i(TAG, "CASE after deleting row Case.bin by URI: case.bin exists=${File(dir, "case.bin").exists()} text='${runCatching { File(dir, "case.bin").readText() }.getOrNull()}'")
        val fresh = insert("fresh.bin")
        val rw = runCatching { resolver.openFileDescriptor(fresh, "rw")!!.use { Os.fstat(it.fileDescriptor).st_size } }
        Log.i(TAG, "CASE rw open of an unwritten row: size ${rw.getOrNull()} (${rw.exceptionOrNull()?.message}); file exists=${File(dir, "fresh.bin").exists()}")
        resolver.delete(fresh, null, null)
        dir.listFiles()?.forEach { it.delete() }
        resolver.delete(collection, "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf("$rel%"))
        dir.delete()
    }

    @Test
    fun probe() {
        val variants = listOf(
            Run("A-seq-journal") { src, dest -> direct(src, dest, journalEach = true); emptyList() },
            Run("B-seq") { src, dest -> direct(src, dest, journalEach = false); emptyList() },
            Run("C2-par") { src, dest -> parallel(src, 2) { direct(it, dest, journalEach = false) }; emptyList() },
            Run("C4-par") { src, dest -> parallel(src, 4) { direct(it, dest, journalEach = false) }; emptyList() },
            Run("C8-par") { src, dest -> parallel(src, 8) { direct(it, dest, journalEach = false) }; emptyList() },
            Run("E1-batchjournal") { src, dest -> batchedDirect(src, dest, threads = 1); emptyList() },
            Run("E4-batchjournal") { src, dest -> batchedDirect(src, dest, threads = 4); emptyList() },
            Run("E8-batchjournal") { src, dest -> batchedDirect(src, dest, threads = 8); emptyList() },
            Run("D1-mediastore") { src, dest -> mediaStore(src, dest, threads = 1) },
            Run("D4-mediastore") { src, dest -> mediaStore(src, dest, threads = 4) },
            Run("G1-rows") { src, dest -> rowsFirst(src, dest, threads = 1) },
            Run("G4-rows") { src, dest -> rowsFirst(src, dest, threads = 4) },
            Run("G8-rows") { src, dest -> rowsFirst(src, dest, threads = 8) },
            Run("H4-rows50") { src, dest -> rowsFirst(src, dest, threads = 4, batchSize = 50) },
            Run("I4-rows250") { src, dest -> rowsFirst(src, dest, threads = 4, batchSize = 250) },
        ).filter { only == null || it.name.substringBefore('-') in only }
        clearOldProbes()
        val srcRoot = ctx.getExternalFilesDir("probe-landing")!!
        srcRoot.deleteRecursively()
        val results = LinkedHashMap<String, MutableList<Double>>()
        var n = 0
        for (r in 0 until rounds) {
            // Interleave: each round starts at a different variant.
            val order = variants.indices.map { variants[(it + r) % variants.size] }
            for (v in order) {
                n++
                val srcDir = File(srcRoot, "r$r-${v.name}")
                val src = makeSources(srcDir, seed = r * 100 + n)
                val dest = File(downloads, "probe-${System.currentTimeMillis()}-$n")
                phases.clear()
                val t0 = System.nanoTime()
                val uris = v.body(src, dest)
                val ms = (System.nanoTime() - t0) / 1e6
                val perFile = ms / count
                results.getOrPut(v.name) { ArrayList() }.add(perFile)
                val breakdown = phases.entries.sortedBy { it.key }
                    .joinToString(" ") { "${it.key}=%.2f".format(it.value.get() / 1e6 / count) }
                val ok = verify(src, dest, uris)
                Log.i(TAG, "round $r ${v.name}: %.0f ms, %.2f ms/file [%s] verified=$ok".format(ms, perFile, breakdown))
                cleanup(dest, uris)
                srcDir.deleteRecursively()
            }
        }
        srcRoot.deleteRecursively()
        journalFile.delete()
        for ((name, xs) in results) {
            val med = xs.sorted()[xs.size / 2]
            Log.i(TAG, "MEDIAN $name: %.2f ms/file (runs %s)".format(med, xs.joinToString { "%.2f".format(it) }))
        }
    }

    /** Writes [count] files of 4 to 64 KiB; returns them with their sizes kept in [sizes]. */
    private val sizes = HashMap<String, Long>()

    private fun makeSources(dir: File, seed: Int): List<File> {
        dir.mkdirs()
        val rnd = Random(seed)
        val buf = ByteArray(64 * 1024)
        rnd.nextBytes(buf)
        return (0 until count).map { i ->
            val f = File(dir, "m%04d.bin".format(i))
            val len = rnd.nextInt(4 * 1024, 64 * 1024 + 1)
            FileOutputStream(f).use { it.write(buf, 0, len) }
            sizes[f.name] = len.toLong()
            f
        }
    }

    // ---- a, b, c: one create per file, optional one-slot journal -------------------------

    private fun direct(src: List<File>, dest: File, journalEach: Boolean) {
        timed("mkdir") { dest.mkdirs() }
        for (f in src) {
            val target = File(dest, f.name)
            val out = timed("create") { create(target) }
            if (journalEach) timed("journal") { writeJournal("${f.path}\n${target.path}\n") }
            copyAndClose(f, out)
            timed("delete") { f.delete() }
            if (journalEach) timed("journal") { journalFile.delete() }
        }
    }

    private fun create(target: File): FileDescriptor =
        Os.open(target.path, OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or OsConstants.O_CLOEXEC, 0b110_100_100)

    private fun copyAndClose(f: File, out: FileDescriptor) {
        val input = timed("open-src") { ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY) }
        timed("copy") { FileUtils.copy(input.fileDescriptor, out) }
        input.close()
        timed("close") { Os.close(out) }
    }

    private fun writeJournal(text: String) {
        FileOutputStream(journalFile).use { o ->
            o.write(text.toByteArray())
            o.fd.sync()
        }
    }

    private fun <T> parallel(src: List<File>, threads: Int, body: (List<File>) -> T) {
        val pool = Executors.newFixedThreadPool(threads)
        try {
            val parts = (0 until threads).map { t -> src.filterIndexed { i, _ -> i % threads == t } }
            pool.invokeAll(parts.map { p -> Callable { body(p) } }).forEach { it.get() }
        } finally {
            pool.shutdown()
        }
    }

    // ---- e: batches of 100: creates (on threads), one journal fsync, copies (on threads) ---

    private fun batchedDirect(src: List<File>, dest: File, threads: Int) {
        timed("mkdir") { dest.mkdirs() }
        val pool = Executors.newFixedThreadPool(threads)
        try {
            for (batch in src.chunked(BATCH)) {
                val targets = batch.map { File(dest, it.name) }
                val fds = pool.invokeAll(targets.map { t -> Callable { timed("create") { create(t) } } }).map { it.get() }
                timed("journal") { writeJournal(batch.indices.joinToString("") { "${batch[it].path}\t${targets[it].path}\n" }) }
                pool.invokeAll(batch.indices.map { i -> Callable { copyAndClose(batch[i], fds[i]) } }).forEach { it.get() }
                timed("delete") { batch.forEach { it.delete() } }
                timed("journal") { journalFile.delete() }
            }
        } finally {
            pool.shutdown()
        }
    }

    // ---- d: MediaStore rows inserted pending in batches, published in batches -------------

    private fun mediaStore(src: List<File>, dest: File, threads: Int): List<Uri> {
        val resolver = ctx.contentResolver
        val rel = "${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/${dest.name}/"
        val all = ArrayList<Uri>()
        val pool = Executors.newFixedThreadPool(threads)
        try {
            for (batch in src.chunked(BATCH)) {
                val ops = batch.map { f ->
                    ContentProviderOperation.newInsert(collection).withValues(
                        ContentValues().apply {
                            put(MediaStore.Downloads.DISPLAY_NAME, f.name)
                            put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                            put(MediaStore.Downloads.RELATIVE_PATH, rel)
                            put(MediaStore.Downloads.IS_PENDING, 1)
                        },
                    ).build()
                }
                val uris = timed("insert") { resolver.applyBatch(MediaStore.AUTHORITY, ArrayList(ops)) }.map { it.uri!! }
                timed("journal") { writeJournal(batch.indices.joinToString("") { "${batch[it].path}\t${uris[it]}\n" }) }
                pool.invokeAll(
                    batch.indices.map { i ->
                        Callable {
                            val out = timed("open") { resolver.openFileDescriptor(uris[i], "w")!! }
                            val input = timed("open-src") { ParcelFileDescriptor.open(batch[i], ParcelFileDescriptor.MODE_READ_ONLY) }
                            timed("copy") { FileUtils.copy(input.fileDescriptor, out.fileDescriptor) }
                            input.close()
                            timed("close") { out.close() }
                        }
                    },
                ).forEach { it.get() }
                val publish = uris.map { u ->
                    ContentProviderOperation.newUpdate(u).withValue(MediaStore.Downloads.IS_PENDING, 0).build()
                }
                timed("publish") { resolver.applyBatch(MediaStore.AUTHORITY, ArrayList(publish)) }
                timed("delete") { batch.forEach { it.delete() } }
                timed("journal") { journalFile.delete() }
                all.addAll(uris)
            }
        } finally {
            pool.shutdown()
        }
        return all
    }

    // ---- g: published rows inserted in batches, then each row's file opened and written ----
    // (Opening the row's path with O_CREAT instead is refused: FUSE answers EEXIST for a path
    // that has a row but no file yet, and a published row has no file until it is written.)

    private fun rowsFirst(src: List<File>, dest: File, threads: Int, batchSize: Int = BATCH): List<Uri> {
        val resolver = ctx.contentResolver
        val rel = "${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/${dest.name}/"
        val all = ArrayList<Uri>()
        val pool = Executors.newFixedThreadPool(threads)
        try {
            for (batch in src.chunked(batchSize)) {
                val ops = batch.map { f ->
                    ContentProviderOperation.newInsert(collection).withValues(
                        ContentValues().apply {
                            put(MediaStore.Downloads.DISPLAY_NAME, f.name)
                            put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                            put(MediaStore.Downloads.RELATIVE_PATH, rel)
                        },
                    ).build()
                }
                val uris = timed("insert") { resolver.applyBatch(MediaStore.AUTHORITY, ArrayList(ops)) }.map { it.uri!! }
                timed("journal") { writeJournal(batch.indices.joinToString("") { "${batch[it].path}\t${uris[it]}\n" }) }
                pool.invokeAll(
                    batch.indices.map { i ->
                        Callable {
                            val out = timed("open") { resolver.openFileDescriptor(uris[i], "w")!! }
                            val input = timed("open-src") { ParcelFileDescriptor.open(batch[i], ParcelFileDescriptor.MODE_READ_ONLY) }
                            timed("copy") { FileUtils.copy(input.fileDescriptor, out.fileDescriptor) }
                            input.close()
                            timed("close") { out.close() }
                        }
                    },
                ).forEach { it.get() }
                timed("delete") { batch.forEach { it.delete() } }
                timed("journal") { journalFile.delete() }
                all.addAll(uris)
            }
        } finally {
            pool.shutdown()
        }
        return all
    }

    // ---- check and clean up ---------------------------------------------------------------

    /** Removes what an earlier, failed probe run left: its rows (and files) and folders. */
    private fun clearOldProbes() {
        val n = ctx.contentResolver.delete(collection, "${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf("${Environment.DIRECTORY_DOWNLOADS}/PlayDrop/probe-%"))
        downloads.listFiles { f -> f.isDirectory && f.name.startsWith("probe-") }?.forEach { d ->
            d.listFiles()?.forEach { it.delete() }
            d.delete()
        }
        Log.i(TAG, "cleared $n rows of earlier probe runs")
    }

    private fun verify(src: List<File>, dest: File, uris: List<Uri>): Boolean {
        val names = dest.list()?.toSet().orEmpty()
        if (names.size != src.size) {
            Log.w(TAG, "expected ${src.size} files in ${dest.name}, found ${names.size}")
            return false
        }
        return src.all { File(dest, it.name).length() == sizes[it.name] } && src.none { it.exists() } &&
            (uris.isEmpty() || uris.size == src.size)
    }

    private fun cleanup(dest: File, uris: List<Uri>) {
        if (uris.isNotEmpty()) {
            uris.chunked(BATCH).forEach { b ->
                ctx.contentResolver.applyBatch(MediaStore.AUTHORITY, ArrayList(b.map { ContentProviderOperation.newDelete(it).build() }))
            }
        } else {
            dest.listFiles()?.forEach { it.delete() }
        }
        if (!dest.delete()) Log.w(TAG, "could not remove ${dest.path}: ${dest.list()?.size} left")
    }

    private companion object {
        const val TAG = "MoverProbe"
        const val BATCH = 100
    }
}
