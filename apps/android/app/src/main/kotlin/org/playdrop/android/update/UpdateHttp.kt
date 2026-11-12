package org.playdrop.android.update

import java.io.Closeable
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.InterruptedIOException
import java.io.OutputStream
import java.net.ProtocolException
import java.net.SocketTimeoutException
import java.net.URI
import java.security.MessageDigest

/**
 * One HTTP GET that does not follow redirects. The real one is [UrlConnectionTransport];
 * tests use a fake, so the request plan, the redirect loop and the limits run on the JVM.
 */
fun interface HttpTransport {
    /** Sends GET [url] with exactly [headers] and returns the answer head with its body. */
    @Throws(IOException::class)
    fun get(url: String, headers: Map<String, String>, timeoutMs: Int): HttpAnswer
}

/** An answer: the status, the `Location` of a redirect, the promised length and the body. */
class HttpAnswer(
    /** HTTP status. */
    val status: Int,
    /** The `Location` header, for redirects only. */
    val location: String?,
    /** `Content-Length`, or -1 when the server did not say. */
    val contentLength: Long,
    /** The body; read it at most once. */
    val body: InputStream,
    private val onClose: () -> Unit = {},
) : Closeable {
    override fun close() {
        try {
            body.close()
        } catch (_: IOException) {
            // Closing a half-read body may fail; the connection is dropped next anyway.
        } finally {
            onClose()
        }
    }
}

/** The fixed numbers of the update channel, read from the core once (`playdrop_core::update`). */
data class UpdateLimits(
    /** The only `User-Agent` a request carries. */
    val userAgent: String,
    /** Most redirects for one request. */
    val maxRedirects: Int,
    /** Largest `SHA256SUMS`. */
    val maxSumsLen: Long,
    /** Largest `SHA256SUMS.sig`. */
    val maxSigLen: Long,
    /** Largest download. */
    val maxDownloadLen: Long,
    /** Time limit per check request, redirects included, and per read of a download. */
    val timeoutMs: Int,
)

/**
 * The request head, the same for every update request (SR-45): `User-Agent` set to the
 * core's fixed value, which replaces the platform's "Dalvik/... (Linux; U; Android ...;
 * <model> Build/...)", and `Accept-Encoding: identity`, which replaces the client's own
 * `gzip` (it cannot be removed, only replaced) so the bytes counted and hashed are the bytes
 * sent. Nothing else: no cookie, no cache validator, no version, no ID. `Host` and
 * `Connection: Keep-Alive` are added by the client and cannot be left out.
 */
object RequestPlan {
    /** The headers set on every request and every redirect hop. */
    fun headers(userAgent: String): Map<String, String> = linkedMapOf(
        "User-Agent" to userAgent,
        "Accept-Encoding" to "identity",
    )
}

/** A download that arrived whole. */
class Downloaded(
    /** Bytes written. */
    val length: Long,
    /** Their SHA-256. */
    val sha256: ByteArray,
)

/**
 * Follows redirects by hand, each one checked before anything is sent to it (SR-48), and
 * enforces the size, length and time limits. Blocking; callers run it on an IO thread.
 * Nothing about an answer's headers is logged.
 *
 * @param allowed the redirect allow list ([UpdateEndpoints.redirectAllowed]).
 * @param nanoTime the clock for the per-request deadline.
 */
