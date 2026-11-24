package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AdaptiveQualityTest {
    private fun redmi() = AdaptiveQuality(QualityStep(1080, 60), aspect = 2400.0 / 1080.0)

    @Test
    fun ladderForFullHdProfile() {
        val q = redmi()
        assertEquals(listOf(QualityStep(1080, 60), QualityStep(720, 60), QualityStep(540, 30)), q.steps)
        assertEquals(listOf(QualityStep(720, 60), QualityStep(540, 30)), AdaptiveQuality(QualityStep(720, 60), 2.2).steps)
    }

    @Test
    fun staysAtFullHdWithGoodBitrate() {
        val q = redmi()
        for (t in 0..20_000L step 250) assertNull(q.update(t, 12_000_000))
        assertEquals(0, q.index)
    }

    @Test
    fun stepsDownAfterHoldAndBackUpWithHysteresis() {
        val q = redmi()
        // 3 Mbit/s a 1080p60: 0,019 bit per pixel → dopo 6 s si scende a 720p
        assertNull(q.update(0, 3_000_000))
        assertNull(q.update(5_900, 3_000_000))
        assertEquals(QualityStep(720, 60), q.update(6_000, 3_000_000))
        // 6 Mbit/s: a 720p va bene (0,087) ma a 1080p sarebbe 0,039 < 0,08 → resta a 720p
        for (t in 6_000..30_000L step 500) assertNull(q.update(t, 6_000_000))
        assertEquals(1, q.index)
        // 14 Mbit/s: 1080p avrebbe 0,09 ≥ 0,08 → dopo 20 s si risale
        assertNull(q.update(40_000, 14_000_000))
        assertNull(q.update(59_900, 14_000_000))
        assertEquals(QualityStep(1080, 60), q.update(60_000, 14_000_000))
    }

    @Test
    fun briefDipDoesNotChangeQuality() {
        val q = redmi()
        assertNull(q.update(0, 2_000_000))
        assertNull(q.update(2_000, 2_000_000))
        assertNull(q.update(2_500, 12_000_000))
        assertNull(q.update(6_000, 2_000_000))
        assertEquals(0, q.index)
    }

    @Test
    fun fpsCapRaisesBitsPerPixel() {
        val q = AdaptiveQuality(QualityStep(1080, 120), aspect = 2400.0 / 1080.0)
        val at120 = q.bitsPerPixel(10_000_000, q.current)
        q.fpsCap = 60
        assertEquals(at120 * 2, q.bitsPerPixel(10_000_000, q.current), 1e-9)
    }

    /**
     * Regressione 1.3.2: a 720p60 HEVC il tetto utile è ~10,4 Mbit/s, che rapportato a 1080p fa ~0,067 bpp < 0,08.
     * Se la qualità adattiva vede il bitrate già tagliato non risale mai; con quello della rete sì.
     */
    @Test
    fun climbsBackToFullHdWhenNetworkAllowsIt() {
        val normal = ThermalGuard.evaluate(null, 0)
        val usefulMax720 = HostFormatHelpers.maxUsefulBitrate(720, 1600, 60, hevc = true)
        val plan = BitratePlan.compute(20_000_000, normal, 3_000_000, usefulMax720)
        assertEquals(usefulMax720, plan.encoderBps)
        assertEquals(20_000_000L, plan.qualityBps)

        // il vecchio comportamento: con il bitrate tagliato si resta a 720p
        val old = redmi()
        assertEquals(QualityStep(720, 60), run { old.update(0, 2_000_000); old.update(6_000, 2_000_000) })
        for (t in 6_000..90_000L step 250) assertNull(old.update(t, plan.encoderBps))
        assertEquals(1, old.index)

        // il nuovo: dopo 8 s si torna a 1080p
        val q = redmi()
        q.update(0, 2_000_000)
        assertEquals(QualityStep(720, 60), q.update(6_000, 2_000_000))
        var climbed: QualityStep? = null
        for (t in 6_250..60_000L step 250) climbed = climbed ?: q.update(t, plan.qualityBps)
        assertEquals(QualityStep(1080, 60), climbed)
    }

    @Test
    fun bitratePlanRespectsThermalAndMinimum() {
        val hot = ThermalGuard.evaluate(null, 3) // severo → 35%
        val plan = BitratePlan.compute(20_000_000, hot, 3_000_000, 30_000_000)
        assertEquals(7_000_000L, plan.qualityBps)
        assertEquals(7_000_000L, plan.encoderBps)
        val low = BitratePlan.compute(1_000_000, ThermalGuard.evaluate(null, 0), 3_000_000, 2_000_000)
        assertEquals(3_000_000L, low.encoderBps)
        assertEquals(3_000_000L, low.qualityBps)
    }

    /**
     * Regressione 1.4.0: con la rete che oscilla, la qualità cambiava 720p ↔ 1080p ogni pochi minuti. Dopo una discesa
     * si risale solo con 20 s buoni di fila e almeno 30 s dopo l'ultimo cambio.
     */
    @Test
    fun staysOnAStepAtLeastTheMinimumDwell() {
        val q = redmi()
        q.update(0, 3_000_000)
        assertEquals(QualityStep(720, 60), q.update(6_000, 3_000_000))
        // subito dopo la rete torna ottima: 20 s buoni arrivano a 26 s, ma la permanenza minima scade a 36 s
        for (t in 6_250..35_750L step 250) assertNull(q.update(t, 14_000_000))
        assertEquals(QualityStep(1080, 60), q.update(36_000, 14_000_000))
        // un calo breve (meno di 6 s) non fa scendere
        for (t in 36_250..41_750L step 250) assertNull(q.update(t, 2_000_000))
        assertNull(q.update(42_000, 14_000_000))
        assertEquals(0, q.index)
    }

    /** Fa girare `update` ogni 250 ms da `from` a `to` compresi con lo stesso bitrate; restituisce i cambi. */
    private fun run(q: AdaptiveQuality, from: Long, to: Long, bps: Long): List<Pair<Long, QualityStep>> =
        (from..to step 250).mapNotNull { t -> q.update(t, bps)?.let { t to it } }

    /**
     * Regressione dei test del 24/11: la rete torna «ottima» dopo pochi minuti, si risale a 1080p e dopo meno di un
     * minuto si riscende. Ogni risalita fallita raddoppia l'attesa della successiva (2, 4, 8 minuti, massimo 10).
     */
    @Test
    fun failedClimbsWaitLongerEachTime() {
        val q = redmi()
        assertEquals(listOf(6_000L to QualityStep(720, 60)), run(q, 0, 6_000, 3_000_000))
        assertEquals(20_000L, q.nextClimbHoldMs())
        // primo tentativo con l'attesa normale (permanenza minima: 36 s)
        assertEquals(listOf(36_000L to QualityStep(1080, 60)), run(q, 6_250, 36_000, 14_000_000))
        // il collegamento cede dopo 30 s: si riscende e la prossima salita aspetta 2 minuti
        assertEquals(listOf(72_250L to QualityStep(720, 60)), run(q, 66_250, 72_250, 3_000_000))
        assertEquals(120_000L, q.nextClimbHoldMs())
        assertEquals(listOf(192_500L to QualityStep(1080, 60)), run(q, 72_500, 200_000, 14_000_000))
        // cede di nuovo: 4 minuti
        assertEquals(listOf(206_250L to QualityStep(720, 60)), run(q, 200_250, 206_250, 3_000_000))
        assertEquals(240_000L, q.nextClimbHoldMs())
        assertEquals(listOf(446_500L to QualityStep(1080, 60)), run(q, 206_500, 450_000, 14_000_000))
        // terzo fallimento: 8 minuti; il quarto arriva al massimo di 10
        run(q, 450_250, 456_250, 3_000_000)
        assertEquals(480_000L, q.nextClimbHoldMs())
        run(q, 456_500, 936_500, 14_000_000)
        assertEquals(0, q.index)
        run(q, 936_750, 942_750, 3_000_000)
        assertEquals(600_000L, q.nextClimbHoldMs())
    }

    /** Una salita che tiene per 5 minuti cancella la penalità; una discesa dopo più di 2 minuti non la aumenta. */
    @Test
    fun stableClimbResetsTheBackoff() {
        val q = redmi()
        run(q, 0, 6_000, 3_000_000)
        run(q, 6_250, 36_000, 14_000_000)
        run(q, 36_250, 42_250, 3_000_000)
        assertEquals(120_000L, q.nextClimbHoldMs())
        assertEquals(listOf(162_500L to QualityStep(1080, 60)), run(q, 42_500, 162_500, 14_000_000))
        // tiene 5 minuti, poi la rete peggiora: discesa «normale», si risale con l'attesa normale
        run(q, 162_750, 470_000, 14_000_000)
        assertEquals(1, run(q, 470_250, 476_250, 3_000_000).size)
        assertEquals(20_000L, q.nextClimbHoldMs())
    }
}
