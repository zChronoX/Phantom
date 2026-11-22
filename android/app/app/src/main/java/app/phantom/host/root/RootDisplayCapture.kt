package app.phantom.host.root

import android.annotation.SuppressLint
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.os.Build
import android.os.IBinder
import android.view.Surface
import dalvik.system.PathClassLoader
import java.lang.reflect.InvocationTargetException

/**
 * Legge le dimensioni del display fisico principale e crea un display virtuale specchiato
 * sulla superficie di `VideoEncoder` quando il worker gira con privilegi root (`app_process`).
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi")
object RootDisplayCapture {
    fun readPrimaryDisplaySize(): Pair<Int, Int> {
        try {
            val dmgCls = Class.forName("android.hardware.display.DisplayManagerGlobal")
            val dmg = dmgCls.getMethod("getInstance").invoke(null)
            val info = dmgCls.getMethod("getDisplayInfo", Int::class.javaPrimitiveType).invoke(dmg, 0)
            if (info != null) {
                val w = info.javaClass.getField("logicalWidth").getInt(info)
                val h = info.javaClass.getField("logicalHeight").getInt(info)
                if (w > 0 && h > 0) return w to h
            }
        } catch (_: Throwable) {
        }
        return 1080 to 2400
    }

    /** Notifica (una volta per metodo) quale tecnica di cattura è stata usata; impostata da `RootHostMain`. */
    @Volatile
    var onLog: ((String) -> Unit)? = null

    @Volatile
    private var lastMethod: String? = null

    /**
     * Crea un display virtuale che specchia il display 0 sulla superficie dell'encoder.
     *
     * Ordine delle tecniche (come scrcpy):
     * - Android 14+: `DisplayManager.createVirtualDisplay(name, w, h, displayIdToMirror, surface)` (API nascosta statica,
     *   l'unica che funziona su Android 15–17, dove `SurfaceControl.createDisplay` è sparito e
     *   `DisplayControl.createDisplay(String, boolean)` ha cambiato firma);
     * - Android 10–13: `SurfaceControl.createDisplay` + transazione (percorso storico, già collaudato);
     * - in ogni caso l'altra tecnica resta come ripiego, e `DisplayControl` come ultima spiaggia.
     */
    fun attachVirtualDisplay(surface: Surface, width: Int, height: Int, srcW: Int, srcH: Int): AutoCloseable {
        val strategies: List<Pair<String, () -> AutoCloseable>> = buildList {
            val mirror = "DisplayManager.createVirtualDisplay" to { mirrorWithDisplayManager(surface, width, height) }
            val legacy = "SurfaceControl.createDisplay" to { attachWithSurfaceControl(surface, width, height, srcW, srcH) }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                add(mirror)
                add(legacy)
            } else {
                add(legacy)
                add(mirror)
            }
        }
        val failures = mutableListOf<String>()
        for ((name, attempt) in strategies) {
            try {
                val handle = attempt()
                if (lastMethod != name) {
                    lastMethod = name
                    onLog?.invoke("Cattura display via $name${if (failures.isEmpty()) "" else " (dopo: ${failures.joinToString("; ")})"}")
                }
                return handle
            } catch (t: Throwable) {
                val cause = (t as? InvocationTargetException)?.targetException ?: t
                failures += "$name → ${cause.javaClass.simpleName}${cause.message?.let { ": ${it.take(120)}" } ?: ""}"
            }
        }
        throw IllegalStateException("Nessuna tecnica di cattura display disponibile: ${failures.joinToString(" | ")}")
    }

    private fun mirrorWithDisplayManager(surface: Surface, width: Int, height: Int): AutoCloseable {
        val method = DisplayManager::class.java.getMethod(
            "createVirtualDisplay",
            String::class.java,
            Int::class.javaPrimitiveType,
            Int::class.javaPrimitiveType,
            Int::class.javaPrimitiveType,
            Surface::class.java,
        )
        val display = method.invoke(null, "phantom-host", width, height, 0, surface) as? VirtualDisplay
            ?: throw IllegalStateException("createVirtualDisplay ha restituito null")
        return AutoCloseable {
            try {
                display.release()
            } catch (_: Throwable) {
            }
        }
    }

    private fun attachWithSurfaceControl(surface: Surface, width: Int, height: Int, srcW: Int, srcH: Int): AutoCloseable {
        val sc = Class.forName("android.view.SurfaceControl")
        val (token, destroyFn) = createDisplayToken(sc)

        val openTx = sc.getMethod("openTransaction")
        val closeTx = sc.getMethod("closeTransaction")
        val setSurface = sc.getMethod("setDisplaySurface", IBinder::class.java, Surface::class.java)
        val setProjection = sc.getMethod(
            "setDisplayProjection",
            IBinder::class.java,
            Int::class.javaPrimitiveType,
            Rect::class.java,
            Rect::class.java,
        )
        val setLayerStack = sc.getMethod("setDisplayLayerStack", IBinder::class.java, Int::class.javaPrimitiveType)

        openTx.invoke(null)
        try {
            setSurface.invoke(null, token, surface)
            setProjection.invoke(null, token, 0, Rect(0, 0, srcW, srcH), Rect(0, 0, width, height))
            setLayerStack.invoke(null, token, 0)
        } catch (t: Throwable) {
            try {
                destroyFn(token)
            } catch (_: Throwable) {
            }
            throw t
        } finally {
            closeTx.invoke(null)
        }

        return AutoCloseable {
            try {
                destroyFn(token)
            } catch (_: Throwable) {
            }
        }
    }

    private fun createDisplayToken(surfaceControlCls: Class<*>): Pair<IBinder, (IBinder) -> Unit> {
        // Android 10–13 (e molte ROM 14): SurfaceControl.createDisplay / destroyDisplay
        try {
            val create = surfaceControlCls.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType)
            val destroy = surfaceControlCls.getMethod("destroyDisplay", IBinder::class.java)
            val token = create.invoke(null, "phantom-host", false) as IBinder
            return token to { t -> destroy.invoke(null, t) }
        } catch (_: NoSuchMethodException) {
        }

        // Android 14+: DisplayControl in /system/framework/services.jar
        val loader = PathClassLoader("/system/framework/services.jar", ClassLoader.getSystemClassLoader())
        val dc = Class.forName("com.android.server.display.DisplayControl", true, loader)
        val destroy = dc.getMethod("destroyDisplay", IBinder::class.java)
        val token = try {
            dc.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType)
                .invoke(null, "phantom-host", false) as IBinder
        } catch (_: NoSuchMethodException) {
            // Android 15+: aggiunto `requestedRefreshRate` (float)
            dc.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType, Float::class.javaPrimitiveType)
                .invoke(null, "phantom-host", false, 0f) as IBinder
        }
        return token to { t -> destroy.invoke(null, t) }
    }
}
