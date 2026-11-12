package org.playdrop.android.update

import org.playdrop.android.R

/**
 * Every way an update check or an update can fail, each with one plain sentence
 * (anti-pattern 18). Never an HTTP status, a header or a wire code (SR-32 extension).
 */
enum class UpdateFailure(
    /** The sentence shown to the user. */
    val sentence: Int,
) {
    /** No network, no DNS, the connection broke. */
    NO_CONNECTION(R.string.update_fail_no_connection),

    /** A request took longer than its limit (SR-48). */
    TIMED_OUT(R.string.update_fail_timed_out),

    /** GitHub answered with an error. */
    SERVER_ERROR(R.string.update_fail_server),

    /** A redirect pointed off the allow list or to plain HTTP (SR-48). */
    REDIRECT_REFUSED(R.string.update_fail_redirect),

    /** More redirects than allowed (SR-48). */
    TOO_MANY_REDIRECTS(R.string.update_fail_redirects),

    /** An answer or the download was larger than its limit (SR-48). */
    TOO_LARGE(R.string.update_fail_too_large),

    /** The download ended early or ran past the length the server promised (SR-48). */
    WRONG_LENGTH(R.string.update_fail_wrong_length),

    /** The signed list failed verification: unsigned, tampered or malformed (SR-46). */
    NOT_VERIFIED(R.string.update_fail_not_verified),

    /** This build's own version is not `major.minor.patch`, so nothing can be compared. */
    BAD_VERSION(R.string.update_fail_bad_version),

    /** The download's SHA-256 is not the signed one; the file was deleted (SR-46). */
    DIGEST_MISMATCH(R.string.update_fail_digest),

    /** The download could not be stored on the phone. */
    STORAGE(R.string.update_fail_storage),

    /** The person cancelled Android's install dialog. */
    INSTALL_ABORTED(R.string.update_fail_install_aborted),

    /** Android or a device policy blocked the install. */
    INSTALL_BLOCKED(R.string.update_fail_install_blocked),

    /** The installed app and the update are signed with different keys (a debug-key test install). */
    INSTALL_SIGNATURE(R.string.update_fail_install_signature),

    /** Android found the file broken. */
    INSTALL_INVALID(R.string.update_fail_install_invalid),

    /** No room to install. */
    INSTALL_STORAGE(R.string.update_fail_install_storage),

    /** The update needs a newer Android, or is otherwise not for this phone. */
    INSTALL_INCOMPATIBLE(R.string.update_fail_install_incompatible),

    /** Any other installer failure. */
    INSTALL_FAILED(R.string.update_fail_install_other),
}

/** A failed request or download, carrying the [failure] to show. */
class UpdateFetchException(val failure: UpdateFailure, cause: Throwable? = null) : Exception(failure.name, cause)
