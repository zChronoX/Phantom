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
}
