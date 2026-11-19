package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ThermalGuardTest {
    @Test
    fun normalTemperaturesKeepFullBitrate() {
        val policy = ThermalGuard.evaluate(batteryTempTenthsC = 365, androidThermalStatus = 0)
        assertEquals(ThermalLevel.NORMAL, policy.level)
        assertEquals(1.0, policy.bitrateFactor, 0.0001)
        assertFalse(policy.shouldStop)
        assertEquals(15_000_000L, ThermalGuard.cappedBitrate(15_000_000L, policy, 2_000_000L))
    }

    @Test
    fun warmAndHotReduceBitrateGradually() {
        val warm = ThermalGuard.evaluate(batteryTempTenthsC = 415, androidThermalStatus = 0)
        assertEquals(ThermalLevel.WARM, warm.level)
        assertEquals(6_000_000L, ThermalGuard.cappedBitrate(10_000_000L, warm, 2_000_000L))

        val hotFromStatus = ThermalGuard.evaluate(batteryTempTenthsC = 380, androidThermalStatus = 3)
        assertEquals(ThermalLevel.HOT, hotFromStatus.level)
        assertEquals(3_500_000L, ThermalGuard.cappedBitrate(10_000_000L, hotFromStatus, 2_000_000L))

        // Rispetta il minimo configurato
        assertEquals(2_000_000L, ThermalGuard.cappedBitrate(3_000_000L, hotFromStatus, 2_000_000L))
    }

    @Test
    fun criticalStopsStreamFromBatteryOrAndroidStatus() {
        val byBattery = ThermalGuard.evaluate(batteryTempTenthsC = 470, androidThermalStatus = 0)
        assertEquals(ThermalLevel.CRITICAL, byBattery.level)
        assertTrue(byBattery.shouldStop)
        assertEquals(0L, ThermalGuard.cappedBitrate(10_000_000L, byBattery, 2_000_000L))

        val byStatus = ThermalGuard.evaluate(batteryTempTenthsC = null, androidThermalStatus = 4)
        assertEquals(ThermalLevel.CRITICAL, byStatus.level)
        assertTrue(byStatus.shouldStop)
    }
}
