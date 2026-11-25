package app.phantom.host.root

import android.annotation.SuppressLint
import android.os.Build
import android.os.IBinder
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Controlla lo stato di alimentazione del display fisico principale (`POWER_MODE_OFF` / `POWER_MODE_NORMAL`)
 * con privilegi root, permettendo lo spegnimento schermo stile `scrcpy --turn-screen-off` durante lo streaming.
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi", "SoonBlockedPrivateApi", "BlockedPrivateApi")
object RootDisplayPower {
    private const val POWER_MODE_OFF = 0
    private const val POWER_MODE_NORMAL = 2
    private val displayTurnedOff = AtomicBoolean(false)

    /** Motivo dell'ultimo fallimento (per il log), vuoto se l'ultima chiamata è riuscita. */
    @Volatile
    var lastError: String = ""
        private set

    /** `DisplayControl` di `system_server`, caricata una volta sola (Android 14+). */
    private val displayControl: Result<Class<*>> by lazy { runCatching { loadDisplayControl() } }

    fun setPhysicalDisplayPower(on: Boolean): Boolean {
        val errors = mutableListOf<String>()
        val mode = if (on) POWER_MODE_NORMAL else POWER_MODE_OFF
        // Come scrcpy: si cambia il power mode di tutti i display fisici con `SurfaceControl.setDisplayPowerMode`.
        // Su Android 14+ i token fisici stanno in `DisplayControl` (services.jar + libandroid_servers).
        val ok = try {
            val sc = Class.forName("android.view.SurfaceControl")
            val tokens = resolvePhysicalDisplayTokens(sc, errors)
            if (tokens.isEmpty()) {
                false
            } else {
                val setPower = sc.getMethod("setDisplayPowerMode", IBinder::class.java, Int::class.javaPrimitiveType)
                tokens.forEach { setPower.invoke(null, it, mode) }
                true
            }
        } catch (t: Throwable) {
            errors += "setDisplayPowerMode: ${describe(t)}"
            false
        }
        // Ripiego Android 15+: `DisplayManagerGlobal.requestDisplayPower`. scrcpy non lo usa più perché su alcune ROM
        // non si comporta come previsto (issue 5530), quindi è solo l'ultima strada.
        val done = ok || (Build.VERSION.SDK_INT >= 35 && requestDisplayPower(on, errors))
        if (done) {
            displayTurnedOff.set(!on)
            lastError = ""
        } else {
            lastError = errors.joinToString(" | ").ifEmpty { "nessun display fisico trovato" }
        }
        return done
    }

    private fun requestDisplayPower(on: Boolean, errors: MutableList<String>): Boolean = try {
        val cls = Class.forName("android.hardware.display.DisplayManagerGlobal")
        val dmg = cls.getMethod("getInstance").invoke(null)
        val res = cls.getMethod("requestDisplayPower", Int::class.javaPrimitiveType, Boolean::class.javaPrimitiveType)
            .invoke(dmg, 0, on) as? Boolean ?: false
        if (!res) errors += "requestDisplayPower: false"
        res
    } catch (t: Throwable) {
        errors += "requestDisplayPower: ${describe(t)}"
        false
    }

    /** `true` se il display fisico è stato spento da Phantom («Schermo Off») e non ancora riacceso. */
    fun isTurnedOff(): Boolean = displayTurnedOff.get()

    /** Riaccende il display fisico solo se questa sessione lo aveva effettivamente spento. */
    fun restoreIfTurnedOff() {
        if (displayTurnedOff.compareAndSet(true, false)) {
            setPhysicalDisplayPower(true)
        }
    }

    /**
     * Carica `com.android.server.display.DisplayControl` con il classpath di `system_server` e la sua libreria nativa,
     * come `wrappers/DisplayControl` di scrcpy. Con un semplice `PathClassLoader` la classe si carica ma i metodi
     * nativi falliscono con `UnsatisfiedLinkError`.
     */
    private fun loadDisplayControl(): Class<*> {
        val factory = Class.forName("com.android.internal.os.ClassLoaderFactory")
        val create = factory.getDeclaredMethod(
            "createClassLoader",
            String::class.java,
            String::class.java,
            String::class.java,
            ClassLoader::class.java,
            Int::class.javaPrimitiveType,
            Boolean::class.javaPrimitiveType,
            String::class.java,
        )
        val classpath = System.getenv("SYSTEMSERVERCLASSPATH") ?: "/system/framework/services.jar"
        val loader = create.invoke(null, classpath, null, null, ClassLoader.getSystemClassLoader(), 0, true, null)
            as ClassLoader
        val dc = loader.loadClass("com.android.server.display.DisplayControl")
        val load = Runtime::class.java.getDeclaredMethod("loadLibrary0", Class::class.java, String::class.java)
        load.isAccessible = true
        load.invoke(Runtime.getRuntime(), dc, "android_servers")
        return dc
    }

    private fun resolvePhysicalDisplayTokens(surfaceControlCls: Class<*>, errors: MutableList<String>): List<IBinder> {
        // Android 10–13: SurfaceControl.getPhysicalDisplayIds / getPhysicalDisplayToken
        val hasScIds = runCatching { surfaceControlCls.getMethod("getPhysicalDisplayIds") }.isSuccess
        if (hasScIds) {
            try {
                val ids = surfaceControlCls.getMethod("getPhysicalDisplayIds").invoke(null) as? LongArray
                val getToken = surfaceControlCls.getMethod("getPhysicalDisplayToken", Long::class.javaPrimitiveType)
                val tokens = (ids ?: LongArray(0)).toList().mapNotNull { getToken.invoke(null, it) as? IBinder }
                if (tokens.isNotEmpty()) return tokens
            } catch (t: Throwable) {
                errors += "SurfaceControl: ${describe(t)}"
            }
        } else if (Build.VERSION.SDK_INT >= 34) {
            // Android 14+: DisplayControl di system_server
            try {
                val dc = displayControl.getOrThrow()
                val ids = dc.getMethod("getPhysicalDisplayIds").invoke(null) as? LongArray
                val getToken = dc.getMethod("getPhysicalDisplayToken", Long::class.javaPrimitiveType)
                val tokens = (ids ?: LongArray(0)).toList().mapNotNull { getToken.invoke(null, it) as? IBinder }
                if (tokens.isNotEmpty()) return tokens
                errors += "DisplayControl: nessun display fisico"
            } catch (t: Throwable) {
                errors += "DisplayControl: ${describe(t)}"
            }
        }

        // Ripiego Android 10–12: getInternalDisplayToken()
        try {
            val token = surfaceControlCls.getMethod("getInternalDisplayToken").invoke(null) as? IBinder
            if (token != null) return listOf(token)
        } catch (_: Throwable) {
        }

        // Ripiego Android 8–9: getBuiltInDisplay(0)
        try {
            val token = surfaceControlCls.getMethod("getBuiltInDisplay", Int::class.javaPrimitiveType)
                .invoke(null, 0) as? IBinder
            if (token != null) return listOf(token)
        } catch (_: Throwable) {
        }

        return emptyList()
    }

    private fun describe(t: Throwable): String {
        val root = generateSequence(t) { it.cause }.last()
        return "${root.javaClass.simpleName}: ${root.message?.replace('\n', ' ') ?: ""}".trim()
    }
}
