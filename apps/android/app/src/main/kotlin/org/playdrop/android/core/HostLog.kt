package org.playdrop.android.core

import android.util.Log

/**
 * Kotlin-side logging under the same logcat tag the Rust core uses (`playdrop`), so one
 * `adb logcat -s playdrop:*` shows both. Callers never pass clipboard text, SAS digits, QR
 * URIs or file contents (SR-32).
 */
internal object HostLog {
    /** Logcat tag shared with the core. */
    const val TAG = "playdrop"

    /** Informational line. */
    fun i(area: String, message: String) {
        Log.i(TAG, "[$area] $message")
    }

    /** Debug line; file names are allowed here only (SR-32). */
    fun d(area: String, message: String) {
        Log.d(TAG, "[$area] $message")
    }

    /**
     * Warning with the exception class (anti-pattern 18: no silent failures). The message of
     * the exception is included because core and platform errors carry no secrets.
     */
    fun w(area: String, message: String, error: Throwable? = null) {
        logW(area, message, error)
    }

    /**
     * Warning with the exception class only, for errors whose text can name a shared file
     * ("Cannot read <name>"): file names stay at debug level (SR-32), where the text goes.
     */
    fun wNoDetail(area: String, message: String, error: Throwable) {
        Log.w(TAG, "[$area] $message: ${error.javaClass.simpleName}")
        Log.d(TAG, "[$area] $message: ${error.message}")
    }

    private fun logW(area: String, message: String, error: Throwable?) {
        if (error == null) {
            Log.w(TAG, "[$area] $message")
        } else {
            Log.w(TAG, "[$area] $message: ${error.javaClass.simpleName}: ${error.message}")
        }
    }
}
