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

    /**
     * Sceglie l'IPv4 LAN migliore tra le interfacce attive: preferisce Wi-Fi (`wlan*`) ed Ethernet/bridge
     * (`eth*`/`en*`/`br*`, es. LDPlayer in bridge) ed evita NAT interni degli emulatori (`10.0.2.*`) e link-local.
     */
    fun selectPreferredLanIpv4(candidates: List<Pair<String, String>>): String {
        if (candidates.isEmpty()) return ""
        return candidates.minByOrNull { (iface, ip) -> lanCandidateScore(iface, ip) }?.second.orEmpty()
    }

    /**
     * Rileva un eventuale IPv4 VPN/remoto (es. Tailscale `100.64.0.0/10` o WireGuard `tun*`/`wg*`/`tailscale*`)
     * così l'iPhone può raggiungere il telefono da remoto in 5G/4G senza configurazione manuale.
     */
    fun selectPreferredVpnIpv4(candidates: List<Pair<String, String>>): String {
        var genericVpn = ""
        for ((iface, ip) in candidates) {
            if (ip.startsWith("169.254.") || ip.startsWith("127.")) continue
            val lower = iface.lowercase()
            val isVpnIface = lower.startsWith("tun") || lower.startsWith("wg") || lower.startsWith("tailscale")
            if (isTailscaleCgnat(ip)) return ip
            if (isVpnIface && genericVpn.isEmpty()) genericVpn = ip
        }
        return genericVpn
    }

    private fun isTailscaleCgnat(ip: String): Boolean {
        val parts = ip.split('.')
        if (parts.size != 4) return false
        val first = parts[0].toIntOrNull() ?: return false
        val second = parts[1].toIntOrNull() ?: return false
        return first == 100 && second in 64..127
    }

    private fun lanCandidateScore(iface: String, ip: String): Int {
        val lower = iface.lowercase()
        val isLinkLocal = ip.startsWith("169.254.")
        val isEmulatorNat = ip.startsWith("10.0.2.") || ip.startsWith("172.16.1.")
        val isVpn = lower.startsWith("tun") || lower.startsWith("wg") || lower.startsWith("tailscale") || isTailscaleCgnat(ip)
        val isWifi = lower.startsWith("wlan")
        val isEthernet = lower.startsWith("eth") || lower.startsWith("en") || lower.startsWith("br")
        val isCellular = lower.startsWith("rmnet") || lower.startsWith("ccmni") || lower.startsWith("pdp")
        return when {
            isWifi && !isLinkLocal && !isEmulatorNat -> 0
            isEthernet && !isLinkLocal && !isEmulatorNat -> 1
            !isCellular && !isVpn && !isLinkLocal && !isEmulatorNat -> 2
            !isVpn && !isLinkLocal && !isEmulatorNat -> 3
            isEmulatorNat -> 4
            isVpn -> 5
            else -> 6
        }
    }

    private const val HEX_CHARS = "0123456789abcdef"
}
