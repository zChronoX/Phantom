package app.phantom.host

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.BatteryManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.view.WindowManager
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.ThermalGuard
import app.phantom.host.core.ThermalPolicy
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.HostRunConfig
import app.phantom.host.stream.PhantomHostSession
import app.phantom.host.stream.StreamProfile
import app.phantom.host.stream.SurfaceCaptureFactory
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.Inet4Address
import java.net.NetworkInterface
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * Servizio in primo piano (stile FKM) visibile nella barra delle notifiche durante tutta la sessione.
 * In modalità Root esegue `app_process` solo finché lo stream è attivo; in modalità Standard usa `MediaProjection`.
 */
class HostForegroundService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null
    private val currentThermalPolicy = AtomicReference(ThermalGuard.evaluate(null, 0))
    private var batteryTempTenths: Int? = null

    private var workerThread: Thread? = null
    private var rootProcess: Process? = null
    private var rootStdin: BufferedWriter? = null
    private var projectionSession: PhantomHostSession? = null
    private var mediaProjection: MediaProjection? = null

    private val batteryReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent == null) return
            val temp = intent.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE)
            batteryTempTenths = if (temp != Int.MIN_VALUE) temp else null
            refreshThermalPolicy()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        ensureChannel()
        registerReceiver(batteryReceiver, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        refreshThermalPolicy()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                HostStateStore.addLog("Arresto richiesto")
                stopAllWork()
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_START_ROOT -> {
                val cfg = readConfig(intent, CaptureMode.ROOT)
                stopAllWork()
                beginForeground(useProjectionType = false)
                startRootMode(cfg)
            }
            ACTION_START_PROJECTION -> {
                val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, Activity.RESULT_CANCELED)
                @Suppress("DEPRECATION")
                val resultData: Intent? = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
                } else {
                    intent.getParcelableExtra(EXTRA_RESULT_DATA)
                }
                if (resultCode != Activity.RESULT_OK || resultData == null) {
                    HostStateStore.addLog("Permesso MediaProjection annullato")
                    stopSelf()
                    return START_NOT_STICKY
                }
                val cfg = readConfig(intent, CaptureMode.MEDIA_PROJECTION)
                stopAllWork()
                beginForeground(useProjectionType = true)
                startProjectionMode(cfg, resultCode, resultData)
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        stopAllWork()
        try {
            unregisterReceiver(batteryReceiver)
        } catch (_: Throwable) {
        }
        super.onDestroy()
    }

    private fun readConfig(intent: Intent, mode: CaptureMode): HostRunConfig {
        val port = intent.getIntExtra(EXTRA_PORT, 47800)
        val profileName = intent.getStringExtra(EXTRA_PROFILE) ?: StreamProfile.P1080_120.name
        val profile = StreamProfile.entries.firstOrNull { it.name == profileName } ?: StreamProfile.P1080_120
        val preferHevc = intent.getBooleanExtra(EXTRA_HEVC, true)
        return HostRunConfig(port = port, profile = profile, preferHevc = preferHevc, mode = mode)
    }

    private fun beginForeground(useProjectionType: Boolean) {
        val notification = buildNotification("Avvio host QUIC…")
        if (Build.VERSION.SDK_INT >= 34) {
            val type = if (useProjectionType) {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            } else {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
            }
            startForeground(NOTIFICATION_ID, notification, type)
        } else if (Build.VERSION.SDK_INT >= 29 && useProjectionType) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        acquireWakeLock()
    }

    private fun startRootMode(config: HostRunConfig) {
        val ip = localWifiIpv4()
        HostStateStore.update {
            it.copy(
                running = true,
                starting = true,
                connected = false,
                mode = CaptureMode.ROOT,
                profile = config.profile,
                preferHevc = config.preferHevc,
                statusText = "Richiesta privilegi root…",
                ipAddress = ip,
                port = config.port,
                certificateHex = "",
            )
        }
        HostStateStore.addLog("Avvio Root · ${config.profile.label}")

        val thread = Thread({
            var proc: Process? = null
            try {
                val apkPath = applicationInfo.sourceDir
                val libDir = applicationInfo.nativeLibraryDir
                val ifacesSummary = describeActiveIpv4Interfaces()
                if (ifacesSummary.isNotBlank()) {
                    HostStateStore.addLog("Interfacce IPv4: $ifacesSummary")
                }
                val cmd = "export CLASSPATH=${shellQuote(apkPath)} LD_LIBRARY_PATH=${shellQuote(libDir)}:/system/lib64:/vendor/lib64; " +
                    "APP_PROC=/system/bin/app_process64; [ -x \"\$APP_PROC\" ] || APP_PROC=app_process; " +
                    "exec \"\$APP_PROC\" " +
                    "-Djna.library.path=${shellQuote(libDir)} " +
                    "-Djna.boot.library.path=${shellQuote(libDir)} " +
                    "-Djna.nounpack=true " +
                    "/system/bin --nice-name=phantom-host app.phantom.host.root.RootHostMain " +
                    "${config.port} ${config.profile.name} ${config.preferHevc} ${shellQuote(libDir)}"

                val pb = ProcessBuilder("su", "-c", cmd).redirectErrorStream(true)
                proc = pb.start()
                rootProcess = proc
                rootStdin = BufferedWriter(OutputStreamWriter(proc.outputStream)).also { writer ->
                    val temp = batteryTempTenths?.toString() ?: "none"
                    val status = readAndroidThermalStatus()
                    writer.write("THERMAL|$temp|$status\n")
                    writer.flush()
                }

                val reader = BufferedReader(InputStreamReader(proc.inputStream))
                while (true) {
                    val line = reader.readLine() ?: break
                    handleRootWorkerLine(line)
                }
                val code = proc.waitFor()
                if (HostStateStore.snapshot().running) {
                    HostStateStore.addLog("Worker root terminato (exit $code)")
                }
            } catch (t: Throwable) {
                HostStateStore.addLog("Errore avvio root: ${t.message}")
            } finally {
                rootStdin = null
                rootProcess = null
                proc?.destroy()
                markStopped("Host fermo")
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }, "phantom-su-supervisor")
        workerThread = thread
        thread.start()
    }

    private fun handleRootWorkerLine(line: String) {
        when {
            line.startsWith("READY|") -> {
                val parts = line.split('|', limit = 3)
                val boundPort = parts.getOrNull(1)?.toIntOrNull() ?: 47800
                val cert = parts.getOrNull(2).orEmpty()
                val ip = localWifiIpv4()
                HostStateStore.update {
                    it.copy(
                        running = true,
                        starting = false,
                        ipAddress = ip,
                        port = boundPort,
                        certificateHex = cert,
                        statusText = "In ascolto su ${formatAddr(ip, boundPort)}",
                    )
                }
                updateNotification("In ascolto su ${formatAddr(ip, boundPort)}")
            }
            line.startsWith("STATS|") -> {
                val p = line.split('|', limit = 7)
                val connected = p.getOrNull(1) == "1"
                val fps = p.getOrNull(2)?.toDoubleOrNull() ?: 0.0
                val mbps = p.getOrNull(3)?.toDoubleOrNull() ?: 0.0
                val touches = p.getOrNull(4)?.toLongOrNull() ?: 0L
                val keys = p.getOrNull(5)?.toLongOrNull() ?: 0L
                val prof = p.getOrNull(6).orEmpty()
                val cur = HostStateStore.snapshot()
                val status = if (connected) {
                    String.format(java.util.Locale.US, "Connesso · %.0f fps · %.1f Mbps", fps, mbps)
                } else {
                    "In ascolto su ${formatAddr(cur.ipAddress, cur.port)}"
                }
                HostStateStore.update {
                    it.copy(
                        connected = connected,
                        fps = fps,
                        mbps = mbps,
                        touches = touches,
                        keys = keys,
                        activeProfileText = prof,
                        statusText = status,
                    )
                }
                updateNotification(status)
            }
            line.startsWith("LOG|") -> {
                HostStateStore.addLog(line.removePrefix("LOG|"))
            }
            line == "THERMAL_STOP" -> {
                HostStateStore.addLog("Stop automatico per protezione termica")
            }
            line == "STOPPED" -> {
                HostStateStore.addLog("Worker root chiuso in modo pulito")
            }
            else -> {
                if (line.isNotBlank()) HostStateStore.addLog("root: $line")
            }
        }
    }

    private fun startProjectionMode(config: HostRunConfig, resultCode: Int, resultData: Intent) {
        val ip = localWifiIpv4()
        val ifacesSummary = describeActiveIpv4Interfaces()
        if (ifacesSummary.isNotBlank()) {
            HostStateStore.addLog("Interfacce IPv4: $ifacesSummary")
        }
        HostStateStore.update {
            it.copy(
                running = true,
                starting = true,
                connected = false,
                mode = CaptureMode.MEDIA_PROJECTION,
                profile = config.profile,
                preferHevc = config.preferHevc,
                statusText = "Avvio MediaProjection…",
                ipAddress = ip,
                port = config.port,
                certificateHex = "",
            )
        }

        val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val projection = mpm.getMediaProjection(resultCode, resultData)
        if (projection == null) {
            HostStateStore.addLog("Impossibile ottenere MediaProjection")
            markStopped("Errore MediaProjection")
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return
        }
        mediaProjection = projection
        projection.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                HostStateStore.addLog("MediaProjection revocata dal sistema")
                projectionSession?.stop()
            }
        }, null)

        val (srcW, srcH) = readDisplayMetrics()
        val densityDpi = resources.displayMetrics.densityDpi.coerceAtLeast(160)

        val session = PhantomHostSession(
            config = config,
            sourceWidth = srcW,
            sourceHeight = srcH,
            captureFactory = SurfaceCaptureFactory { surface, width, height ->
                val vd: VirtualDisplay = projection.createVirtualDisplay(
                    "phantom-host",
                    width,
                    height,
                    densityDpi,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    surface,
                    null,
                    null,
                ) ?: throw IllegalStateException("createVirtualDisplay ha restituito null")
                AutoCloseable {
                    try {
                        vd.release()
                    } catch (_: Throwable) {
                    }
                }
            },
            thermalProvider = { currentThermalPolicy.get() },
            onTouch = {},
            onKey = {},
            onReady = { boundPort, certHex ->
                val addr = formatAddr(ip, boundPort)
                HostStateStore.update {
                    it.copy(
                        running = true,
                        starting = false,
                        ipAddress = ip,
                        port = boundPort,
                        certificateHex = certHex,
                        statusText = "In ascolto su $addr",
                    )
                }
                updateNotification("In ascolto su $addr")
            },
            onStats = { s ->
                val cur = HostStateStore.snapshot()
                val status = if (s.connected) {
                    String.format(java.util.Locale.US, "Connesso · %.0f fps · %.1f Mbps", s.fps, s.mbps)
                } else {
                    "In ascolto su ${formatAddr(cur.ipAddress, cur.port)}"
                }
                HostStateStore.update {
                    it.copy(
                        connected = s.connected,
                        fps = s.fps,
                        mbps = s.mbps,
                        touches = s.touches,
                        keys = s.keys,
                        activeProfileText = s.activeProfile,
                        statusText = status,
                    )
                }
                updateNotification(status)
            },
            onLog = { msg -> HostStateStore.addLog(msg) },
            onThermalStop = { HostStateStore.addLog("Stop automatico per protezione termica") },
        )
        projectionSession = session

        val thread = Thread({
            try {
                session.runBlocking()
            } finally {
                try {
                    projection.stop()
                } catch (_: Throwable) {
                }
                mediaProjection = null
                projectionSession = null
                markStopped("Host fermo")
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }, "phantom-proj-host")
        workerThread = thread
        thread.start()
    }

    private fun stopAllWork() {
        projectionSession?.stop()
        projectionSession = null
        try {
            mediaProjection?.stop()
        } catch (_: Throwable) {
        }
        mediaProjection = null

        val writer = rootStdin
        rootStdin = null
        if (writer != null) {
            try {
                writer.write("STOP\n")
                writer.flush()
            } catch (_: Throwable) {
            }
        }
        val proc = rootProcess
        if (proc != null) {
            try {
                if (!proc.waitFor(1200, TimeUnit.MILLISECONDS)) {
                    proc.destroyForcibly()
                }
            } catch (_: Throwable) {
                proc.destroyForcibly()
            }
            rootProcess = null
        }
        workerThread = null
        releaseWakeLock()
        markStopped("Fermo")
    }

    private fun markStopped(status: String) {
        HostStateStore.update {
            it.copy(
                running = false,
                starting = false,
                connected = false,
                fps = 0.0,
                mbps = 0.0,
                activeProfileText = "",
                statusText = status,
            )
        }
    }

    private fun refreshThermalPolicy() {
        val status = readAndroidThermalStatus()
        val policy = ThermalGuard.evaluate(batteryTempTenths, status)
        currentThermalPolicy.set(policy)

        val tempC = batteryTempTenths?.let { it / 10.0 }
        HostStateStore.update {
            it.copy(batteryTempC = tempC, thermalLabel = policy.label)
        }

        val writer = rootStdin
        if (writer != null) {
            try {
                val temp = batteryTempTenths?.toString() ?: "none"
                writer.write("THERMAL|$temp|$status\n")
                writer.flush()
            } catch (_: Throwable) {
            }
        }
        if (policy.shouldStop && HostStateStore.snapshot().running) {
            HostStateStore.addLog("Temperatura critica (${tempC ?: "?"} °C): arresto host")
            projectionSession?.stop()
        }
    }

    private fun readAndroidThermalStatus(): Int {
        val pm = getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return 0
        return if (Build.VERSION.SDK_INT >= 29) pm.currentThermalStatus else 0
    }

    private fun readDisplayMetrics(): Pair<Int, Int> {
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        return if (Build.VERSION.SDK_INT >= 30) {
            val bounds = wm.maximumWindowMetrics.bounds
            bounds.width() to bounds.height()
        } else {
            val dm = android.util.DisplayMetrics()
            @Suppress("DEPRECATION")
            wm.defaultDisplay.getRealMetrics(dm)
            dm.widthPixels to dm.heightPixels
        }
    }

    private fun acquireWakeLock() {
        if (wakeLock?.isHeld == true) return
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "phantom:host").apply {
            setReferenceCounted(false)
            acquire(6 * 60 * 60 * 1000L)
        }
    }

    private fun releaseWakeLock() {
        try {
            if (wakeLock?.isHeld == true) wakeLock?.release()
        } catch (_: Throwable) {
        }
        wakeLock = null
    }

    private fun ensureChannel() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "Phantom Host",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Stato e controllo rapido dello streaming verso iPhone"
        }
        nm.createNotificationChannel(channel)
    }

    private fun buildNotification(text: String): Notification {
        val openIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, HostForegroundService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stopAction = Notification.Action.Builder(null, "Ferma", stopIntent).build()
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Phantom Host")
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(openIntent)
            .addAction(stopAction)
            .build()
    }

    private fun updateNotification(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun collectActiveIpv4Interfaces(): List<Pair<String, String>> {
        return try {
            val ifaces = NetworkInterface.getNetworkInterfaces() ?: return emptyList()
            val candidates = ArrayList<Pair<String, String>>()
            while (ifaces.hasMoreElements()) {
                val iface = ifaces.nextElement()
                if (!iface.isUp || iface.isLoopback) continue
                val addrs = iface.inetAddresses
                while (addrs.hasMoreElements()) {
                    val addr = addrs.nextElement()
                    if (addr is Inet4Address && !addr.isLoopbackAddress) {
                        val host = addr.hostAddress.orEmpty()
                        if (host.isNotBlank()) {
                            candidates.add(iface.name to host)
                        }
                    }
                }
            }
            candidates
        } catch (_: Throwable) {
            emptyList()
        }
    }

    private fun describeActiveIpv4Interfaces(): String =
        collectActiveIpv4Interfaces().joinToString(", ") { (name, ip) -> "$name=$ip" }

    private fun localWifiIpv4(): String =
        HostFormatHelpers.selectPreferredLanIpv4(collectActiveIpv4Interfaces())

    private fun formatAddr(ip: String, port: Int): String = if (ip.isBlank()) ":$port" else "$ip:$port"

    private fun shellQuote(s: String): String = "'" + s.replace("'", "'\"'\"'") + "'"

    companion object {
        const val ACTION_START_ROOT = "app.phantom.host.START_ROOT"
        const val ACTION_START_PROJECTION = "app.phantom.host.START_PROJECTION"
        const val ACTION_STOP = "app.phantom.host.STOP"

        const val EXTRA_PORT = "port"
        const val EXTRA_PROFILE = "profile"
        const val EXTRA_HEVC = "hevc"
        const val EXTRA_RESULT_CODE = "result_code"
        const val EXTRA_RESULT_DATA = "result_data"

        private const val CHANNEL_ID = "phantom_host"
        private const val NOTIFICATION_ID = 4780
    }
}
