package org.playdrop.android.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/** Round trip and layout checks of the Keystore seal format with a fake cipher. */
class SealFormatTest {
    /** XORs with a key byte and appends a 16 byte "tag" made of the nonce's first byte. */
    private class FakeCipher : NonceCipher {
        var counter = 0

        override fun encrypt(plain: ByteArray): Pair<ByteArray, ByteArray> {
            val iv = ByteArray(SealFormat.IV_LEN) { (counter + it).toByte() }
            counter++
            val body = plain.map { (it.toInt() xor 0x5A).toByte() }.toByteArray() + ByteArray(SealFormat.TAG_LEN) { iv[0] }
            return iv to body
        }

        override fun decrypt(nonce: ByteArray, cipherText: ByteArray): ByteArray {
            val tag = cipherText.takeLast(SealFormat.TAG_LEN)
            require(tag.all { it == nonce[0] }) { "bad tag" }
            return cipherText.dropLast(SealFormat.TAG_LEN).map { (it.toInt() xor 0x5A).toByte() }.toByteArray()
        }
    }

    @Test
    fun roundTrip() {
        val cipher = FakeCipher()
        val plain = "identity key bytes".toByteArray()
        val sealed = SealFormat.seal(plain, cipher)
        assertArrayEquals(plain, SealFormat.open(sealed, cipher))
    }

    @Test
    fun layoutIsVersionIvThenBody() {
        val cipher = FakeCipher()
        val sealed = SealFormat.seal(byteArrayOf(1, 2, 3), cipher)
        assertEquals(1 + SealFormat.IV_LEN + 3 + SealFormat.TAG_LEN, sealed.size)
        assertEquals(SealFormat.VERSION, sealed[0])
        assertArrayEquals(ByteArray(SealFormat.IV_LEN) { it.toByte() }, sealed.copyOfRange(1, 1 + SealFormat.IV_LEN))
    }

    @Test
    fun emptyPlaintextRoundTrips() {
        val cipher = FakeCipher()
        assertArrayEquals(ByteArray(0), SealFormat.open(SealFormat.seal(ByteArray(0), cipher), cipher))
    }

    @Test
    fun unknownVersionIsRefused() {
        val cipher = FakeCipher()
        val sealed = SealFormat.seal(byteArrayOf(9), cipher)
        sealed[0] = 2
        assertThrows(IllegalArgumentException::class.java) { SealFormat.open(sealed, cipher) }
    }

    @Test
    fun truncatedInputIsRefused() {
        val cipher = FakeCipher()
        assertThrows(IllegalArgumentException::class.java) { SealFormat.open(ByteArray(10) { 1 }, cipher) }
    }

    @Test
    fun tamperedIvFailsAuthentication() {
        val cipher = FakeCipher()
        val sealed = SealFormat.seal(byteArrayOf(4, 5, 6), cipher)
        sealed[1] = (sealed[1] + 1).toByte()
        assertThrows(IllegalArgumentException::class.java) { SealFormat.open(sealed, cipher) }
    }
}
