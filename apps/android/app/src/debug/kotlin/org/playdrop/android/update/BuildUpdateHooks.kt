package org.playdrop.android.update

import android.content.Intent
import org.playdrop.android.core.HostLog
import org.playdrop.core.UpdateInstallKind
import org.playdrop.core.UpdateOffer
import org.playdrop.core.updateOfferWithTestKey

/**
 * The debug build's update hooks (`docs/design/update-check.md` section 8): point the
 * check at a stand-in server and trust one more key, for phone tests without a release.
 * Only in the debug build type; the release one has a `BuildUpdateHooks` that does nothing.
 *
 * Set by starting `MainActivity` with these extras (they stay for the life of the process):
 * - [EXTRA_BASE]: replaces `https://github.com/QasimBhb/PlayDrop/releases`. Only
 *   `http://127.0.0.1:<port>` or `http://localhost:<port>` is accepted (the debug network
 *   rule allows cleartext to those two hosts only; use `adb reverse`), so no other app can
 *   point a debug build at a server elsewhere; redirects may then also go to that origin.
 *   An empty value clears the override.
 * - [EXTRA_TEST_KEY]: the stand-in's Ed25519 public key, 64 hex characters. Required with a
 *   base: the release keys stay trusted next to it.
 * - [EXTRA_CHECK_NOW]: `true` runs a check at once, as "Check now" does (the switch still
 *   counts).
 */
internal object BuildUpdateHooks : UpdateHooks {
    const val EXTRA_BASE = "org.playdrop.android.debug.UPDATE_BASE"
    const val EXTRA_TEST_KEY = "org.playdrop.android.debug.UPDATE_TEST_KEY"
    const val EXTRA_CHECK_NOW = "org.playdrop.android.debug.UPDATE_CHECK_NOW"

    @Volatile
    private var override: UpdateOverride? = null

    override fun read(intent: Intent?): HookRequest {
        if (intent == null) return HookRequest(checkNow = false)
        val base = intent.getStringExtra(EXTRA_BASE)
        if (base != null) {
            override = parse(base.trim().trimEnd('/'), intent.getStringExtra(EXTRA_TEST_KEY)?.trim().orEmpty())
            // Read once: a recreated activity gets the same intent and must not parse it again.
            intent.removeExtra(EXTRA_BASE)
            intent.removeExtra(EXTRA_TEST_KEY)
        }
        val checkNow = intent.getBooleanExtra(EXTRA_CHECK_NOW, false)
        intent.removeExtra(EXTRA_CHECK_NOW)
        return HookRequest(checkNow)
    }

    override fun current(): UpdateOverride? = override

    private fun parse(base: String, keyHex: String): UpdateOverride? {
        if (base.isEmpty()) {
            HostLog.i("update", "update test hooks cleared (debug build)")
            return null
        }
        val origin = UpdateEndpoints.loopbackOrigin(base)
        if (origin == null) {
            HostLog.w("update", "$EXTRA_BASE ignored: only http://127.0.0.1:<port> or http://localhost:<port> is accepted")
            return override
        }
        if (keyHex.length != 64 || !keyHex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) {
            HostLog.w("update", "$EXTRA_TEST_KEY missing or not 64 hex characters; hooks unchanged")
            return override
        }
        HostLog.w("update", "update check test hooks in effect (debug build): $base")
        return TestOverride(base, origin, keyHex)
    }

    private class TestOverride(
        override val releasesBase: String,
        override val testOrigin: String?,
        private val keyHex: String,
    ) : UpdateOverride {
        override fun offer(sums: ByteArray, sig: ByteArray, currentVersion: String): UpdateOffer? =
            refusalsAsFailures { updateOfferWithTestKey(sums, sig, currentVersion, UpdateInstallKind.ANDROID, keyHex) }
    }
}
