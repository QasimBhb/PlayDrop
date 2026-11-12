package org.playdrop.android.update

import android.content.Intent
import org.playdrop.core.UpdateAsset
import org.playdrop.core.UpdateOffer

/**
 * Where the update check goes and which redirects it may follow. In a release build this is
 * always GitHub with the core's allow list; a debug build may swap the releases base for a
 * stand-in server on loopback ([UpdateOverride], `docs/design/update-check.md` section 8).
 *
 * @param sumsUrl the core's `SHA256SUMS` URL.
 * @param sigUrl the core's `SHA256SUMS.sig` URL.
 * @param coreAllows the core's redirect rule (`update_redirect_allowed`).
 * @param override the debug override, always null in a release build.
 */
class UpdateEndpoints(
    sumsUrl: String,
    sigUrl: String,
    private val coreAllows: (String) -> Boolean,
    private val override: UpdateOverride?,
) {
    /** The first request. */
    val sums: String = rebase(sumsUrl)

    /** The second request. */
    val sig: String = rebase(sigUrl)

    /**
     * Whether a redirect to [url] may be followed: the core's rule (HTTPS to GitHub only),
     * or, with a debug override on loopback, the same origin as the stand-in server.
     */
    fun redirectAllowed(url: String): Boolean =
        coreAllows(url) || override?.testOrigin?.let { origin -> sameOrigin(url, origin) } == true

    /** [offer] with its URLs pointed at the configured base (only ever different in debug builds). */
    fun rebase(offer: UpdateOffer): UpdateOffer = offer.copy(
        pageUrl = rebase(offer.pageUrl),
        asset = offer.asset?.let { a: UpdateAsset -> a.copy(url = rebase(a.url)) },
    )

    private fun rebase(url: String): String {
        val base = override?.releasesBase ?: return url
        return if (url.startsWith(RELEASES)) base + url.removePrefix(RELEASES) else url
    }

    companion object {
        /** The releases base every core URL starts with; a debug override replaces it. */
        const val RELEASES = "https://github.com/QasimBhb/PlayDrop/releases"

        /** True when [url] is [origin] itself or a path under it. */
        fun sameOrigin(url: String, origin: String): Boolean {
            if (!url.startsWith(origin)) return false
            val rest = url.substring(origin.length)
            return rest.isEmpty() || rest.startsWith("/")
        }

        /**
         * `http://127.0.0.1:<port>` or `http://localhost:<port>` when [base] starts with exactly
         * that origin, else null: the only plain-HTTP bases a debug override accepts (the
         * debug network rule allows cleartext to those two hosts only).
         */
        fun loopbackOrigin(base: String): String? {
            if (!base.startsWith("http://")) return null
            val rest = base.removePrefix("http://")
            val authority = rest.substringBefore('/')
            val host = authority.substringBefore(':', "")
            val port = authority.substringAfter(':', "")
            val portOk = port.isNotEmpty() && port.length <= 5 && port.all(Char::isDigit) && port.toInt() in 1..65535
            return if ((host == "127.0.0.1" || host == "localhost") && portOk) "http://$authority" else null
        }
    }
}

/**
 * A debug build's stand-in for GitHub and its throwaway key. Only the debug source set has
 * an implementation; the release one never returns an override.
 */
interface UpdateOverride {
    /** Replaces [UpdateEndpoints.RELEASES] in every URL. */
    val releasesBase: String

    /** The loopback origin of [releasesBase] when it is plain HTTP, where redirects may also go. */
    val testOrigin: String?

    /** `update_offer_with_test_key` with the override's test key; same contract as [OfferDecider]. */
    fun offer(sums: ByteArray, sig: ByteArray, currentVersion: String): UpdateOffer?
}

/** What an intent asked of the test hooks. */
data class HookRequest(
    /** The intent asked for an update check right away (debug only). */
    val checkNow: Boolean,
)

/**
 * The seam between the app and the build type: `BuildUpdateHooks` in `src/debug/kotlin`
 * reads intent extras and keeps an override for the process; the one in `src/release/kotlin`
 * reads nothing and never overrides, so a release APK contains neither the code path nor
 * the cleartext rule.
 */
interface UpdateHooks {
    /** Reads the debug extras of [intent], if any. */
    fun read(intent: Intent?): HookRequest

    /** The current override, or null. */
    fun current(): UpdateOverride?
}
