package app.phantom.host.core

import java.net.URLDecoder
import java.net.URLEncoder
import java.security.MessageDigest

data class ParsedPairing(
    val name: String,
    val lan: String?,
    val wan: String?,
    val p2pId: String?,
    val authToken: ByteArray?,
    val certDer: ByteArray,
)

/**
 * Codifica e decodifica dell'URI compatto `phantom://pair?...` usato nel codice QR di accoppiamento
 * tra host Android e client iPhone (include sia l'indirizzo LAN sia l'eventuale indirizzo remoto 5G/4G).
 */
object PairingCodec {
    private const val B64_URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"

    fun sha256FingerprintHex(bytes: ByteArray, lengthHex: Int = 16): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
        val full = HostFormatHelpers.toHex(digest)
        return full.take(lengthHex.coerceIn(2, full.length))
    }

    fun encodeBase64UrlNoPad(data: ByteArray): String {
        if (data.isEmpty()) return ""
        val out = StringBuilder(((data.size + 2) / 3) * 4)
        var i = 0
        while (i < data.size) {
            val b0 = data[i].toInt() and 0xFF
            val has1 = i + 1 < data.size
            val has2 = i + 2 < data.size
            val b1 = if (has1) data[i + 1].toInt() and 0xFF else 0
            val b2 = if (has2) data[i + 2].toInt() and 0xFF else 0

            out.append(B64_URL[(b0 ushr 2) and 0x3F])
            out.append(B64_URL[((b0 and 0x03) shl 4) or ((b1 ushr 4) and 0x0F)])
            if (has1) {
                out.append(B64_URL[((b1 and 0x0F) shl 2) or ((b2 ushr 6) and 0x03)])
            }
            if (has2) {
                out.append(B64_URL[b2 and 0x3F])
            }
            i += 3
        }
        return out.toString()
    }

    fun decodeBase64UrlNoPad(raw: String): ByteArray? {
        val cleaned = raw.trim().trimEnd('=')
        if (cleaned.isEmpty()) return ByteArray(0)
        if (cleaned.length % 4 == 1) return null
        val out = ByteArray((cleaned.length * 3) / 4)
        var outIdx = 0
        var i = 0
        while (i < cleaned.length) {
            val c0 = decodeChar(cleaned[i])
            val c1 = decodeChar(cleaned[i + 1])
            val has2 = i + 2 < cleaned.length
            val has3 = i + 3 < cleaned.length
            val c2 = if (has2) decodeChar(cleaned[i + 2]) else 0
            val c3 = if (has3) decodeChar(cleaned[i + 3]) else 0
            if (c0 < 0 || c1 < 0 || c2 < 0 || c3 < 0) return null

            val triple = (c0 shl 18) or (c1 shl 12) or (c2 shl 6) or c3
            out[outIdx++] = ((triple ushr 16) and 0xFF).toByte()
            if (has2 && outIdx < out.size) {
                out[outIdx++] = ((triple ushr 8) and 0xFF).toByte()
            }
            if (has3 && outIdx < out.size) {
                out[outIdx++] = (triple and 0xFF).toByte()
            }
            i += 4
        }
        return if (outIdx == out.size) out else out.copyOf(outIdx)
    }

    fun buildPairingUri(
        name: String,
        lan: String?,
        wan: String?,
        certDer: ByteArray,
        p2pId: String? = null,
        authToken: ByteArray? = null,
    ): String {
        val params = ArrayList<String>(6)
        val cleanName = name.trim().ifEmpty { "Android" }
        params.add("name=${urlEncode(cleanName)}")
        val cleanLan = lan?.trim().orEmpty()
        if (cleanLan.isNotEmpty()) {
            params.add("lan=${urlEncode(cleanLan)}")
        }
        val cleanP2p = p2pId?.trim().orEmpty()
        if (cleanP2p.isNotEmpty()) {
            params.add("p2p=${urlEncode(cleanP2p)}")
        }
        val cleanWan = wan?.trim().orEmpty()
        if (cleanWan.isNotEmpty()) {
            params.add("wan=${urlEncode(cleanWan)}")
        }
        if (authToken != null && authToken.isNotEmpty()) {
            params.add("tok=${encodeBase64UrlNoPad(authToken)}")
        }
        params.add("cert=${encodeBase64UrlNoPad(certDer)}")
        return "phantom://pair?${params.joinToString("&")}"
    }

    fun parsePairingUri(uri: String): ParsedPairing? {
        val trimmed = uri.trim()
        if (!trimmed.startsWith("phantom://pair?")) return null
        val query = trimmed.removePrefix("phantom://pair?")
        val map = LinkedHashMap<String, String>()
        for (pair in query.split('&')) {
            val eq = pair.indexOf('=')
            if (eq <= 0) continue
            val k = pair.substring(0, eq)
            val v = urlDecode(pair.substring(eq + 1))
            map[k] = v
        }
        val certRaw = map["cert"] ?: return null
        val cert = decodeBase64UrlNoPad(certRaw) ?: return null
        if (cert.isEmpty()) return null
        val tok = map["tok"]?.let { decodeBase64UrlNoPad(it) }?.takeIf { it.isNotEmpty() }
        return ParsedPairing(
            name = map["name"]?.trim().orEmpty().ifEmpty { "Android" },
            lan = map["lan"]?.trim()?.takeIf { it.isNotEmpty() },
            wan = map["wan"]?.trim()?.takeIf { it.isNotEmpty() },
            p2pId = map["p2p"]?.trim()?.takeIf { it.isNotEmpty() },
            authToken = tok,
            certDer = cert,
        )
    }

    private fun decodeChar(c: Char): Int = when (c) {
        in 'A'..'Z' -> c.code - 'A'.code
        in 'a'..'z' -> c.code - 'a'.code + 26
        in '0'..'9' -> c.code - '0'.code + 52
        '-', '+' -> 62
        '_', '/' -> 63
        else -> -1
    }

    private fun urlEncode(value: String): String =
        URLEncoder.encode(value, "UTF-8").replace("+", "%20")

    private fun urlDecode(value: String): String =
        URLDecoder.decode(value, "UTF-8")
}
