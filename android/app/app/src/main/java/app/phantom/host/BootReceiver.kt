package app.phantom.host

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.SystemClock

/**
 * Avvio automatico dell'host all'accensione del telefono e dopo un aggiornamento dell'app (se l'opzione è attiva).
 *
 * - **Root:** parte subito.
 * - **Shizuku:** si aspetta fino a ~45 s che il servizio Shizuku sia pronto (con il root parte da solo all'avvio;
 *   con il Debug wireless va riavviato a mano). Se non arriva, una notifica invita ad aprire Phantom.
 * - **Standard:** il consenso alla registrazione dello schermo si dà solo dall'app: una notifica la apre.
 *
 * Il servizio usa il tipo `specialUse` in Root e Shizuku, che Android consente di avviare da `BOOT_COMPLETED`
 * (Android 15 lo vieta solo per tipi come `mediaProjection`).
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = intent.action
        if (action != Intent.ACTION_BOOT_COMPLETED && action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        if (!HostLauncher.autostartEnabled(context)) return
        if (HostStateStore.snapshot().running) return
        val app = context.applicationContext
        val reason = if (action == Intent.ACTION_BOOT_COMPLETED) "accensione" else "aggiornamento dell'app"
        val pending = goAsync()
        Thread({
            try {
                val deadline = SystemClock.elapsedRealtime() + SHIZUKU_WAIT_MS
                var outcome = HostLauncher.startFromPrefs(app)
                while (outcome == HostLauncher.Outcome.SHIZUKU_NOT_READY && SystemClock.elapsedRealtime() < deadline) {
                    Thread.sleep(3_000)
                    outcome = HostLauncher.startFromPrefs(app)
                }
                when (outcome) {
                    HostLauncher.Outcome.STARTED -> HostStateStore.addLog("Avvio automatico dopo $reason")
                    HostLauncher.Outcome.SHIZUKU_NOT_READY ->
                        notifyManualStart(app, "Shizuku non è attivo: avvialo, poi tocca qui per far partire Phantom")
                    HostLauncher.Outcome.NEEDS_CONSENT ->
                        notifyManualStart(app, "Modalità Standard: tocca per dare il consenso e avviare l'host")
                }
            } catch (_: Throwable) {
                // l'avvio automatico non deve mai far crashare l'app all'accensione
            } finally {
                pending.finish()
            }
        }, "phantom-autostart").start()
    }

    private fun notifyManualStart(context: Context, text: String) {
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Avvio automatico", NotificationManager.IMPORTANCE_DEFAULT).apply {
                description = "Avvisa quando l'host non può partire da solo all'accensione"
            },
        )
        val open = PendingIntent.getActivity(
            context,
            3,
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = Notification.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_phantom_tile)
            .setContentTitle("Phantom non è partito da solo")
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        try {
            nm.notify(NOTIFICATION_ID, notification)
        } catch (_: SecurityException) {
            // notifiche non consentite: resta solo il riquadro rapido
        }
    }

    private companion object {
        const val CHANNEL_ID = "phantom_autostart"
        const val NOTIFICATION_ID = 4710
        const val SHIZUKU_WAIT_MS = 45_000L
    }
}