class UpdateFetcher(
    private val transport: HttpTransport,
    private val limits: UpdateLimits,
    private val allowed: (String) -> Boolean,
    private val nanoTime: () -> Long = System::nanoTime,
) {
    private val headers = RequestPlan.headers(limits.userAgent)

    /** GETs [url] and returns at most [limit] bytes of the answer, all within the time limit. */
    @Throws(UpdateFetchException::class)
    fun getSmall(url: String, limit: Long): ByteArray {
        val deadline = nanoTime() + limits.timeoutMs * 1_000_000L
        return guarded {
            follow(url, deadline).use { answer ->
                if (answer.contentLength > limit) throw UpdateFetchException(UpdateFailure.TOO_LARGE)
                val bytes = readCapped(answer.body, limit, deadline)
                if (answer.contentLength >= 0 && bytes.size.toLong() != answer.contentLength) {
                    throw UpdateFetchException(UpdateFailure.WRONG_LENGTH)
                }
                bytes
            }
        }
    }

    /**
     * Streams [url] into [out] while hashing it, refusing more than [limit] bytes and any
     * length other than a promised one. [onProgress] gets (done, total or -1) now and then;
     * [stopped] is asked between reads, and a true answer ends the download with
     * [InterruptedIOException].
     */
    @Throws(UpdateFetchException::class, InterruptedIOException::class)
    fun download(url: String, out: OutputStream, limit: Long, stopped: () -> Boolean, onProgress: (Long, Long) -> Unit): Downloaded {
        val deadline = nanoTime() + limits.timeoutMs * 1_000_000L
        return guarded {
            follow(url, deadline).use { answer ->
                val total = answer.contentLength
                if (total > limit) throw UpdateFetchException(UpdateFailure.TOO_LARGE)
                val digest = MessageDigest.getInstance("SHA-256")
                val buf = ByteArray(BUFFER)
                var done = 0L
                var reported = 0L
                onProgress(0, total)
                while (true) {
                    if (stopped()) throw InterruptedIOException("download stopped")
                    val n = answer.body.read(buf)
                    if (n < 0) break
                    done += n
                    if (done > limit || (total >= 0 && done > total)) {
                        throw UpdateFetchException(if (done > limit) UpdateFailure.TOO_LARGE else UpdateFailure.WRONG_LENGTH)
                    }
                    digest.update(buf, 0, n)
                    try {
                        out.write(buf, 0, n)
                    } catch (e: IOException) {
                        throw UpdateFetchException(UpdateFailure.STORAGE, e)
                    }
                    if (done - reported >= PROGRESS_STEP) {
                        reported = done
                        onProgress(done, total)
                    }
                }
                if (total >= 0 && done != total) throw UpdateFetchException(UpdateFailure.WRONG_LENGTH)
                try {
                    out.flush()
                } catch (e: IOException) {
                    throw UpdateFetchException(UpdateFailure.STORAGE, e)
                }
                onProgress(done, total)
                Downloaded(done, digest.digest())
            }
        }
    }

    /** Sends GET [start] and follows allowed redirects; returns the first 2xx answer, open. */
    private fun follow(start: String, deadline: Long): HttpAnswer {
        var url = start
        var redirects = 0
        while (true) {
            val left = (deadline - nanoTime()) / 1_000_000L
            if (left <= 0) throw UpdateFetchException(UpdateFailure.TIMED_OUT)
            val answer = transport.get(url, headers, minOf(left, limits.timeoutMs.toLong()).toInt())
            when (answer.status) {
                in 200..299 -> return answer
                in REDIRECTS -> {
                    val target = answer.location?.let { resolve(url, it) }
                    answer.close()
                    if (target == null || !allowed(target)) throw UpdateFetchException(UpdateFailure.REDIRECT_REFUSED)
                    if (++redirects > limits.maxRedirects) throw UpdateFetchException(UpdateFailure.TOO_MANY_REDIRECTS)
                    url = target
                }
                else -> {
                    answer.close()
                    throw UpdateFetchException(UpdateFailure.SERVER_ERROR)
                }
            }
        }
    }

    /** Reads [input] whole, failing past [limit] bytes or the [deadline]. */
    private fun readCapped(input: InputStream, limit: Long, deadline: Long): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        val buf = ByteArray(BUFFER)
        var total = 0L
        while (true) {
            if (nanoTime() > deadline) throw UpdateFetchException(UpdateFailure.TIMED_OUT)
            val n = input.read(buf)
            if (n < 0) break
            total += n
            if (total > limit) throw UpdateFetchException(UpdateFailure.TOO_LARGE)
            out.write(buf, 0, n)
        }
        return out.toByteArray()
    }

    /** Turns client exceptions into [UpdateFailure]s; ours and a requested stop pass through. */
    private inline fun <T> guarded(block: () -> T): T = try {
        block()
    } catch (e: UpdateFetchException) {
        throw e
    } catch (e: SocketTimeoutException) {
        throw UpdateFetchException(UpdateFailure.TIMED_OUT, e)
    } catch (e: InterruptedIOException) {
        throw e
    } catch (e: EOFException) {
        throw UpdateFetchException(UpdateFailure.WRONG_LENGTH, e)
    } catch (e: ProtocolException) {
        // The client's "unexpected end of stream" and friends: the body broke its promise.
        throw UpdateFetchException(UpdateFailure.WRONG_LENGTH, e)
    } catch (e: IOException) {
        throw UpdateFetchException(UpdateFailure.NO_CONNECTION, e)
    } catch (e: IllegalArgumentException) {
        // A malformed URL from a redirect.
        throw UpdateFetchException(UpdateFailure.REDIRECT_REFUSED, e)
    }

    companion object {
        /** The statuses followed as redirects. */
        val REDIRECTS = setOf(301, 302, 303, 307, 308)

        private const val BUFFER = 64 * 1024
        private const val PROGRESS_STEP = 256L * 1024

        /** [location] made absolute against [base]; null when it is not a URL. */
        fun resolve(base: String, location: String): String? = try {
            URI(base).resolve(location.trim()).toString()
        } catch (_: Exception) {
            null
        }
    }
}
