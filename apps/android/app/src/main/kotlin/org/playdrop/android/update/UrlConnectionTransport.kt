package org.playdrop.android.update

import java.io.ByteArrayInputStream
import java.net.HttpURLConnection
import java.net.URL

/**
 * [HttpTransport] over the platform's `HttpURLConnection` (HTTPS through
 * `HttpsURLConnection`), set up so a request carries only what [RequestPlan] sets plus what
 * the client cannot leave out (SR-45):
 *
 * - redirects are not followed here (`instanceFollowRedirects = false`); [UpdateFetcher]
 *   checks each target first (SR-48);
 * - `useCaches = false`, and no response cache is installed, so no `If-None-Match` or
 *   `If-Modified-Since` is ever added;
 * - no `CookieHandler` is installed anywhere in the app, so no `Cookie` header is added;
 * - Android's client adds `Host` and `Connection: Keep-Alive` itself, and `Accept-Encoding:
 *   gzip` and its own `User-Agent` only when they are not set; [RequestPlan] sets both. It
 *   adds no `Accept`.
 *
 * Nothing about the answer's headers is logged.
 */
class UrlConnectionTransport : HttpTransport {
    override fun get(url: String, headers: Map<String, String>, timeoutMs: Int): HttpAnswer {
        val conn = URL(url).openConnection() as HttpURLConnection
        try {
            conn.instanceFollowRedirects = false
            conn.useCaches = false
            conn.allowUserInteraction = false
            conn.connectTimeout = timeoutMs
            conn.readTimeout = timeoutMs
            conn.requestMethod = "GET"
            headers.forEach { (name, value) -> conn.setRequestProperty(name, value) }
            val status = conn.responseCode
            val location = if (status in UpdateFetcher.REDIRECTS) conn.getHeaderField("Location") else null
            val body = if (status in 200..299) conn.inputStream else (conn.errorStream ?: ByteArrayInputStream(ByteArray(0)))
            return HttpAnswer(status, location, conn.contentLengthLong, body) { conn.disconnect() }
        } catch (e: Exception) {
            conn.disconnect()
            throw e
        }
    }
}
