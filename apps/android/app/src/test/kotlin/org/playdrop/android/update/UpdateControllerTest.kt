package org.playdrop.android.update

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.playdrop.core.UpdateAsset
import org.playdrop.core.UpdateOffer
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.net.UnknownHostException
import java.security.MessageDigest

/**
 * The update flow with fakes: when a check runs, what it shows, "Later", and the download,
 * verify and hand-over steps with the file deleted on every path.
 */
class UpdateControllerTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val day = 24 * 60 * 60 * 1000L
    private val hour = 60 * 60 * 1000L
    private var now = 100 * day
    private val apk = ByteArray(300_000) { (it * 7).toByte() }
    private val apkUrl = "https://github.com/QasimBhb/PlayDrop/releases/download/v0.12.0/playdrop-0.12.0-android.apk"
    private val asset = UpdateAsset("playdrop-0.12.0-android.apk", apkUrl, hex(sha256(apk)))
    private val offer = UpdateOffer("0.12.0", "https://github.com/QasimBhb/PlayDrop/releases/tag/v0.12.0", asset)

    private val net = FakeTransport().apply {
        replies[SUMS] = FakeTransport.Reply(body = "sums".toByteArray())
        replies[SIG] = FakeTransport.Reply(body = "sig".toByteArray())
        replies[apkUrl] = FakeTransport.Reply(body = apk)
    }
    private val store = MemoryStore()
    private var version = "0.11.0"
    private var decision: () -> UpdateOffer? = { offer }
    private val decided = mutableListOf<String>()
    private val installer = FakeInstaller()
    private lateinit var files: TempFiles
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
    private val controller by lazy {
        files = TempFiles(tmp.newFolder("cache"))
        val endpoints = UpdateEndpoints(SUMS, SIG, ::githubOnly, null)
        val fetcher = UpdateFetcher(net, TEST_LIMITS, endpoints::redirectAllowed)
        val decider = OfferDecider { _, _, version ->
            decided += version
            decision()
        }
        UpdateController(
            scope = scope,
            store = store,
            rules = DayRules,
            channel = { UpdateChannel(endpoints, decider, fetcher, TEST_LIMITS) },
            files = files,
            installer = installer,
            currentVersion = version,
            now = { now },
            io = Dispatchers.Unconfined,
        )
    }

    @After
    fun tearDown() = scope.cancel()

    private val checkRequests get() = net.requests.count { it.url == SUMS || it.url == SIG }

    // When a check runs

    @Test
    fun withTheSwitchOffNothingLeavesThePhone() {
        store.record = store.record.copy(enabled = false)
        controller.onForeground()
        controller.checkNow()
        assertEquals(0, net.requests.size)
        assertEquals(UpdateState.Idle, controller.state.value)
    }

    @Test
    fun aBuildThatIsNotAPlainReleaseNeverAsks() {
        version = "0.11.0-debug"
        controller.onForeground()
        controller.checkNow()
        assertEquals(0, net.requests.size)
        assertTrue(decided.isEmpty())
        assertEquals(false, controller.checksAvailable)
        assertEquals(UpdateState.Idle, controller.state.value)
    }

    @Test
    fun aDueCheckAsksExactlyTwoQuestionsAndRecordsTheAnswer() {
        controller.onForeground()
        assertEquals(listOf(SUMS, SIG), net.requests.map { it.url })
        assertEquals(listOf("0.11.0"), decided)
        assertEquals(now, store.record.lastAnswerMs)
        assertEquals(UpdateState.Available(offer), controller.state.value)
    }

    @Test
    fun atMostOnceADayButCheckNowSkipsTheDay() {
        store.record = store.record.copy(lastAnswerMs = now - hour)
        controller.onForeground()
        assertEquals(0, checkRequests)
        controller.checkNow()
        assertEquals(2, checkRequests)
    }

    @Test
    fun laterHidesTheOfferUntilADayAfterIt() {
        store.record = store.record.copy(lastAnswerMs = now - 2 * day)
        controller.onForeground()
        controller.later()
        assertEquals(UpdateState.Idle, controller.state.value)
        assertEquals("0.12.0", store.record.laterVersion)
        assertEquals(now, store.record.laterAtMs)
        now += 23 * hour
        // A check that got its answer two days ago would be due; "Later" starts a new day.
        store.record = store.record.copy(lastAnswerMs = now - 2 * day)
        controller.onForeground()
        assertEquals(2, checkRequests)
        now += 2 * hour
        controller.onForeground()
        assertEquals(4, checkRequests)
        assertEquals(UpdateState.Available(offer), controller.state.value)
    }

    @Test
    fun theScheduleCountsLaterAsTheStartOfADay() {
        val r = UpdateRecord(true, lastAnswerMs = now - 2 * day, lastFailureMs = null, laterVersion = "0.12.0", laterAtMs = now - hour)
        assertEquals(false, UpdateSchedule.due(r, now, DayRules))
        assertEquals(true, UpdateSchedule.due(r.copy(laterAtMs = now - day), now, DayRules))
        assertEquals(true, UpdateSchedule.due(r.copy(laterAtMs = null), now, DayRules))
    }

    // What a check shows

    @Test
    fun nothingNewerIsSilentUnlessCheckNowAsked() {
        decision = { null }
        controller.onForeground()
        assertEquals(UpdateState.Idle, controller.state.value)
        controller.checkNow()
        assertEquals(UpdateState.UpToDate, controller.state.value)
    }

    @Test
    fun aFailedAutomaticCheckIsSilentAndRetriedAfterAnHour() {
        net.replies[SUMS] = FakeTransport.Reply(error = UnknownHostException("github.com"))
        controller.onForeground()
        assertEquals(UpdateState.Idle, controller.state.value)
        assertEquals(now, store.record.lastFailureMs)
        assertNull(store.record.lastAnswerMs)
        assertEquals(1, net.requests.size)
        now += 30 * 60 * 1000
        controller.onForeground()
        assertEquals(1, net.requests.size)
        now += hour
        controller.onForeground()
        assertEquals(2, net.requests.size)
    }

    @Test
    fun aFailedCheckNowSaysWhy() {
        net.replies[SUMS] = FakeTransport.Reply(status = 500)
        controller.checkNow()
        assertEquals(UpdateState.Failed(UpdateFailure.SERVER_ERROR, null), controller.state.value)
        controller.dismiss()
        net.replies[SUMS] = FakeTransport.Reply(body = "sums".toByteArray())
        decision = { throw UpdateFetchException(UpdateFailure.NOT_VERIFIED) }
        controller.checkNow()
        assertEquals(UpdateState.Failed(UpdateFailure.NOT_VERIFIED, null), controller.state.value)
        assertEquals(now, store.record.lastFailureMs)
        assertNull(store.record.lastAnswerMs)
    }

    // Update

    @Test
    fun updateDownloadsVerifiesAndHandsOverThenDeletesTheFile() {
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.HandingOver(offer), controller.state.value)
        assertArrayEquals(apk, installer.received.single())
        assertTrue(files.all().isEmpty())
        assertEquals(1, net.requests.count { it.url == apkUrl })
        controller.onInstallResult(InstallResult.NeedsConfirmation)
        assertEquals(UpdateState.HandingOver(offer), controller.state.value)
        controller.onInstallResult(InstallResult.Installed)
        assertEquals(UpdateState.Idle, controller.state.value)
    }

    @Test
    fun nothingIsDownloadedBeforeUpdate() {
        controller.onForeground()
        assertEquals(0, net.requests.count { it.url == apkUrl })
    }

    @Test
    fun withoutTheInstallPermissionItAsksFirstAndGoesOnOnceAllowed() {
        installer.allowed = false
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.NeedsPermission(offer), controller.state.value)
        assertEquals(0, net.requests.count { it.url == apkUrl })
        controller.onPermissionMaybeGranted()
        assertEquals(UpdateState.NeedsPermission(offer), controller.state.value)
        installer.allowed = true
        controller.onPermissionMaybeGranted()
        assertEquals(UpdateState.HandingOver(offer), controller.state.value)
    }

    @Test
    fun aMismatchedDownloadIsDeletedAndRefused() {
        net.replies[apkUrl] = FakeTransport.Reply(body = apk.copyOf().also { it[5] = (it[5] + 1).toByte() })
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.Failed(UpdateFailure.DIGEST_MISMATCH, offer), controller.state.value)
        assertTrue(installer.received.isEmpty())
        assertTrue(files.created.isNotEmpty())
        assertTrue(files.all().isEmpty())
    }

    @Test
    fun aFailedDownloadIsDeleted() {
        net.replies[apkUrl] = FakeTransport.Reply(body = apk.copyOf(1000), contentLength = apk.size.toLong())
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.Failed(UpdateFailure.WRONG_LENGTH, offer), controller.state.value)
        assertTrue(files.all().isEmpty())
    }

    @Test
    fun anInstallerRefusalIsShownAndTheFileDeleted() {
        installer.start = InstallStart.Failed(UpdateFailure.INSTALL_STORAGE)
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.Failed(UpdateFailure.INSTALL_STORAGE, offer), controller.state.value)
        assertTrue(files.all().isEmpty())
    }

    @Test
    fun androidsAnswerAfterTheHandOverIsShown() {
        controller.onForeground()
        controller.update()
        controller.onInstallResult(InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE))
        assertEquals(UpdateState.Failed(UpdateFailure.INSTALL_SIGNATURE, offer), controller.state.value)
    }

    @Test
    fun aFailureAfterTheNoticeWasClosedIsStillShownAndStrayAnswersAreIgnored() {
        controller.onInstallResult(InstallResult.Failed(UpdateFailure.INSTALL_ABORTED))
        assertEquals(UpdateState.Idle, controller.state.value)
        controller.onForeground()
        controller.update()
        controller.dismiss()
        controller.onInstallResult(InstallResult.Failed(UpdateFailure.INSTALL_ABORTED))
        assertEquals(UpdateState.Failed(UpdateFailure.INSTALL_ABORTED, offer), controller.state.value)
    }

    @Test
    fun cancellingADownloadStopsItAndDeletesTheFile() {
        net.replies[apkUrl] = FakeTransport.Reply(
            contentLength = apk.size.toLong(),
            stream = {
                object : InputStream() {
                    private val inner = ByteArrayInputStream(apk)
                    override fun read(): Int = inner.read()
                    override fun read(b: ByteArray, off: Int, len: Int): Int {
                        // The person taps Cancel while the first bytes arrive.
                        controller.dismiss()
                        return inner.read(b, off, len)
                    }
                }
            },
        )
        controller.onForeground()
        controller.update()
        assertEquals(UpdateState.Idle, controller.state.value)
        assertTrue(installer.received.isEmpty())
        assertTrue(files.created.isNotEmpty())
        assertTrue(files.all().isEmpty())
    }

    @Test
    fun leftoversOfAKilledProcessAreRemovedOnStart() {
        val c = controller
        val stale = File(files.dir, "old.apk").apply { writeText("x") }
        c.start()
        assertTrue(!stale.exists())
    }

    // Fakes

    private class MemoryStore : UpdateStore {
        var record = UpdateRecord(enabled = true, lastAnswerMs = null, lastFailureMs = null, laterVersion = null, laterAtMs = null)
        override suspend fun load() = record
        override suspend fun save(transform: (UpdateRecord) -> UpdateRecord) {
            record = transform(record)
        }
    }

    /** The core's rule: a day after an answer, an hour after a newer failure. */
    private object DayRules : UpdateRules {
        override fun due(lastAnswerMs: Long?, lastFailureMs: Long?, nowMs: Long): Boolean {
            val failedLast = lastFailureMs != null && (lastAnswerMs == null || lastFailureMs > lastAnswerMs)
            return when {
                failedLast -> nowMs - lastFailureMs >= 60 * 60 * 1000L
                lastAnswerMs == null -> true
                else -> nowMs - lastAnswerMs >= 24 * 60 * 60 * 1000L
            }
        }

        override fun assetMatches(asset: UpdateAsset, sha256: ByteArray) = hex(sha256) == asset.sha256Hex
    }

    private class FakeInstaller : UpdateInstaller {
        var allowed = true
        var start: InstallStart = InstallStart.Committed
        val received = mutableListOf<ByteArray>()
        override fun canInstall() = allowed
        override fun install(apk: File): InstallStart {
            if (start == InstallStart.Committed) received += apk.readBytes()
            return start
        }
    }

    private class TempFiles(val dir: File) : DownloadFiles {
        val created = mutableListOf<File>()
        override fun create(): File = File.createTempFile("update", ".apk", dir).also { created += it }
        override fun delete(file: File) {
            file.delete()
        }
        override fun clearLeftovers() {
            dir.listFiles()?.forEach { it.delete() }
        }
        fun all(): List<File> = dir.listFiles()?.toList().orEmpty()
    }

    private companion object {
        const val SUMS = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS"
        const val SIG = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig"

        fun sha256(b: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(b)
        fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    }
}
