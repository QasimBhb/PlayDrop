package org.playdrop.android.update

import android.content.Intent

/**
 * The release build's update hooks: none. It reads no intent extra and never overrides
 * the GitHub URLs or the release keys, so a shipped PlayDrop cannot be pointed at another
 * server or trust another key (`docs/design/update-check.md` section 8). The debug build
 * type has its own `BuildUpdateHooks` in `src/debug/kotlin`.
 */
internal object BuildUpdateHooks : UpdateHooks {
    override fun read(intent: Intent?): HookRequest = HookRequest(checkNow = false)

    override fun current(): UpdateOverride? = null
}
