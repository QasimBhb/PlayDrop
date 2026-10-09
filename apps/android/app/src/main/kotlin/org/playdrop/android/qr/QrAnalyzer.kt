package org.playdrop.android.qr

import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer

/**
 * Decodes QR codes from the camera's luminance plane with zxing and reports every PlayDrop
 * pairing URI it sees to [onPairUri]. Other codes are ignored. The URI is never logged
 * (SR-32).
 */
class QrAnalyzer(private val onPairUri: (String) -> Unit) : ImageAnalysis.Analyzer {
    private val reader = MultiFormatReader().apply {
        setHints(
            mapOf(
                DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
                DecodeHintType.TRY_HARDER to true,
            ),
        )
    }
    private var buffer = ByteArray(0)

    override fun analyze(image: ImageProxy) {
        try {
            val plane = image.planes[0]
            val data = plane.buffer
            data.rewind()
            if (buffer.size != data.remaining()) buffer = ByteArray(data.remaining())
            data.get(buffer)
            val width = image.width
            val height = image.height
            val stride = plane.rowStride
            val source = PlanarYUVLuminanceSource(buffer, stride, height, 0, 0, width, height, false)
            val text = try {
                reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
            } catch (_: ReaderException) {
                null
            } finally {
                reader.reset()
            }
            if (text != null && QrCodes.isPairUri(text)) onPairUri(text)
        } finally {
            image.close()
        }
    }
}
