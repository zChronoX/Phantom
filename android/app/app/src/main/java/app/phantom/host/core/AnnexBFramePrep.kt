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

    fun alignDown(value: Int, alignment: Int = 16): Int {
        val step = alignment.coerceAtLeast(2)
        return ((value / step) * step).coerceAtLeast(step)
    }

    fun fitWithinBounds(
        width: Int,
        height: Int,
        maxWidth: Int,
        maxHeight: Int,
        alignment: Int = 16,
    ): Pair<Int, Int> {
        val step = alignment.coerceAtLeast(2)
        val capW = alignDown(maxWidth.coerceAtLeast(step), step)
        val capH = alignDown(maxHeight.coerceAtLeast(step), step)
        if (width <= 0 || height <= 0) {
            return capW.coerceAtMost(1088) to capH.coerceAtMost(1920)
        }
        val scale = minOf(1.0, capW.toDouble() / width.toDouble(), capH.toDouble() / height.toDouble())
        val w = alignDown((width * scale).toInt(), step).coerceAtMost(capW)
        val h = alignDown((height * scale).toInt(), step).coerceAtMost(capH)
        return w to h
    }

    /**
     * Riduce la sorgente perché il lato corto non superi `maxShortSide`, **senza arrotondare per eccesso**
     * (il vecchio `align16` portava 1080 a 1088 e stirava l'immagine di 8 px). Restituisce valori pari: l'allineamento
     * vero lo decide poi l'encoder in base alle sue `VideoCapabilities` ([fitEncoderSize]).
     */
    fun scaledDimensions(srcW: Int, srcH: Int, maxShortSide: Int): Pair<Int, Int> {
        if (srcW <= 0 || srcH <= 0 || maxShortSide <= 0) {
            return 1080 to 2400
        }
        val shortSide = minOf(srcW, srcH)
        if (shortSide <= maxShortSide) {
            return alignDown(srcW, 2) to alignDown(srcH, 2)
        }
        val scale = maxShortSide.toDouble() / shortSide.toDouble()
        return alignDown((srcW * scale).toInt(), 2) to alignDown((srcH * scale).toInt(), 2)
    }

    /**
     * Sceglie la risoluzione dell'encoder più grande possibile che:
     * - non supera `width`×`height` (mai ingrandire) né `maxWidth`×`maxHeight`;
     * - ha entrambi i lati multipli di `alignment`;
     * - mantiene le proporzioni: si preferisce una scala esatta (es. 1080×2400 → 864×1920 = 0,8) se costa al massimo
     *   il 3% di risoluzione, altrimenti si arrotondano i due lati per difetto (errore di proporzione < 1%);
     * - è accettata dall'encoder (`isSupported`, es. limite sui macroblocchi totali): se no si riduce a passi del 3%,
     *   non più del 10% alla volta come prima (2400 → 2160 → 1944 → 1744 buttava via risoluzione).
     */
    fun fitEncoderSize(
        width: Int,
        height: Int,
        maxWidth: Int,
        maxHeight: Int,
        alignment: Int,
        isSupported: (Int, Int) -> Boolean = { _, _ -> true },
    ): Pair<Int, Int> {
        val step = alignment.coerceAtLeast(2)
        if (width <= 0 || height <= 0) return alignDown(1080, step) to alignDown(1920, step)
        var scale = minOf(1.0, maxWidth.toDouble() / width, maxHeight.toDouble() / height)
        var best = alignedAtScale(width, height, scale, step)
        var attempts = 0
        while (!isSupported(best.first, best.second) && minOf(best.first, best.second) > 320 && attempts < 40) {
            scale *= 0.97
            best = alignedAtScale(width, height, scale, step)
            attempts++
        }
        return best
    }

    private fun alignedAtScale(width: Int, height: Int, scale: Double, step: Int): Pair<Int, Int> {
        val targetW = width * scale
        // Cerca una larghezza multipla di `step` che dia un'altezza intera e allineata (proporzioni esatte).
        var w = alignDown(targetW.toInt(), step)
        while (w >= step && w >= targetW * 0.97) {
            val num = w.toLong() * height
            if (num % width == 0L) {
                val h = (num / width).toInt()
                if (h % step == 0 && h >= step) return w to h
            }
            w -= step
        }
        return alignDown(targetW.toInt(), step) to alignDown((height * scale).toInt(), step)
    }

    /**
     * Frequenza da chiedere all'encoder. Se il SoC dichiara i suoi `PerformancePoint` (Android 10+) si sceglie la più
     * alta tra 120/90/60/… che sia davvero coperta a questa risoluzione: lo Snapdragon 695 accetta di configurarsi a
     * 120 fps ma regge circa 1080p60, e i frame in più si accumulano dentro l'encoder come latenza.
     * `covers(w, h, fps)` è `null` se il SoC non dichiara nulla: in quel caso si tiene la richiesta.
     */
    fun realisticFps(requestedFps: Int, covers: (Int) -> Boolean?): Int {
        val ladder = listOf(120, 90, 60, 50, 48, 30).filter { it <= requestedFps }.ifEmpty { listOf(requestedFps) }
        if (covers(requestedFps) != false) return requestedFps
        for (fps in ladder) {
            if (covers(fps) == true) return fps
        }
        return minOf(requestedFps, 60)
    }

    /**
     * Istante di cattura sull'orologio dell'host (`elapsedRealtime`, µs) a partire dal `presentationTimeUs` del frame,
     * che con l'ingresso a superficie è sull'orologio monotono (`System.nanoTime`). Così la latenza misurata include
     * anche il tempo passato dentro l'encoder. Se il valore è assurdo (≤ 0, nel futuro o più vecchio di 2 s) si usa
     * l'istante attuale, come prima.
     */
    fun captureTimeUs(presentationTimeUs: Long, nowMonotonicUs: Long, nowElapsedUs: Long): Long {
        if (presentationTimeUs <= 0L) return nowElapsedUs
        val ageUs = nowMonotonicUs - presentationTimeUs
        if (ageUs < 0L || ageUs > 2_000_000L) return nowElapsedUs
        return nowElapsedUs - ageUs
    }

    /**
     * Bitrate oltre il quale la qualità non migliora più a vista: ~0,15 bit per pixel in HEVC, ~0,22 in H.264.
     * Serve a non mandare 30 Mbit/s per un'immagine piccola, che riempiono il Wi-Fi senza alcun vantaggio.
     */
    fun maxUsefulBitrate(width: Int, height: Int, fps: Int, hevc: Boolean): Long {
        val bpp = if (hevc) 0.15 else 0.22
        val pixelsPerSecond = width.toLong() * height.toLong() * fps.coerceAtLeast(1)
        return (pixelsPerSecond * bpp).toLong().coerceAtLeast(1_000_000L)
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
