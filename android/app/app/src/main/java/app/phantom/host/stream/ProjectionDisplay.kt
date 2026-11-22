package app.phantom.host.stream

import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.projection.MediaProjection
import android.view.Surface

/**
 * Display virtuale unico per una `MediaProjection`.
 *
 * Da Android 14 ogni istanza di `MediaProjection` accetta una sola chiamata a `createVirtualDisplay`; una seconda
 * (rotazione, qualità adattiva, nuovo client) lancia `SecurityException`. Qui il display viene creato alla prima
 * [attach] e poi solo ridimensionato e collegato alla nuova superficie dell'encoder. Chiudere l'handle restituito
 * scollega la superficie ma tiene vivo il display; [release] lo distrugge a fine sessione.
 */
class ProjectionDisplay(
    private val projection: MediaProjection,
    private val densityDpi: Int,
) {
    private val lock = Any()
    private var display: VirtualDisplay? = null
    private var released = false

    fun attach(surface: Surface, width: Int, height: Int): AutoCloseable {
        synchronized(lock) {
            check(!released) { "Display di proiezione già rilasciato" }
            val existing = display
            val vd = if (existing == null) {
                projection.createVirtualDisplay(
                    "phantom-host",
                    width,
                    height,
                    densityDpi,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    surface,
                    null,
                    null,
                ) ?: throw IllegalStateException("createVirtualDisplay ha restituito null")
            } else {
                // Prima la dimensione, poi la superficie: evita un frame scalato male sulla nuova superficie.
                existing.resize(width, height, densityDpi)
                existing.surface = surface
                existing
            }
            display = vd
            return AutoCloseable { detach(vd, surface) }
        }
    }

    private fun detach(vd: VirtualDisplay, surface: Surface) {
        synchronized(lock) {
            if (display !== vd || released) return
            try {
                if (vd.surface === surface) vd.surface = null
            } catch (_: Throwable) {
            }
        }
    }

    fun release() {
        synchronized(lock) {
            if (released) return
            released = true
            try {
                display?.release()
            } catch (_: Throwable) {
            }
            display = null
        }
    }
}
