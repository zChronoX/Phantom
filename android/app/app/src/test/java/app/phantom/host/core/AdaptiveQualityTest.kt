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
        // 3 Mbit/s a 1080p60: 0,019 bit per pixel → dopo 3 s si scende a 720p
        assertNull(q.update(0, 3_000_000))
        assertNull(q.update(2_900, 3_000_000))
        assertEquals(QualityStep(720, 60), q.update(3_000, 3_000_000))
        // 6 Mbit/s: a 720p va bene (0,087) ma a 1080p sarebbe 0,039 < 0,08 → resta a 720p
        for (t in 3_000..30_000L step 500) assertNull(q.update(t, 6_000_000))
        assertEquals(1, q.index)
        // 14 Mbit/s: 1080p avrebbe 0,09 ≥ 0,08 → dopo 8 s si risale
        assertNull(q.update(40_000, 14_000_000))
        assertNull(q.update(47_900, 14_000_000))
        assertEquals(QualityStep(1080, 60), q.update(48_000, 14_000_000))
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
        assertEquals(QualityStep(720, 60), run { old.update(0, 2_000_000); old.update(3_000, 2_000_000) })
        for (t in 3_000..60_000L step 250) assertNull(old.update(t, plan.encoderBps))
        assertEquals(1, old.index)

        // il nuovo: dopo 8 s si torna a 1080p
        val q = redmi()
        q.update(0, 2_000_000)
        assertEquals(QualityStep(720, 60), q.update(3_000, 2_000_000))
        var climbed: QualityStep? = null
        for (t in 3_250..20_000L step 250) climbed = climbed ?: q.update(t, plan.qualityBps)
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
}
