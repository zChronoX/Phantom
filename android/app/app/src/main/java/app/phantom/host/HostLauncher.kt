package app.phantom.host

import android.content.Context
import android.content.Intent
import app.phantom.host.root.ShizukuLauncher
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.StreamProfile

/**
 * Avvia l'host con le impostazioni salvate nell'app, senza aprirla: lo usano il riquadro rapido e l'avvio automatico.
 * Root parte sempre, Shizuku solo se il servizio Shizuku è già pronto; la modalità Standard ha bisogno del consenso
 * alla registrazione dello schermo, che si dà solo dall'app.
 */
object HostLauncher {
    enum class Outcome {
        /** Servizio avviato. */
        STARTED,

        /** Modalità Shizuku, ma Shizuku non è (ancora) attivo o non ha dato il permesso. */
        SHIZUKU_NOT_READY,

        /** Modalità Standard: serve il consenso alla registrazione dello schermo dall'app. */
        NEEDS_CONSENT,
    }

    fun savedMode(context: Context): CaptureMode {
        val prefs = context.getSharedPreferences(MainActivity.PREFS_NAME, Context.MODE_PRIVATE)
        val modeName = prefs.getString(MainActivity.KEY_MODE, CaptureMode.ROOT.name)
        return CaptureMode.entries.firstOrNull { it.name == modeName } ?: CaptureMode.ROOT
    }

    fun autostartEnabled(context: Context): Boolean =
        context.getSharedPreferences(MainActivity.PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(MainActivity.KEY_AUTOSTART, false)

    fun startFromPrefs(context: Context): Outcome {
        val prefs = context.getSharedPreferences(MainActivity.PREFS_NAME, Context.MODE_PRIVATE)
        val mode = savedMode(context)
        val profName = prefs.getString(MainActivity.KEY_PROFILE, StreamProfile.P1080_120.name)
        val profile = StreamProfile.entries.firstOrNull { it.name == profName } ?: StreamProfile.P1080_120
        val action = when (mode) {
            CaptureMode.ROOT -> HostForegroundService.ACTION_START_ROOT
            CaptureMode.SHIZUKU -> {
                if (ShizukuLauncher.status() != ShizukuLauncher.Status.READY) return Outcome.SHIZUKU_NOT_READY
                HostForegroundService.ACTION_START_SHIZUKU
            }
            else -> return Outcome.NEEDS_CONSENT
        }
        val intent = Intent(context, HostForegroundService::class.java).apply {
            this.action = action
            putExtra(HostForegroundService.EXTRA_PORT, 47800)
            putExtra(HostForegroundService.EXTRA_PROFILE, profile.name)
            putExtra(HostForegroundService.EXTRA_HEVC, prefs.getBoolean(MainActivity.KEY_HEVC, true))
            putExtra(HostForegroundService.EXTRA_INTRA_REFRESH, prefs.getBoolean(MainActivity.KEY_INTRA_REFRESH, false))
        }
        context.startForegroundService(intent)
        return Outcome.STARTED
    }
}
