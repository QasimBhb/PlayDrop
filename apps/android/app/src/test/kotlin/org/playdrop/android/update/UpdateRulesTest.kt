package org.playdrop.android.update

import android.content.pm.PackageInstaller
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.core.UpdateAsset
import org.playdrop.core.UpdateOffer

/** Installer answers, the debug base override, the version and the failure sentences. */
class UpdateRulesTest {
    @Test
    fun installerStatusesBecomePlainResults() {
        assertEquals(InstallResult.NeedsConfirmation, InstallStatus.of(InstallStatus.PENDING_USER_ACTION))
        assertEquals(InstallResult.Installed, InstallStatus.of(InstallStatus.SUCCESS))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_ABORTED), InstallStatus.of(InstallStatus.FAILURE_ABORTED))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_BLOCKED), InstallStatus.of(InstallStatus.FAILURE_BLOCKED))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE), InstallStatus.of(InstallStatus.FAILURE_CONFLICT))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_INVALID), InstallStatus.of(InstallStatus.FAILURE_INVALID))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_STORAGE), InstallStatus.of(InstallStatus.FAILURE_STORAGE))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_INCOMPATIBLE), InstallStatus.of(InstallStatus.FAILURE_INCOMPATIBLE))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_FAILED), InstallStatus.of(InstallStatus.FAILURE))
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_FAILED), InstallStatus.of(InstallStatus.FAILURE_TIMEOUT))
        // A debug-key install updated with the release key: the legacy code names the key.
        assertEquals(
            InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE),
            InstallStatus.of(InstallStatus.FAILURE_INCOMPATIBLE, InstallStatus.LEGACY_UPDATE_INCOMPATIBLE),
        )
        assertEquals(InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE), InstallStatus.of(InstallStatus.FAILURE, InstallStatus.LEGACY_UPDATE_INCOMPATIBLE))
    }

    @Test
    fun theSpelledOutStatusesAreAndroids() {
        assertEquals(PackageInstaller.STATUS_PENDING_USER_ACTION, InstallStatus.PENDING_USER_ACTION)
        assertEquals(PackageInstaller.STATUS_SUCCESS, InstallStatus.SUCCESS)
        assertEquals(PackageInstaller.STATUS_FAILURE, InstallStatus.FAILURE)
        assertEquals(PackageInstaller.STATUS_FAILURE_BLOCKED, InstallStatus.FAILURE_BLOCKED)
        assertEquals(PackageInstaller.STATUS_FAILURE_ABORTED, InstallStatus.FAILURE_ABORTED)
        assertEquals(PackageInstaller.STATUS_FAILURE_INVALID, InstallStatus.FAILURE_INVALID)
        assertEquals(PackageInstaller.STATUS_FAILURE_CONFLICT, InstallStatus.FAILURE_CONFLICT)
        assertEquals(PackageInstaller.STATUS_FAILURE_STORAGE, InstallStatus.FAILURE_STORAGE)
        assertEquals(PackageInstaller.STATUS_FAILURE_INCOMPATIBLE, InstallStatus.FAILURE_INCOMPATIBLE)
        assertEquals(PackageInstaller.STATUS_FAILURE_TIMEOUT, InstallStatus.FAILURE_TIMEOUT)
    }

    @Test
    fun everyFailureHasItsOwnSentence() {
        val sentences = UpdateFailure.entries.map { it.sentence }
        assertEquals(sentences.size, sentences.toSet().size)
        assertEquals(R.string.update_fail_digest, UpdateFailure.DIGEST_MISMATCH.sentence)
        assertEquals(R.string.update_fail_install_signature, UpdateFailure.INSTALL_SIGNATURE.sentence)
    }

    @Test
    fun onlyLoopbackMayBePlainHttp() {
        assertEquals("http://127.0.0.1:8099", UpdateEndpoints.loopbackOrigin("http://127.0.0.1:8099/QasimBhb/PlayDrop/releases"))
        assertEquals("http://localhost:80", UpdateEndpoints.loopbackOrigin("http://localhost:80"))
        assertNull(UpdateEndpoints.loopbackOrigin("http://127.0.0.1/releases"))
        assertNull(UpdateEndpoints.loopbackOrigin("http://192.168.1.2:8099/releases"))
        assertNull(UpdateEndpoints.loopbackOrigin("http://localhost:80x/releases"))
        assertNull(UpdateEndpoints.loopbackOrigin("https://127.0.0.1:8099/releases"))
        assertNull(UpdateEndpoints.loopbackOrigin("http://127.0.0.1:99999/releases"))
        assertNull(UpdateEndpoints.loopbackOrigin("http://127.0.0.1:0/releases"))
    }

    @Test
    fun withoutAnOverrideEverythingIsGithub() {
        val e = UpdateEndpoints(SUMS, SIG, ::githubOnly, null)
        assertEquals(SUMS, e.sums)
        assertEquals(SIG, e.sig)
        assertEquals(OFFER, e.rebase(OFFER))
        assertFalse(e.redirectAllowed("http://127.0.0.1:8099/x"))
        assertTrue(e.redirectAllowed("https://objects.githubusercontent.com/x"))
    }

    @Test
    fun aDebugOverrideMovesEveryUrlAndAllowsItsOwnOrigin() {
        val base = "http://127.0.0.1:8099/r"
        val e = UpdateEndpoints(SUMS, SIG, ::githubOnly, Override(base, "http://127.0.0.1:8099"))
        assertEquals("$base/latest/download/SHA256SUMS", e.sums)
        assertEquals("$base/latest/download/SHA256SUMS.sig", e.sig)
        val moved = e.rebase(OFFER)
        assertEquals("$base/tag/v0.12.0", moved.pageUrl)
        assertEquals("$base/download/v0.12.0/a.apk", moved.asset?.url)
        assertTrue(e.redirectAllowed("http://127.0.0.1:8099/other"))
        assertFalse(e.redirectAllowed("http://127.0.0.1:80999/other"))
        assertFalse(e.redirectAllowed("http://127.0.0.1:8098/other"))
    }

    @Test
    fun onlyAPlainReleaseVersionChecksForUpdates() {
        assertTrue(ReleaseVersion.isPlain("0.11.0"))
        assertTrue(ReleaseVersion.isPlain("10.0.123"))
        for (v in listOf("0.11.0-debug", "0.11.0+42", "0.11", "v0.11.0", "01.2.3", "0.11.0 ", "", "0.11.0.1", "1.2.x")) {
            assertFalse(v, ReleaseVersion.isPlain(v))
        }
    }

    private class Override(override val releasesBase: String, override val testOrigin: String?) : UpdateOverride {
        override fun offer(sums: ByteArray, sig: ByteArray, currentVersion: String): UpdateOffer? = null
    }

    private companion object {
        const val SUMS = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS"
        const val SIG = "https://github.com/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig"
        val OFFER = UpdateOffer(
            "0.12.0",
            "https://github.com/QasimBhb/PlayDrop/releases/tag/v0.12.0",
            UpdateAsset("a.apk", "https://github.com/QasimBhb/PlayDrop/releases/download/v0.12.0/a.apk", "00"),
        )
    }
}
