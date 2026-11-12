package org.playdrop.android.share

import android.content.Context
import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import org.playdrop.android.core.UriSource

/**
 * What a send activity keeps across a rotation: the items being prepared (so a rotation
 * neither prepares them twice nor drops descriptors half-way), the picked URIs, and how far
 * the send got. Items that were prepared but never handed to a send are released when the
 * activity is gone for good ([onCleared]), including a preparation that finishes after that.
 */
internal class SendFlowModel : ViewModel() {
    private val lock = Any()
    private var preparing: Deferred<UriSource.Prepared>? = null
    private var ready: UriSource.Prepared? = null
    private var cleared = false

    /** URIs the picker returned, kept so a recreated activity can go on with them. */
    var picked: List<Uri>? = null

    /** A send to one device went: a recreated activity just finishes. */
    var handedOver = false

    /** The send to several devices whose card a recreated activity shows again. */
    var runToken: Long? = null

    /**
     * Prepares [uris] once per screen, on `Dispatchers.IO` (a second call returns the same
     * preparation). The application context is enough: grants are checked per app, and this
     * runs while the activity that received them is alive.
     */
    fun prepare(app: Context, uris: List<Uri>): Deferred<UriSource.Prepared> = synchronized(lock) {
        preparing ?: viewModelScope.async(Dispatchers.IO) {
            val p = UriSource.prepare(app, uris)
            val kept = synchronized(lock) {
                if (!cleared) ready = p
                !cleared
            }
            if (!kept) {
                p.release()
                throw CancellationException("the send screen is gone")
            }
            p
        }.also { preparing = it }
    }

    /** Hands the finished preparation to a send, which owns it from now on; null once taken. */
    fun take(): UriSource.Prepared? = synchronized(lock) { ready.also { ready = null } }

    override fun onCleared() {
        val left = synchronized(lock) {
            cleared = true
            ready.also { ready = null }
        }
        left?.release()
    }
}
