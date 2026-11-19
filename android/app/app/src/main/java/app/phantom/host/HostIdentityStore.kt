package app.phantom.host

import android.content.Context
import app.phantom.host.core.PairingCodec
import uniffi.phantom_ffi.generateHostIdentity
import java.io.File

data class StoredHostIdentity(
    val certDer: ByteArray,
    val keyDer: ByteArray,
    val certFile: File,
    val keyFile: File,
    val fingerprintHex: String,
)

/**
 * Conserva l'identità TLS persistente (certificato DER + chiave privata PKCS#8 DER) nella memoria
 * privata dell'app così il certificato non cambia tra un avvio e l'altro e l'iPhone può riconnettersi
 * anche da remoto (5G/4G) senza mai rifare l'accoppiamento.
 */
object HostIdentityStore {
    private val lock = Any()

    fun loadOrCreate(context: Context): StoredHostIdentity = synchronized(lock) {
        val dir = File(context.filesDir, "identity").apply { mkdirs() }
        val certFile = File(dir, "host_cert.der")
        val keyFile = File(dir, "host_key.der")

        if (certFile.isFile && keyFile.isFile) {
            val cert = certFile.readBytes()
            val key = keyFile.readBytes()
            if (cert.isNotEmpty() && key.isNotEmpty()) {
                return StoredHostIdentity(
                    certDer = cert,
                    keyDer = key,
                    certFile = certFile,
                    keyFile = keyFile,
                    fingerprintHex = PairingCodec.sha256FingerprintHex(cert),
                )
            }
        }
        createAndSave(certFile, keyFile)
    }

    fun regenerate(context: Context): StoredHostIdentity = synchronized(lock) {
        val dir = File(context.filesDir, "identity").apply { mkdirs() }
        val certFile = File(dir, "host_cert.der")
        val keyFile = File(dir, "host_key.der")
        createAndSave(certFile, keyFile)
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

    private fun createAndSave(certFile: File, keyFile: File): StoredHostIdentity {
        val fresh = generateHostIdentity()
        writePrivate(certFile, fresh.certDer)
        writePrivate(keyFile, fresh.keyDer)
        return StoredHostIdentity(
            certDer = fresh.certDer,
            keyDer = fresh.keyDer,
            certFile = certFile,
            keyFile = keyFile,
            fingerprintHex = PairingCodec.sha256FingerprintHex(fresh.certDer),
        )
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
