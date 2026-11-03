package org.playdrop.android.core

import com.sun.jna.Native

/**
 * Keeps the core's native threads attached to the VM between callbacks.
 *
 * UniFFI calls Kotlin callbacks through JNA, which by default attaches the calling native
 * thread to the VM for each call and detaches it afterwards: every call then builds and
 * drops a `java.lang.Thread` (the thread shows up as `Thread-1`, `Thread-2`, ... in `top`).
 * The core's bundle delivery threads call [DownloadsSink.deliver] once per received file,
 * thousands of times per transfer, so that churn sat on the critical path of a bulk drop
 * (`docs/release/throughput.md` section 13). After [keepAttached] the thread stays attached
 * until it exits; JNA detaches it then from its thread-exit hook.
 */
internal object CallbackThreads {
    private val attached = ThreadLocal<Boolean>()

    /** Call at the start of a callback that native threads make often. Never throws. */
    fun keepAttached() {
        if (attached.get() == true) return
        attached.set(true)
        // Only fails where JNA's native part is missing (plain JVM unit tests): harmless.
        runCatching { Native.detach(false) }
    }
}
