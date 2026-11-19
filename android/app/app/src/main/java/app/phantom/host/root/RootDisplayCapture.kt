package app.phantom.host.root

import android.annotation.SuppressLint
import android.graphics.Rect
import android.os.IBinder
import android.view.Surface
import dalvik.system.PathClassLoader

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

    fun attachVirtualDisplay(surface: Surface, width: Int, height: Int, srcW: Int, srcH: Int): AutoCloseable {
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
        val create = dc.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType)
        val destroy = dc.getMethod("destroyDisplay", IBinder::class.java)
        val token = create.invoke(null, "phantom-host", false) as IBinder
        return token to { t -> destroy.invoke(null, t) }
    }
}
