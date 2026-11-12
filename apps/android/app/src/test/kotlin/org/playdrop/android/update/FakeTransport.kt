package org.playdrop.android.update

import java.io.ByteArrayInputStream
import java.io.IOException
import java.io.InputStream

/** A canned server: each URL answers with the reply given for it, and every request is recorded. */
internal class FakeTransport : HttpTransport {
    /** One canned answer. */
    data class Reply(
        val status: Int = 200,
        val body: ByteArray = ByteArray(0),
        val location: String? = null,
        /** Content-Length to announce; null announces the body's real length, -1 none. */
        val contentLength: Long? = null,
        /** Fails the request itself (no answer at all). */
        val error: IOException? = null,
        /** The body stream to hand out instead of [body]. */
        val stream: (() -> InputStream)? = null,
    )

    /** One request as sent. */
    data class Request(val url: String, val headers: Map<String, String>, val timeoutMs: Int)

    val replies = mutableMapOf<String, Reply>()
    val requests = mutableListOf<Request>()
    var closed = 0

    override fun get(url: String, headers: Map<String, String>, timeoutMs: Int): HttpAnswer {
        requests += Request(url, headers, timeoutMs)
        val r = replies[url] ?: Reply(status = 404)
        r.error?.let { throw it }
        val length = r.contentLength ?: r.body.size.toLong()
        return HttpAnswer(r.status, r.location, length, r.stream?.invoke() ?: ByteArrayInputStream(r.body)) { closed++ }
    }
}

internal val TEST_LIMITS = UpdateLimits(
    userAgent = "PlayDrop",
    maxRedirects = 5,
    maxSumsLen = 64 * 1024,
    maxSigLen = 1024,
    maxDownloadLen = 1024 * 1024,
    timeoutMs = 15_000,
)

/** The core's allow list, as `update_redirect_allowed` decides it, for tests. */
internal fun githubOnly(url: String): Boolean {
    if (!url.startsWith("https://")) return false
    val host = url.removePrefix("https://").substringBefore('/').substringBefore('?')
    return host == "github.com" || host.endsWith(".githubusercontent.com")
}
