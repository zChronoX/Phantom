package app.phantom.host.root

import android.annotation.SuppressLint
import android.os.IBinder
import dalvik.system.PathClassLoader
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Controlla lo stato di alimentazione del display fisico principale (`POWER_MODE_OFF` / `POWER_MODE_NORMAL`)
 * con privilegi root, permettendo lo spegnimento schermo stile `scrcpy --turn-screen-off` durante lo streaming.
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi")
object RootDisplayPower {
    private const val POWER_MODE_OFF = 0
    private const val POWER_MODE_NORMAL = 2
    private val displayTurnedOff = AtomicBoolean(false)

    fun setPhysicalDisplayPower(on: Boolean): Boolean {
        return try {
            val sc = Class.forName("android.view.SurfaceControl")
            val token = resolvePrimaryDisplayToken(sc) ?: return false
            val setPower = sc.getMethod(
                "setDisplayPowerMode",
                IBinder::class.java,
                Int::class.javaPrimitiveType,
            )
            setPower.invoke(null, token, if (on) POWER_MODE_NORMAL else POWER_MODE_OFF)
            displayTurnedOff.set(!on)
            true
        } catch (_: Throwable) {
            false
        }
    }

    /** Riaccende il display fisico solo se questa sessione lo aveva effettivamente spento. */
    fun restoreIfTurnedOff() {
        if (displayTurnedOff.compareAndSet(true, false)) {
            setPhysicalDisplayPower(true)
        }
    }

    private fun resolvePrimaryDisplayToken(surfaceControlCls: Class<*>): IBinder? {
        // Android 14+: DisplayControl in /system/framework/services.jar
        try {
            val loader = PathClassLoader("/system/framework/services.jar", ClassLoader.getSystemClassLoader())
            val dc = Class.forName("com.android.server.display.DisplayControl", true, loader)
            val ids = dc.getMethod("getPhysicalDisplayIds").invoke(null) as? LongArray
            if (ids != null && ids.isNotEmpty()) {
                val token = dc.getMethod("getPhysicalDisplayToken", Long::class.javaPrimitiveType)
                    .invoke(null, ids[0]) as? IBinder
                if (token != null) return token
            }
        } catch (_: Throwable) {
        }

        // Android 10–13: SurfaceControl.getPhysicalDisplayIds / getPhysicalDisplayToken
        try {
            val ids = surfaceControlCls.getMethod("getPhysicalDisplayIds").invoke(null) as? LongArray
            if (ids != null && ids.isNotEmpty()) {
                val token = surfaceControlCls.getMethod("getPhysicalDisplayToken", Long::class.javaPrimitiveType)
                    .invoke(null, ids[0]) as? IBinder
                if (token != null) return token
            }
        } catch (_: Throwable) {
        }

        // Fallback Android 10–12: getInternalDisplayToken()
        try {
            val token = surfaceControlCls.getMethod("getInternalDisplayToken").invoke(null) as? IBinder
            if (token != null) return token
        } catch (_: Throwable) {
        }

        // Fallback Android 8–9: getBuiltInDisplay(0)
        try {
            val token = surfaceControlCls.getMethod("getBuiltInDisplay", Int::class.javaPrimitiveType)
                .invoke(null, 0) as? IBinder
            if (token != null) return token
        } catch (_: Throwable) {
        }

        return null
    }
}
