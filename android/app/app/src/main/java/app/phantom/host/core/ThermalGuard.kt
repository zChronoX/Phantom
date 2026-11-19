package app.phantom.host.core

enum class ThermalLevel {
    NORMAL,
    WARM,
    HOT,
    CRITICAL,
}

data class ThermalPolicy(
    val level: ThermalLevel,
    val bitrateFactor: Double,
    val shouldStop: Boolean,
    val label: String,
)

/**
 * Protezione termica per evitare surriscaldamento prolungato (es. su Snapdragon 695 a 120 fps).
 *
 * Combina temperatura batteria (decimi di °C da `BatteryManager.EXTRA_TEMPERATURE`) e
 * `PowerManager.currentThermalStatus` (0 = NONE .. 4+ = CRITICAL/EMERGENCY/SHUTDOWN).
 */
object ThermalGuard {
    fun evaluate(batteryTempTenthsC: Int?, androidThermalStatus: Int): ThermalPolicy {
        if ((batteryTempTenthsC != null && batteryTempTenthsC >= 470) || androidThermalStatus >= 4) {
            return ThermalPolicy(ThermalLevel.CRITICAL, 0.0, shouldStop = true, label = "Critico (stop)")
        }
        if ((batteryTempTenthsC != null && batteryTempTenthsC >= 440) || androidThermalStatus == 3) {
            return ThermalPolicy(ThermalLevel.HOT, 0.35, shouldStop = false, label = "Caldo (35%)")
        }
        if ((batteryTempTenthsC != null && batteryTempTenthsC >= 410) || androidThermalStatus == 2) {
            return ThermalPolicy(ThermalLevel.WARM, 0.60, shouldStop = false, label = "Tiepido (60%)")
        }
        return ThermalPolicy(ThermalLevel.NORMAL, 1.0, shouldStop = false, label = "Normale")
    }

    fun cappedBitrate(requestedBps: Long, policy: ThermalPolicy, minBps: Long): Long {
        if (policy.shouldStop || policy.bitrateFactor <= 0.0) return 0L
        val scaled = (requestedBps.toDouble() * policy.bitrateFactor).toLong()
        return minOf(requestedBps, scaled).coerceAtLeast(minBps)
    }
}
