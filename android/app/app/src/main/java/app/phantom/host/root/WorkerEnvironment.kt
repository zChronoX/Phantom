package app.phantom.host.root

import android.annotation.SuppressLint
import android.app.Application
import android.app.Instrumentation
import android.content.AttributionSource
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import android.os.Build

/**
 * Ambiente minimo da "app finta" per il worker `app_process` (Root/Shizuku), sul modello dei `Workarounds` di scrcpy.
 *
 * Su Android 14+ (e in particolare 15–17) varie API di sistema chiamate dal worker si aspettano un `ActivityThread`
 * corrente, un `Context` di sistema valido e un'applicazione iniziale: senza, `AudioRecord.Builder` va in
 * `NullPointerException` (es. `Context.getDeviceId()` su una base nulla) e `DisplayManager.createVirtualDisplay`
 * non trova un contesto. [apply] va chiamato una sola volta, subito dopo `Looper.prepareMainLooper()`.
 *
 * Tutte le operazioni sono tolleranti: un fallimento viene annotato in [notes] ma non blocca l'avvio.
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi", "SoonBlockedPrivateApi", "BlockedPrivateApi", "NewApi")
object WorkerEnvironment {
    /** Pacchetto con cui il worker si presenta al sistema (come `adb shell` / scrcpy). */
    const val SHELL_PACKAGE = "com.android.shell"
    private const val SHELL_UID = 2000

    private val notesList = mutableListOf<String>()
    val notes: List<String> get() = synchronized(notesList) { notesList.toList() }

    private var activityThreadClass: Class<*>? = null
    private var activityThread: Any? = null
    private var applied = false

    @Volatile
    private var fakeContext: FakeContext? = null

    @Synchronized
    fun apply() {
        if (applied) return
        applied = true
        try {
            val cls = Class.forName("android.app.ActivityThread")
            val ctor = cls.getDeclaredConstructor().apply { isAccessible = true }
            val thread = ctor.newInstance()
            cls.getDeclaredField("sCurrentActivityThread").apply { isAccessible = true }.set(null, thread)
            cls.getDeclaredField("mSystemThread").apply { isAccessible = true }.setBoolean(thread, true)
            activityThreadClass = cls
            activityThread = thread
        } catch (t: Throwable) {
            note("ActivityThread non inizializzato: ${t.javaClass.simpleName}")
            return
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            fillConfigurationController()
        }
        if (!Build.BRAND.equals("ONYX", ignoreCase = true)) {
            fillAppInfo()
        }
        fillAppContext()
    }

    /** Contesto "shell" da usare per `AudioRecord.Builder.setContext` e affini (mai `null`). */
    fun context(): Context {
        fakeContext?.let { return it }
        synchronized(this) {
            fakeContext?.let { return it }
            val created = FakeContext(systemContext())
            fakeContext = created
            return created
        }
    }

    private fun systemContext(): Context? {
        val cls = activityThreadClass ?: return null
        val thread = activityThread ?: return null
        return try {
            cls.getDeclaredMethod("getSystemContext").invoke(thread) as? Context
        } catch (t: Throwable) {
            note("Contesto di sistema non disponibile: ${t.javaClass.simpleName}")
            null
        }
    }

    private fun fillConfigurationController() {
        val cls = activityThreadClass ?: return
        try {
            val controllerCls = Class.forName("android.app.ConfigurationController")
            val internalCls = Class.forName("android.app.ActivityThreadInternal")
            val ctor = controllerCls.getDeclaredConstructor(internalCls).apply { isAccessible = true }
            val controller = ctor.newInstance(activityThread)
            cls.getDeclaredField("mConfigurationController").apply { isAccessible = true }.set(activityThread, controller)
        } catch (t: Throwable) {
            note("ConfigurationController non impostato: ${t.javaClass.simpleName}")
        }
    }

    private fun fillAppInfo() {
        val cls = activityThreadClass ?: return
        try {
            val bindCls = Class.forName("android.app.ActivityThread\$AppBindData")
            val bind = bindCls.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
            val info = ApplicationInfo().apply { packageName = SHELL_PACKAGE }
            bindCls.getDeclaredField("appInfo").apply { isAccessible = true }.set(bind, info)
            cls.getDeclaredField("mBoundApplication").apply { isAccessible = true }.set(activityThread, bind)
        } catch (t: Throwable) {
            note("AppBindData non impostato: ${t.javaClass.simpleName}")
        }
    }

    private fun fillAppContext() {
        val cls = activityThreadClass ?: return
        try {
            val app = Instrumentation.newApplication(Application::class.java, context())
            cls.getDeclaredField("mInitialApplication").apply { isAccessible = true }.set(activityThread, app)
        } catch (t: Throwable) {
            note("Applicazione iniziale non impostata: ${t.javaClass.simpleName}")
        }
    }

    private fun note(message: String) {
        synchronized(notesList) { notesList.add(message) }
    }

    /**
     * `ContextWrapper` sul contesto di sistema che si presenta come `com.android.shell` (uid 2000), come fa scrcpy.
     * `getDeviceId()` restituisce sempre il dispositivo predefinito (0): su Android 14+ `AudioRecord.Builder` lo
     * interroga e, con una base nulla, generava la `NullPointerException` vista su Android 17.
     */
    class FakeContext(base: Context?) : ContextWrapper(base) {
        override fun getPackageName(): String = SHELL_PACKAGE

        override fun getOpPackageName(): String = SHELL_PACKAGE

        override fun getAttributionTag(): String? = null

        @SuppressLint("NewApi")
        override fun getAttributionSource(): AttributionSource =
            AttributionSource.Builder(SHELL_UID).setPackageName(SHELL_PACKAGE).build()

        override fun getDeviceId(): Int = 0

        override fun getApplicationContext(): Context = this

        override fun createPackageContext(packageName: String?, flags: Int): Context = this
    }
}
