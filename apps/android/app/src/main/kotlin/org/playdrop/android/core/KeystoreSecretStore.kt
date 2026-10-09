package org.playdrop.android.core

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import org.playdrop.core.PlayDropException
import org.playdrop.core.SecretStore
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Seals the core's identity key and pin store with a non-exportable AES-256-GCM key in the
 * AndroidKeyStore (SR-28). StrongBox is used when the phone has it, the TEE otherwise. The key
 * needs no user authentication and works while the device is locked, so a phone in a pocket
 * still receives (screen-off receive). Layout: [SealFormat].
 */
internal class KeystoreSecretStore : SecretStore {
    private val cipher = object : NonceCipher {
        override fun encrypt(plain: ByteArray): Pair<ByteArray, ByteArray> {
            val c = Cipher.getInstance(TRANSFORMATION)
            // The Keystore insists on choosing the IV itself (randomized encryption).
            c.init(Cipher.ENCRYPT_MODE, key())
            return c.iv to c.doFinal(plain)
        }

        override fun decrypt(nonce: ByteArray, cipherText: ByteArray): ByteArray {
            val c = Cipher.getInstance(TRANSFORMATION)
            c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(SealFormat.TAG_LEN * 8, nonce))
            return c.doFinal(cipherText)
        }
    }

    @Volatile
    private var cached: SecretKey? = null

    /** Encrypts [plain] for the data dir. */
    override fun seal(plain: ByteArray): ByteArray = guarded("seal") { SealFormat.seal(plain, cipher) }

    /** Decrypts bytes the core read from the data dir. */
    override fun open(sealed: ByteArray): ByteArray = guarded("open") { SealFormat.open(sealed, cipher) }

    /** Runs [block], turning any failure into a logged [PlayDropException.Other]. */
    private inline fun guarded(op: String, block: () -> ByteArray): ByteArray = try {
        block()
    } catch (e: Exception) {
        HostLog.w("secrets", "keystore $op failed", e)
        throw PlayDropException.Other("Secure key storage failed (${e.javaClass.simpleName}). " +
            "If this persists, clear PlayDrop's data and pair again.")
    }

    /** Loads the key, creating it on first use. */
    @Synchronized
    private fun key(): SecretKey {
        cached?.let { return it }
        val ks = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        val existing = ks.getKey(ALIAS, null) as? SecretKey
        val key = existing ?: generate()
        cached = key
        return key
    }

    /** Generates the key, preferring StrongBox and falling back to the TEE. */
    private fun generate(): SecretKey {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                return generate(strongBox = true).also { HostLog.i("secrets", "created StrongBox key") }
            } catch (e: StrongBoxUnavailableException) {
                HostLog.i("secrets", "StrongBox unavailable (${e.javaClass.simpleName}); using the TEE")
            }
        }
        return generate(strongBox = false).also { HostLog.i("secrets", "created TEE key") }
    }

    private fun generate(strongBox: Boolean): SecretKey {
        val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(KEY_BITS)
            .setUserAuthenticationRequired(false)
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                    setUnlockedDeviceRequired(false)
                    if (strongBox) setIsStrongBoxBacked(true)
                }
            }
            .build()
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        gen.init(spec)
        return gen.generateKey()
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "playdrop-secrets"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val KEY_BITS = 256
    }
}
