package app.phantom.host.core

/** Un gradino di qualità: lato corto massimo e frequenza. */
data class QualityStep(val maxShortSide: Int, val fps: Int)

/**
 * Sceglie risoluzione e frequenza in base al bitrate che la rete concede. Il controllo del bitrate abbassa solo i
 * bit: su una rete debole un 1080p a 3 Mbit/s diventa un mosaico, mentre un 720p allo stesso bitrate resta nitido.
 *
 * Il criterio sono i **bit per pixel** (bitrate / (pixel × fps)): sotto `lowBpp` per `downHoldMs` si scende di un
 * gradino; si risale quando il gradino superiore avrebbe almeno `highBpp` per `upHoldMs`. Le due soglie distanti e i
 * tempi di attesa evitano di cambiare continuamente (ogni cambio costa un keyframe e ~100 ms di immagine ferma).
 */
class AdaptiveQuality(
    top: QualityStep,
    /** Lato lungo / lato corto della sorgente (es. 2400 / 1080 = 2,22). */
    private var aspect: Double,
    private val lowBpp: Double = 0.035,
    private val highBpp: Double = 0.08,
    private val downHoldMs: Long = 3_000L,
    private val upHoldMs: Long = 8_000L,
) {
    val steps: List<QualityStep> = buildList {
        add(top)
        if (top.maxShortSide > 720) add(QualityStep(720, minOf(top.fps, 60)))
        if (top.maxShortSide > 540) add(QualityStep(540, minOf(top.fps, 30)))
    }

    var index = 0
        private set

    val current: QualityStep get() = steps[index]

    private var lowSinceMs = -1L
    private var highSinceMs = -1L

    /** Frequenza che l'encoder regge davvero (può essere più bassa di quella chiesta, es. 120 → 60). */
    var fpsCap: Int = top.fps

    fun setAspect(longOverShort: Double) {
        if (longOverShort >= 1.0) aspect = longOverShort
    }

    fun bitsPerPixel(bitrateBps: Long, step: QualityStep): Double {
        val short = step.maxShortSide.toDouble()
        val fps = minOf(step.fps, fpsCap).coerceAtLeast(1)
        return bitrateBps / (short * short * aspect * fps)
    }

    /** Nuovo bitrate obiettivo; restituisce il nuovo gradino se bisogna cambiare, altrimenti `null`. */
    fun update(nowMs: Long, bitrateBps: Long): QualityStep? {
        val bpp = bitsPerPixel(bitrateBps, current)
        if (bpp < lowBpp && index < steps.size - 1) {
            if (lowSinceMs < 0) lowSinceMs = nowMs
            highSinceMs = -1L
            if (nowMs - lowSinceMs >= downHoldMs) {
                index++
                lowSinceMs = -1L
                return current
            }
            return null
        }
        lowSinceMs = -1L
        if (index > 0 && bitsPerPixel(bitrateBps, steps[index - 1]) >= highBpp) {
            if (highSinceMs < 0) highSinceMs = nowMs
            if (nowMs - highSinceMs >= upHoldMs) {
                index--
                highSinceMs = -1L
                return current
            }
            return null
        }
        highSinceMs = -1L
        return null
    }
}

/**
 * Bitrate da usare in un giro di controllo: uno per l'encoder e uno per la scelta del gradino.
 *
 * L'encoder riceve il bitrate del controllo di rete limitato dal termico, dal minimo e dal tetto utile della
 * risoluzione **corrente** (oltre ~0,15 bit per pixel l'HEVC non migliora). La qualità adattiva invece deve vedere il
 * bitrate che la rete concede *senza* quel tetto: altrimenti, scesi a 720p, il tetto di 720p (≈10 Mbit/s a 60 fps in
 * HEVC) rapportato a 1080p non supera mai la soglia di risalita e la sessione resta a 720p per sempre.
 */
data class BitratePlan(val encoderBps: Long, val qualityBps: Long) {
    companion object {
        fun compute(networkBps: Long, policy: ThermalPolicy, minBps: Long, usefulMaxBps: Long): BitratePlan {
            val thermal = ThermalGuard.cappedBitrate(networkBps, policy, minBps).coerceAtLeast(minBps)
            return BitratePlan(
                encoderBps = thermal.coerceAtMost(usefulMaxBps).coerceAtLeast(minBps),
                qualityBps = thermal,
            )
        }
    }
}
