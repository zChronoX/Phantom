package app.phantom.host.root

import android.content.pm.PackageManager
import rikka.shizuku.Shizuku

/**
 * Avvio del worker privilegiato tramite [Shizuku](https://shizuku.rikka.app/) invece di `su`.
 *
 * Shizuku gira con i permessi di ADB (utente `shell`) e si avvia dal telefono con il Debug wireless (Android 11+),
 * senza PC e senza sbloccare il bootloader. Con quei permessi lo stesso `RootHostMain` della modalità Root cattura lo
 * schermo, inietta tocco e tastiera, spegne il display e (Android 12+) cattura l'audio di sistema, come fa scrcpy.
 */
object ShizukuLauncher {
    const val PERMISSION_REQUEST_CODE = 4711

    enum class Status(val label: String) {
        NOT_RUNNING("Shizuku non è in esecuzione"),
        TOO_OLD("Versione di Shizuku troppo vecchia (serve la 11 o successiva)"),
        NO_PERMISSION("Permesso Shizuku da concedere"),
        READY("Shizuku pronto"),
    }

    fun status(): Status {
        val alive = try {
            Shizuku.pingBinder()
        } catch (_: Throwable) {
            false
        }
        if (!alive) return Status.NOT_RUNNING
        return try {
            when {
                Shizuku.isPreV11() -> Status.TOO_OLD
                Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED -> Status.READY
                else -> Status.NO_PERMISSION
            }
        } catch (_: Throwable) {
            Status.NOT_RUNNING
        }
    }

    /** Chiede il permesso a Shizuku; `onResult(true)` se l'utente lo concede. */
    fun requestPermission(onResult: (Boolean) -> Unit) {
        val listener = object : Shizuku.OnRequestPermissionResultListener {
            override fun onRequestPermissionResult(requestCode: Int, grantResult: Int) {
                if (requestCode != PERMISSION_REQUEST_CODE) return
                try {
                    Shizuku.removeRequestPermissionResultListener(this)
                } catch (_: Throwable) {
                }
                onResult(grantResult == PackageManager.PERMISSION_GRANTED)
            }
        }
        try {
            Shizuku.addRequestPermissionResultListener(listener)
            Shizuku.requestPermission(PERMISSION_REQUEST_CODE)
        } catch (_: Throwable) {
            onResult(false)
        }
    }

    /**
     * Esegue `sh -c command` come utente `shell` tramite Shizuku. `newProcess` è nascosto nelle API recenti (resta
     * per chi migra da `su`, come qui), quindi si chiama per riflessione; con l'API 14 servirà una `UserService`.
     */
    fun startProcess(command: String): Process {
        val method = Shizuku::class.java.getDeclaredMethod(
            "newProcess",
            Array<String>::class.java,
            Array<String>::class.java,
            String::class.java,
        )
        method.isAccessible = true
        return method.invoke(null, arrayOf("sh", "-c", command), null, null) as Process
    }
}
