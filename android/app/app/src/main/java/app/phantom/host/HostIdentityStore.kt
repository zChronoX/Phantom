package app.phantom.host

import android.content.Context
import app.phantom.host.core.PairingCodec
import uniffi.phantom_ffi.generateHostIdentity
import uniffi.phantom_ffi.generateP2pSecretKey
import uniffi.phantom_ffi.p2pEndpointIdFromSecret
import java.io.File
import java.io.FileInputStream
import kotlin.random.Random

data class StoredHostIdentity(
    val certDer: ByteArray,
    val keyDer: ByteArray,
    val p2pSecretKey: ByteArray,
    val authToken: ByteArray,
    val certFile: File,
    val keyFile: File,
    val p2pSecretFile: File,
    val authTokenFile: File,
    val fingerprintHex: String,
    val p2pEndpointId: String,
    val authTokenB64: String,
)

/**
 * Conserva l'identità persistente dell'host nella memoria privata dell'app:
 * - certificato TLS + chiave privata per il QUIC diretto in LAN;
 * - chiave segreta Ed25519 (32 byte) per il trasporto P2P integrato (`iroh`) raggiungibile da 5G/4G ovunque;
 * - token di accoppiamento segreto (16 byte) per accettare solo i client accoppiati via QR.
 */
object HostIdentityStore {
    private val lock = Any()

    fun loadOrCreate(context: Context): StoredHostIdentity = synchronized(lock) {
        val dir = File(context.filesDir, "identity").apply { mkdirs() }
        val certFile = File(dir, "host_cert.der")
        val keyFile = File(dir, "host_key.der")
        val p2pFile = File(dir, "p2p_secret.key")
        val authFile = File(dir, "auth.token")

        val tlsPair = if (certFile.isFile && keyFile.isFile) {
            val c = certFile.readBytes()
            val k = keyFile.readBytes()
            if (c.isNotEmpty() && k.isNotEmpty()) c to k else null
        } else {
            null
        } ?: run {
            val fresh = generateHostIdentity()
            writePrivate(certFile, fresh.certDer)
            writePrivate(keyFile, fresh.keyDer)
            fresh.certDer to fresh.keyDer
        }

        val p2pSecret = if (p2pFile.isFile) {
            val raw = p2pFile.readBytes()
            if (raw.size == 32) raw else null
        } else {
            null
        } ?: run {
            val fresh = generateP2pSecretKey()
            writePrivate(p2pFile, fresh)
            fresh
        }

        val authToken = if (authFile.isFile) {
            val raw = authFile.readBytes()
            if (raw.size == 16) raw else null
        } else {
            null
        } ?: run {
            val fresh = randomToken16()
            writePrivate(authFile, fresh)
            fresh
        }

        val p2pId = p2pEndpointIdFromSecret(p2pSecret)
        StoredHostIdentity(
            certDer = tlsPair.first,
            keyDer = tlsPair.second,
            p2pSecretKey = p2pSecret,
            authToken = authToken,
            certFile = certFile,
            keyFile = keyFile,
            p2pSecretFile = p2pFile,
            authTokenFile = authFile,
            fingerprintHex = PairingCodec.sha256FingerprintHex(tlsPair.first),
            p2pEndpointId = p2pId,
            authTokenB64 = PairingCodec.encodeBase64UrlNoPad(authToken),
        )
    }

    fun regenerate(context: Context): StoredHostIdentity = synchronized(lock) {
        val dir = File(context.filesDir, "identity").apply { mkdirs() }
        val certFile = File(dir, "host_cert.der")
        val keyFile = File(dir, "host_key.der")
        val p2pFile = File(dir, "p2p_secret.key")
        val authFile = File(dir, "auth.token")

        val freshTls = generateHostIdentity()
        writePrivate(certFile, freshTls.certDer)
        writePrivate(keyFile, freshTls.keyDer)

        val freshP2p = generateP2pSecretKey()
        writePrivate(p2pFile, freshP2p)

        val freshAuth = randomToken16()
        writePrivate(authFile, freshAuth)

        StoredHostIdentity(
            certDer = freshTls.certDer,
            keyDer = freshTls.keyDer,
            p2pSecretKey = freshP2p,
            authToken = freshAuth,
            certFile = certFile,
            keyFile = keyFile,
            p2pSecretFile = p2pFile,
            authTokenFile = authFile,
            fingerprintHex = PairingCodec.sha256FingerprintHex(freshTls.certDer),
            p2pEndpointId = p2pEndpointIdFromSecret(freshP2p),
            authTokenB64 = PairingCodec.encodeBase64UrlNoPad(freshAuth),
        )
    }

    fun loadFromPaths(certPath: String?, keyPath: String?): Pair<ByteArray, ByteArray>? {
        if (certPath.isNullOrBlank() || keyPath.isNullOrBlank()) return null
        val certFile = File(certPath)
        val keyFile = File(keyPath)
        if (!certFile.isFile || !keyFile.isFile) return null
        val cert = certFile.readBytes()
        val key = keyFile.readBytes()
        if (cert.isEmpty() || key.isEmpty()) return null
        return cert to key
    }

    fun loadP2pSecretFromPath(p2pPath: String?): ByteArray? {
        if (p2pPath.isNullOrBlank()) return null
        val file = File(p2pPath)
        if (!file.isFile) return null
        val bytes = file.readBytes()
        return if (bytes.size == 32) bytes else null
    }

    fun loadAuthTokenFromPath(authPath: String?): ByteArray? {
        if (authPath.isNullOrBlank()) return null
        val file = File(authPath)
        if (!file.isFile) return null
        val bytes = file.readBytes()
        return if (bytes.size == 16) bytes else null
    }

    private fun randomToken16(): ByteArray {
        val out = ByteArray(16)
        try {
            FileInputStream("/dev/urandom").use { stream ->
                var off = 0
                while (off < out.size) {
                    val n = stream.read(out, off, out.size - off)
                    if (n <= 0) break
                    off += n
                }
                if (off == out.size) return out
            }
        } catch (_: Throwable) {
        }
        return Random.nextBytes(16)
    }

    private fun writePrivate(target: File, bytes: ByteArray) {
        val tmp = File(target.parentFile, "${target.name}.tmp")
        tmp.writeBytes(bytes)
        tmp.setReadable(false, false)
        tmp.setReadable(true, true)
        tmp.setWritable(false, false)
        tmp.setWritable(true, true)
        if (!tmp.renameTo(target)) {
            target.writeBytes(bytes)
            tmp.delete()
        }
    }
}
