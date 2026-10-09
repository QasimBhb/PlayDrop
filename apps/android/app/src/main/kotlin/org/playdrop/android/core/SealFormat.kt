package org.playdrop.android.core

/**
 * An authenticated cipher that picks its own nonce on encryption. The Keystore AES-GCM key
 * implements it on the device; unit tests supply a fake.
 */
internal interface NonceCipher {
    /** Encrypts [plain] with a fresh nonce; returns the nonce and the ciphertext with tag. */
    fun encrypt(plain: ByteArray): Pair<ByteArray, ByteArray>

    /** Decrypts and authenticates [cipherText] under [nonce]; throws on a bad tag. */
    fun decrypt(nonce: ByteArray, cipherText: ByteArray): ByteArray
}

/**
 * The byte layout of a sealed secret (SR-28): one version byte, the 12 byte GCM IV, then the
 * ciphertext with its 16 byte tag. The core adds its own `PDSEALED1` header around this.
 */
internal object SealFormat {
    /** Current layout version. */
    const val VERSION: Byte = 1

    /** GCM IV length the Keystore uses for AES-GCM. */
    const val IV_LEN = 12

    /** GCM tag length in bytes. */
    const val TAG_LEN = 16

    /** Seals [plain] with [cipher] into `version || iv || ciphertext+tag`. */
    fun seal(plain: ByteArray, cipher: NonceCipher): ByteArray {
        val (iv, body) = cipher.encrypt(plain)
        require(iv.size == IV_LEN) { "cipher returned a ${iv.size} byte IV, expected $IV_LEN" }
        val out = ByteArray(1 + IV_LEN + body.size)
        out[0] = VERSION
        iv.copyInto(out, 1)
        body.copyInto(out, 1 + IV_LEN)
        return out
    }

    /** Opens bytes produced by [seal]; throws [IllegalArgumentException] on a bad layout. */
    fun open(sealed: ByteArray, cipher: NonceCipher): ByteArray {
        require(sealed.size >= 1 + IV_LEN + TAG_LEN) { "sealed secret is truncated (${sealed.size} bytes)" }
        require(sealed[0] == VERSION) { "unknown sealed secret version ${sealed[0]}" }
        val iv = sealed.copyOfRange(1, 1 + IV_LEN)
        val body = sealed.copyOfRange(1 + IV_LEN, sealed.size)
        return cipher.decrypt(iv, body)
    }
}
