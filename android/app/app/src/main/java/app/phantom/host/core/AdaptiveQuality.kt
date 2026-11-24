package app.phantom.host.core

/** Un gradino di qualità: lato corto massimo e frequenza. */
data class QualityStep(val maxShortSide: Int, val fps: Int)

/**
 * Sceglie risoluzione e frequenza in base al bitrate che la rete concede. Il controllo del bitrate abbassa solo i
 * bit: su una rete debole un 1080p a 3 Mbit/s diventa un mosaico, mentre un 720p allo stesso bitrate resta nitido.
 *
 * Il criterio sono i **bit per pixel** (bitrate / (pixel × fps)): sotto `lowBpp` per `downHoldMs` si scende di un
 * gradino; si risale quando il gradino superiore avrebbe almeno `highBpp` per `upHoldMs`, e non prima di `minDwellMs`
 * dall'ultimo cambio. Le due soglie distanti e i tempi di attesa evitano di cambiare continuamente: ogni cambio
 * ricrea l'encoder (keyframe e immagine ferma). Nei test 1.4.0, con 3 s e 8 s, si cambiava 720p ↔ 1080p 11 volte
 * in 45 minuti.
 *
 * **Risalite fallite.** Nei test del 24/11 la rete stimata tornava a ~15 Mbit/s dopo un paio di minuti, la qualità
 * risaliva a 1080p e il collegamento cedeva di nuovo entro un minuto: 21 cambi in un'ora, sempre a minuti di distanza,
 * quindi oltre la permanenza minima. Ora, se dopo una salita bisogna riscendere entro `failedClimbWindowMs`, il
 * tentativo successivo verso quel gradino aspetta di più: prima `backoffStartMs`, poi il doppio a ogni nuovo
 * fallimento, fino a `maxUpHoldMs`. Restare sul gradino raggiunto per `stableResetMs` cancella la penalità.
 */
class AdaptiveQuality(
    top: QualityStep,
    /** Lato lungo / lato corto della sorgente (es. 2400 / 1080 = 2,22). */
    private var aspect: Double,
    private val lowBpp: Double = 0.035,
    private val highBpp: Double = 0.08,
    private val downHoldMs: Long = 6_000L,
    private val upHoldMs: Long = 20_000L,
    private val minDwellMs: Long = 30_000L,
    private val failedClimbWindowMs: Long = 120_000L,
    private val backoffStartMs: Long = 120_000L,
    private val maxUpHoldMs: Long = 600_000L,
    private val stableResetMs: Long = 300_000L,
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
    private var lastChangeMs = Long.MIN_VALUE / 2

    /** Attesa con la rete buona prima di salire a ciascun gradino (cresce dopo le risalite fallite). */
    private val climbHoldMs = LongArray(steps.size) { upHoldMs }
    /** Gradino raggiunto con l'ultima salita (-1 se nessuna in corso di verifica) e quando. */
    private var lastClimbTo = -1
    private var lastClimbMs = 0L

    /** Quanto bisognerà aspettare, con la rete buona, prima di salire di un gradino da qui (0 se si è già in cima). */
    fun nextClimbHoldMs(): Long = if (index > 0) climbHoldMs[index - 1] else 0L

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
        if (lastClimbTo == index && nowMs - lastClimbMs >= stableResetMs) {
            // la salita ha tenuto: il gradino torna raggiungibile con l'attesa normale
            climbHoldMs[index] = upHoldMs
            lastClimbTo = -1
        }
        val bpp = bitsPerPixel(bitrateBps, current)
        if (bpp < lowBpp && index < steps.size - 1) {
            if (lowSinceMs < 0) lowSinceMs = nowMs
            highSinceMs = -1L
            if (nowMs - lowSinceMs >= downHoldMs) {
                if (lastClimbTo == index && nowMs - lastClimbMs <= failedClimbWindowMs) {
                    climbHoldMs[index] = (climbHoldMs[index] * 2).coerceAtLeast(backoffStartMs).coerceAtMost(maxUpHoldMs)
                }
                lastClimbTo = -1
                index++
                lowSinceMs = -1L
                lastChangeMs = nowMs
                return current
            }
            return null
        }
        lowSinceMs = -1L
        if (index > 0 && bitsPerPixel(bitrateBps, steps[index - 1]) >= highBpp) {
            if (highSinceMs < 0) highSinceMs = nowMs
            if (nowMs - highSinceMs >= climbHoldMs[index - 1] && nowMs - lastChangeMs >= minDwellMs) {
                index--
                highSinceMs = -1L
                lastChangeMs = nowMs
                lastClimbTo = index
                lastClimbMs = nowMs
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
