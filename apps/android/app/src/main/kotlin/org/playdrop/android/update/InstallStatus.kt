package org.playdrop.android.update

/**
 * Android's `PackageInstaller` answers in plain terms. The status values are the public
 * `PackageInstaller.STATUS_*` constants, spelled out so the mapping is unit-tested on the JVM.
 */
object InstallStatus {
    /** `STATUS_PENDING_USER_ACTION`. */
    const val PENDING_USER_ACTION = -1

    /** `STATUS_SUCCESS`. */
    const val SUCCESS = 0

    /** `STATUS_FAILURE`. */
    const val FAILURE = 1

    /** `STATUS_FAILURE_BLOCKED`. */
    const val FAILURE_BLOCKED = 2

    /** `STATUS_FAILURE_ABORTED`: the person cancelled. */
    const val FAILURE_ABORTED = 3

    /** `STATUS_FAILURE_INVALID`. */
    const val FAILURE_INVALID = 4

    /** `STATUS_FAILURE_CONFLICT`: an installed package conflicts, mostly another signing key. */
    const val FAILURE_CONFLICT = 5

    /** `STATUS_FAILURE_STORAGE`. */
    const val FAILURE_STORAGE = 6

    /** `STATUS_FAILURE_INCOMPATIBLE`. */
    const val FAILURE_INCOMPATIBLE = 7

    /** `STATUS_FAILURE_TIMEOUT`. */
    const val FAILURE_TIMEOUT = 8

    /**
     * `INSTALL_FAILED_UPDATE_INCOMPATIBLE` in the legacy status extra: the update is signed
     * with another key than the installed app.
     */
    const val LEGACY_UPDATE_INCOMPATIBLE = -7

    /** The [InstallResult] for [status] and the legacy status (0 when absent). */
    fun of(status: Int, legacyStatus: Int = 0): InstallResult = when (status) {
        PENDING_USER_ACTION -> InstallResult.NeedsConfirmation
        SUCCESS -> InstallResult.Installed
        FAILURE_ABORTED -> InstallResult.Failed(UpdateFailure.INSTALL_ABORTED)
        FAILURE_BLOCKED -> InstallResult.Failed(UpdateFailure.INSTALL_BLOCKED)
        // A conflict with the installed app is, for an update of itself, the signing key.
        FAILURE_CONFLICT -> InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE)
        FAILURE_INVALID -> InstallResult.Failed(UpdateFailure.INSTALL_INVALID)
        FAILURE_STORAGE -> InstallResult.Failed(UpdateFailure.INSTALL_STORAGE)
        FAILURE_INCOMPATIBLE ->
            if (legacyStatus == LEGACY_UPDATE_INCOMPATIBLE) {
                InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE)
            } else {
                InstallResult.Failed(UpdateFailure.INSTALL_INCOMPATIBLE)
            }
        else ->
            if (legacyStatus == LEGACY_UPDATE_INCOMPATIBLE) {
                InstallResult.Failed(UpdateFailure.INSTALL_SIGNATURE)
            } else {
                InstallResult.Failed(UpdateFailure.INSTALL_FAILED)
            }
    }
}
