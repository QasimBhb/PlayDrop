package org.playdrop.android.qr

import android.graphics.Bitmap
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/** QR helpers: encoding the pairing URI and recognising one when scanned. */
object QrCodes {
    /** Scheme and path every PlayDrop pairing URI starts with (protocol.md, QR pairing). */
    const val PAIR_PREFIX = "playdrop://pair?"

    /** True when [text] is a PlayDrop pairing URI. */
    fun isPairUri(text: String): Boolean = text.startsWith(PAIR_PREFIX, ignoreCase = true)

    /** Encodes [text] as a QR matrix of [size] pixels with a one-module quiet zone. */
    fun encode(text: String, size: Int): BitMatrix = QRCodeWriter().encode(
        text,
        BarcodeFormat.QR_CODE,
        size,
        size,
        mapOf(EncodeHintType.MARGIN to 1, EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M),
    )

    /** ARGB pixels of [m]: black modules on white. */
    fun pixels(m: BitMatrix): IntArray {
        val w = m.width
        val h = m.height
        val out = IntArray(w * h)
        for (y in 0 until h) {
            val row = y * w
            for (x in 0 until w) out[row + x] = if (m.get(x, y)) BLACK else WHITE
        }
        return out
    }

    /** A bitmap of [text]; CPU work, call off the main thread. */
    fun bitmap(text: String, size: Int): Bitmap {
        val m = encode(text, size)
        return Bitmap.createBitmap(pixels(m), m.width, m.height, Bitmap.Config.ARGB_8888)
    }

    private const val BLACK = 0xFF000000.toInt()
    private const val WHITE = 0xFFFFFFFF.toInt()
}
