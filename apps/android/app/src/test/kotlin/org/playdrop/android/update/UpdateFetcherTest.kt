package org.playdrop.android.update

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.io.InterruptedIOException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.security.MessageDigest

/** The request head, the redirect loop and the limits of the update channel (SR-45, SR-48). */
class UpdateFetcherTest {
    private val sums = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS"
    private val store = "https://objects.githubusercontent.com/x/SHA256SUMS"
    private val net = FakeTransport()
    private var clockNs = 0L
    private val fetcher = UpdateFetcher(net, TEST_LIMITS, ::githubOnly) { clockNs }

    private fun expectFailure(expected: UpdateFailure, block: () -> Unit) {
        try {
            block()
            fail("expected $expected")
        } catch (e: UpdateFetchException) {
            assertEquals(expected, e.failure)
        }
    }

    @Test
    fun theHeadCarriesOnlyTheFixedUserAgentAndNoCompression() {
        net.replies[sums] = FakeTransport.Reply(body = "x".toByteArray())
        fetcher.getSmall(sums, 100)
        val head = net.requests.single().headers
        assertEquals(mapOf("User-Agent" to "PlayDrop", "Accept-Encoding" to "identity"), head)
        // Nothing that identifies the phone or remembers an earlier answer.
        listOf("Cookie", "If-None-Match", "If-Modified-Since", "Referer", "Accept-Language").forEach { assertFalse(it, it in head) }
        assertEquals(15_000, net.requests.single().timeoutMs)
    }

    @Test
    fun redirectsAreFollowedByHandWithTheSameHead() {
        net.replies[sums] = FakeTransport.Reply(status = 302, location = store)
        net.replies[store] = FakeTransport.Reply(body = "sums".toByteArray())
        assertArrayEquals("sums".toByteArray(), fetcher.getSmall(sums, 100))
        assertEquals(listOf(sums, store), net.requests.map { it.url })
        assertEquals(net.requests[0].headers, net.requests[1].headers)
        assertEquals(2, net.closed)
    }

    @Test
    fun aRelativeLocationIsResolvedAndChecked() {
        net.replies[sums] = FakeTransport.Reply(status = 301, location = "/QasimBhb/PlayDrop/other")
        net.replies["https://github.com/QasimBhb/PlayDrop/other"] = FakeTransport.Reply(body = "ok".toByteArray())
        assertArrayEquals("ok".toByteArray(), fetcher.getSmall(sums, 100))
    }

    @Test
    fun aRedirectOffTheAllowListIsRefusedBeforeAnythingIsSentThere() {
        net.replies[sums] = FakeTransport.Reply(status = 302, location = "https://evil.example/SHA256SUMS")
        expectFailure(UpdateFailure.REDIRECT_REFUSED) { fetcher.getSmall(sums, 100) }
        assertEquals(listOf(sums), net.requests.map { it.url })
    }

    @Test
    fun aRedirectToPlainHttpOrWithoutLocationIsRefused() {
        net.replies[sums] = FakeTransport.Reply(status = 307, location = "http://objects.githubusercontent.com/x")
        expectFailure(UpdateFailure.REDIRECT_REFUSED) { fetcher.getSmall(sums, 100) }
        net.replies[sums] = FakeTransport.Reply(status = 308, location = null)
        expectFailure(UpdateFailure.REDIRECT_REFUSED) { fetcher.getSmall(sums, 100) }
    }

    @Test
    fun fiveRedirectsAreFollowedASixthIsNot() {
        val hops = (0..6).map { "https://github.com/hop$it" }
        hops.zipWithNext().forEach { (from, to) -> net.replies[from] = FakeTransport.Reply(status = 302, location = to) }
        net.replies[hops[5]] = FakeTransport.Reply(body = "end".toByteArray())
        assertArrayEquals("end".toByteArray(), fetcher.getSmall(hops[0], 100))
        assertEquals(6, net.requests.size)

        net.requests.clear()
        net.replies[hops[5]] = FakeTransport.Reply(status = 302, location = hops[6])
        net.replies[hops[6]] = FakeTransport.Reply(body = "too far".toByteArray())
        expectFailure(UpdateFailure.TOO_MANY_REDIRECTS) { fetcher.getSmall(hops[0], 100) }
        assertEquals(6, net.requests.size)
    }

