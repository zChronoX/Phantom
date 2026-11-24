package app.phantom.host

import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import java.util.Locale

/**
 * Quick Settings Tile nativo per avviare/fermare Phantom dalla tendina Android
 * con un solo tocco (in modalità Root; in modalità MediaProjection apre l'app per il consenso).
 */
class PhantomTileService : TileService() {
    private val mainHandler = Handler(Looper.getMainLooper())
    private val stateListener: (HostUiState) -> Unit = { state ->
        mainHandler.post { syncTile(state) }
    }

    override fun onStartListening() {
        super.onStartListening()
        HostStateStore.addListener(stateListener)
    }

    override fun onStopListening() {
        HostStateStore.removeListener(stateListener)
        super.onStopListening()
    }

    override fun onClick() {
        super.onClick()
        val current = HostStateStore.snapshot()
        if (current.running) {
            startService(
                Intent(this, HostForegroundService::class.java)
                    .setAction(HostForegroundService.ACTION_STOP),
            )
            return
        }

        // Root, o Shizuku già pronto: parte con le impostazioni salvate; altrimenti si apre l'app
        if (HostLauncher.startFromPrefs(this) != HostLauncher.Outcome.STARTED) {
            val launch = Intent(this, MainActivity::class.java).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            if (Build.VERSION.SDK_INT >= 34) {
                val pi = PendingIntent.getActivity(
                    this,
                    2,
                    launch,
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
                startActivityAndCollapse(pi)
            } else {
                @Suppress("DEPRECATION")
                startActivityAndCollapse(launch)
            }
        }
    }

    private fun syncTile(state: HostUiState) {
        val tile = qsTile ?: return
        tile.state = if (state.running) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = "Phantom"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            tile.subtitle = when {
                state.connected -> String.format(Locale.US, "%.0f fps", state.fps)
                state.starting -> "Avvio…"
                state.running -> "In ascolto"
                else -> "Spento"
            }
        }
        tile.updateTile()
    }
}
