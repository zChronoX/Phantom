package app.phantom.host.core

/**
 * Conserva l'ultimo pacchetto `BUFFER_FLAG_CODEC_CONFIG` (VPS/SPS/PPS) emesso da `MediaCodec`
 * e lo prepone ai keyframe quando il codec lo emette separatamente.
 */
class CodecConfigCache {
    private var configBytes: ByteArray? = null

    fun reset() {
        configBytes = null
    }

    /**
     * Restituisce `null` se il buffer contiene solo parametri del codec (salvandoli in cache);
     * altrimenti restituisce il frame Annex-B pronto per il protocollo Phantom.
     */
    fun prepareFrame(data: ByteArray, isConfig: Boolean, isKeyFrame: Boolean): ByteArray? {
        if (isConfig) {
            configBytes = data.copyOf()
            return null
        }
        if (isKeyFrame) {
            val cfg = configBytes
            if (cfg != null && cfg.isNotEmpty() && !startsWith(data, cfg)) {
                val combined = ByteArray(cfg.size + data.size)
                System.arraycopy(cfg, 0, combined, 0, cfg.size)
                System.arraycopy(data, 0, combined, cfg.size, data.size)
                return combined
            }
        }
        return data
    }

    private fun startsWith(haystack: ByteArray, prefix: ByteArray): Boolean {
        if (haystack.size < prefix.size) return false
        for (i in prefix.indices) {
            if (haystack[i] != prefix[i]) return false
        }
        return true
    }
}

object HostFormatHelpers {
    fun align16(value: Int): Int {
        val clamped = value.coerceAtLeast(16)
        return (((clamped + 8) / 16) * 16).coerceAtLeast(16)
    }

    fun scaledDimensions(srcW: Int, srcH: Int, maxShortSide: Int): Pair<Int, Int> {
        if (srcW <= 0 || srcH <= 0 || maxShortSide <= 0) {
            return 1080 to 2400
        }
        val shortSide = minOf(srcW, srcH)
        if (shortSide <= maxShortSide) {
            return align16(srcW) to align16(srcH)
        }
        val scale = maxShortSide.toDouble() / shortSide.toDouble()
        val w = align16((srcW * scale).toInt())
        val h = align16((srcH * scale).toInt())
        return w to h
    }

    fun toHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xFF
            sb.append(HEX_CHARS[v ushr 4])
            sb.append(HEX_CHARS[v and 0x0F])
        }
        return sb.toString()
    }

    private const val HEX_CHARS = "0123456789abcdef"
}