    @Test
    fun sizeLimitsHoldWithAndWithoutAPromisedLength() {
        net.replies[sums] = FakeTransport.Reply(body = ByteArray(101))
        expectFailure(UpdateFailure.TOO_LARGE) { fetcher.getSmall(sums, 100) }
        net.replies[sums] = FakeTransport.Reply(body = ByteArray(101), contentLength = -1)
        expectFailure(UpdateFailure.TOO_LARGE) { fetcher.getSmall(sums, 100) }
        net.replies[sums] = FakeTransport.Reply(body = ByteArray(10), contentLength = 20)
        expectFailure(UpdateFailure.WRONG_LENGTH) { fetcher.getSmall(sums, 100) }
    }

    @Test
    fun errorsBecomePlainFailures() {
        net.replies[sums] = FakeTransport.Reply(status = 503)
        expectFailure(UpdateFailure.SERVER_ERROR) { fetcher.getSmall(sums, 100) }
        net.replies[sums] = FakeTransport.Reply(error = UnknownHostException("github.com"))
        expectFailure(UpdateFailure.NO_CONNECTION) { fetcher.getSmall(sums, 100) }
        net.replies[sums] = FakeTransport.Reply(error = SocketTimeoutException())
        expectFailure(UpdateFailure.TIMED_OUT) { fetcher.getSmall(sums, 100) }
    }

    @Test
    fun theWholeCheckRequestHasOneDeadline() {
        net.replies[sums] = FakeTransport.Reply(status = 302, location = store)
        net.replies[store] = FakeTransport.Reply(
            body = ByteArray(0),
            stream = {
                // The redirect took the whole time limit.
                clockNs += 16_000L * 1_000_000
                "late".byteInputStream()
            },
        )
        expectFailure(UpdateFailure.TIMED_OUT) { fetcher.getSmall(sums, 100) }
    }

    @Test
    fun aDownloadIsHashedWhileItStreams() {
        val apk = ByteArray(700_000) { (it % 251).toByte() }
        net.replies[store] = FakeTransport.Reply(body = apk)
        val out = ByteArrayOutputStream()
        val progress = mutableListOf<Pair<Long, Long>>()
        val got = fetcher.download(store, out, 1_000_000, stopped = { false }) { d, t -> progress += d to t }
        assertEquals(apk.size.toLong(), got.length)
        assertArrayEquals(MessageDigest.getInstance("SHA-256").digest(apk), got.sha256)
        assertArrayEquals(apk, out.toByteArray())
        assertEquals(0L to apk.size.toLong(), progress.first())
        assertEquals(apk.size.toLong() to apk.size.toLong(), progress.last())
    }

    @Test
    fun aDownloadOverTheLimitOrOffItsPromiseIsRefused() {
        net.replies[store] = FakeTransport.Reply(body = ByteArray(2_000))
        expectFailure(UpdateFailure.TOO_LARGE) { fetcher.download(store, ByteArrayOutputStream(), 1_000, { false }) { _, _ -> } }
        net.replies[store] = FakeTransport.Reply(body = ByteArray(2_000), contentLength = -1)
        expectFailure(UpdateFailure.TOO_LARGE) { fetcher.download(store, ByteArrayOutputStream(), 1_000, { false }) { _, _ -> } }
        net.replies[store] = FakeTransport.Reply(body = ByteArray(500), contentLength = 600)
        expectFailure(UpdateFailure.WRONG_LENGTH) { fetcher.download(store, ByteArrayOutputStream(), 1_000, { false }) { _, _ -> } }
        net.replies[store] = FakeTransport.Reply(body = ByteArray(700), contentLength = 600)
        expectFailure(UpdateFailure.WRONG_LENGTH) { fetcher.download(store, ByteArrayOutputStream(), 1_000, { false }) { _, _ -> } }
    }

    @Test
    fun aStoppedDownloadEndsAtOnce() {
        net.replies[store] = FakeTransport.Reply(body = ByteArray(10))
        try {
            fetcher.download(store, ByteArrayOutputStream(), 1_000, stopped = { true }) { _, _ -> }
            fail("expected a stop")
        } catch (_: InterruptedIOException) {
            // Expected.
        }
    }

    @Test
    fun aBodyThatBreaksOffIsAWrongLength() {
        net.replies[store] = FakeTransport.Reply(
            contentLength = 100,
            stream = {
                object : InputStream() {
                    private var sent = false
                    override fun read(): Int = throw UnsupportedOperationException()
                    override fun read(b: ByteArray, off: Int, len: Int): Int {
                        if (sent) throw java.net.ProtocolException("unexpected end of stream")
                        sent = true
                        return 10
                    }
                }
            },
        )
        expectFailure(UpdateFailure.WRONG_LENGTH) { fetcher.download(store, ByteArrayOutputStream(), 1_000, { false }) { _, _ -> } }
        assertTrue(net.closed > 0)
    }
}
