package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GestureTrackerTest {
    private fun finger(id: Int, x: Int, y: Int) = NormalizedInputFinger(id, x, y, 255)

    @Test
    fun tapProducesSingleShortStroke() {
        val t = GestureTracker()
        assertNull(t.update(listOf(finger(0, 32_768, 32_768)), 1080, 2400, 1_000))
        val g = t.update(emptyList(), 1080, 2400, 1_060)!!
        assertEquals(1, g.size)
        assertEquals(60L, g[0].durationMs)
        assertEquals(539.5f, g[0].points.first().x, 0.6f)
        assertEquals(1199.5f, g[0].points.first().y, 0.6f)
    }

    @Test
    fun swipeKeepsPathAndEndTime() {
        val t = GestureTracker()
        t.update(listOf(finger(0, 32_768, 50_000)), 1080, 2400, 0)
        t.update(listOf(finger(0, 32_768, 40_000)), 1080, 2400, 50)
        t.update(listOf(finger(0, 32_768, 30_000)), 1080, 2400, 100)
        val g = t.update(emptyList(), 1080, 2400, 120)!!
        assertEquals(1, g.size)
        assertEquals(120L, g[0].durationMs)
        assertTrue(g[0].points.first().y > g[0].points.last().y)
    }

    @Test
    fun pinchWithTwoFingersWaitsForBothToLift() {
        val t = GestureTracker()
        t.update(listOf(finger(0, 30_000, 30_000), finger(1, 35_000, 35_000)), 1080, 2400, 0)
        t.update(listOf(finger(0, 20_000, 20_000), finger(1, 45_000, 45_000)), 1080, 2400, 100)
        assertNull("un dito ancora giù", t.update(listOf(finger(1, 46_000, 46_000)), 1080, 2400, 150))
        val g = t.update(emptyList(), 1080, 2400, 200)!!
        assertEquals(2, g.size)
        assertEquals(0L, g[0].startMs)
        assertEquals(150L, g[0].durationMs)
        assertEquals(200L, g[1].durationMs)
    }

    @Test
    fun longDragIsDownsampled() {
        val t = GestureTracker(maxPointsPerStroke = 10)
        for (i in 0..200) {
            t.update(listOf(finger(0, 1_000 + i * 300, 1_000)), 1080, 2400, i * 10L)
        }
        val g = t.update(emptyList(), 1080, 2400, 2_010)!!
        assertEquals(10, g[0].points.size)
        assertEquals(2_010L, g[0].points.last().tMs)
    }
}
