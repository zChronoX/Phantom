package app.phantom.host

import android.Manifest
import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.service.quicksettings.TileService
import android.view.WindowManager
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.PairingCodec
import app.phantom.host.core.ThermalGuard
import app.phantom.host.stream.AudioStreamer
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
 * Usa identità TLS persistente (`HostIdentityStore`), annuncia il servizio via mDNS (`_phantom._udp.`)
 * per scoperta automatica in LAN e rileva anche eventuali IP VPN/Tailscale per accesso remoto in 5G/4G.
 */
class HostForegroundService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null
    private val currentThermalPolicy = AtomicReference(ThermalGuard.evaluate(null, 0))
    private var batteryTempTenths: Int? = null

    private var workerThread: Thread? = null
    private var rootProcess: Process? = null
    private val rootStdinLock = Any()
    private var rootStdin: BufferedWriter? = null
    private var projectionSession: PhantomHostSession? = null
    private var mediaProjection: MediaProjection? = null

    private var nsdManager: NsdManager? = null
    private var nsdListener: NsdManager.RegistrationListener? = null
    private var connectivityManager: ConnectivityManager? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var clipboardManager: ClipboardManager? = null
    private var clipboardListener: ClipboardManager.OnPrimaryClipChangedListener? = null
    private val lastSyncedClipboard = AtomicReference<String?>(null)
    private val mainHandler = Handler(Looper.getMainLooper())
    private var activeIdentity: StoredHostIdentity? = null

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
        requestTileRefresh()
    }

    private fun startRootMode(config: HostRunConfig) {
        val ifaces = collectActiveIpv4Interfaces()
        val ip = HostFormatHelpers.selectPreferredLanIpv4(ifaces)
        val vpnIp = HostFormatHelpers.selectPreferredVpnIpv4(ifaces)
        val identity = try {
            HostIdentityStore.loadOrCreate(this)
        } catch (t: Throwable) {
            HostStateStore.addLog("Errore identità persistente: ${t.message}")
            null
        }
        activeIdentity = identity

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
                vpnIpAddress = vpnIp,
                port = config.port,
                certificateHex = identity?.let { id -> HostFormatHelpers.toHex(id.certDer) } ?: "",
                certificateDer = identity?.certDer ?: ByteArray(0),
                fingerprintHex = identity?.fingerprintHex ?: "",
                p2pEndpointId = identity?.p2pEndpointId ?: "",
                p2pReady = false,
                authToken = identity?.authToken ?: ByteArray(0),
            )
        }
        registerNetworkMonitor()
        registerClipboardMonitor()
        HostStateStore.addLog("Avvio Root · ${config.profile.label}")

        val thread = Thread({
            var proc: Process? = null
            try {
                val apkPath = applicationInfo.sourceDir
                val libDir = applicationInfo.nativeLibraryDir
                val ifacesSummary = ifaces.joinToString(", ") { (name, addr) -> "$name=$addr" }
                if (ifacesSummary.isNotBlank()) {
                    HostStateStore.addLog("Interfacce IPv4: $ifacesSummary")
                }
                val certPathArg = identity?.certFile?.absolutePath?.let { " ${shellQuote(it)}" }.orEmpty()
                val keyPathArg = identity?.keyFile?.absolutePath?.let { " ${shellQuote(it)}" }.orEmpty()
                val p2pPathArg = identity?.p2pSecretFile?.absolutePath?.let { " ${shellQuote(it)}" }.orEmpty()
                val authPathArg = identity?.authTokenFile?.absolutePath?.let { " ${shellQuote(it)}" }.orEmpty()

                val cmd = "export CLASSPATH=${shellQuote(apkPath)} LD_LIBRARY_PATH=${shellQuote(libDir)}:/system/lib64:/vendor/lib64; " +
                    "APP_PROC=/system/bin/app_process64; [ -x \"\$APP_PROC\" ] || APP_PROC=app_process; " +
                    "exec \"\$APP_PROC\" " +
                    "-Djna.library.path=${shellQuote(libDir)} " +
                    "-Djna.boot.library.path=${shellQuote(libDir)} " +
                    "-Djna.nounpack=true " +
                    "/system/bin --nice-name=phantom-host app.phantom.host.root.RootHostMain " +
                    "${config.port} ${config.profile.name} ${config.preferHevc} ${shellQuote(libDir)}$certPathArg$keyPathArg$p2pPathArg$authPathArg"

                val pb = ProcessBuilder("su", "-c", cmd).redirectErrorStream(true)
                proc = pb.start()
                rootProcess = proc
                val writer = BufferedWriter(OutputStreamWriter(proc.outputStream))
                synchronized(rootStdinLock) {
                    rootStdin = writer
                }
                val temp = batteryTempTenths?.toString() ?: "none"
                val status = readAndroidThermalStatus()
                sendRootCommand("THERMAL|$temp|$status")

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
                unregisterMdns()
                synchronized(rootStdinLock) {
                    rootStdin = null
                }
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
                val certHex = parts.getOrNull(2).orEmpty()
                val ifaces = collectActiveIpv4Interfaces()
                val ip = HostFormatHelpers.selectPreferredLanIpv4(ifaces)
                val vpnIp = HostFormatHelpers.selectPreferredVpnIpv4(ifaces)
                val id = activeIdentity
                HostStateStore.update {
                    it.copy(
                        running = true,
                        starting = false,
                        ipAddress = ip,
                        vpnIpAddress = vpnIp,
                        port = boundPort,
                        certificateHex = certHex,
                        certificateDer = id?.certDer ?: it.certificateDer,
                        fingerprintHex = id?.fingerprintHex ?: it.fingerprintHex,
                        statusText = "In ascolto su ${formatAddr(ip, boundPort)}",
                    )
                }
                registerMdns(boundPort, id?.fingerprintHex.orEmpty())
                updateNotification("In ascolto su ${formatAddr(ip, boundPort)}")
            }
            line.startsWith("P2P_READY|") -> {
                val eid = line.removePrefix("P2P_READY|").trim()
                HostStateStore.update {
                    it.copy(
                        p2pEndpointId = eid.ifBlank { it.p2pEndpointId },
                        p2pReady = true,
                    )
                }
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
            line.startsWith("SET_CLIP|") -> {
                val raw = PairingCodec.decodeBase64UrlNoPad(line.removePrefix("SET_CLIP|"))
                if (raw != null && raw.isNotEmpty()) {
                    applyRemoteClipboard(String(raw, Charsets.UTF_8))
                }
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
        val ifaces = collectActiveIpv4Interfaces()
        val ip = HostFormatHelpers.selectPreferredLanIpv4(ifaces)
        val vpnIp = HostFormatHelpers.selectPreferredVpnIpv4(ifaces)
        val ifacesSummary = ifaces.joinToString(", ") { (name, addr) -> "$name=$addr" }
        if (ifacesSummary.isNotBlank()) {
            HostStateStore.addLog("Interfacce IPv4: $ifacesSummary")
        }
        val identity = try {
            HostIdentityStore.loadOrCreate(this)
        } catch (t: Throwable) {
            HostStateStore.addLog("Errore identità persistente: ${t.message}")
            null
        }
        activeIdentity = identity

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
                vpnIpAddress = vpnIp,
                port = config.port,
                certificateHex = identity?.let { id -> HostFormatHelpers.toHex(id.certDer) } ?: "",
                certificateDer = identity?.certDer ?: ByteArray(0),
                fingerprintHex = identity?.fingerprintHex ?: "",
                p2pEndpointId = identity?.p2pEndpointId ?: "",
                p2pReady = false,
                authToken = identity?.authToken ?: ByteArray(0),
            )
        }
        registerNetworkMonitor()
        registerClipboardMonitor()

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

        val densityDpi = resources.displayMetrics.densityDpi.coerceAtLeast(160)

        val session = PhantomHostSession(
            config = config,
            sourceSizeProvider = { readDisplayMetrics() },
            captureFactory = SurfaceCaptureFactory { surface, width, height, _, _ ->
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
            identityCertDer = identity?.certDer,
            identityKeyDer = identity?.keyDer,
            p2pSecretKey = identity?.p2pSecretKey,
            authToken = identity?.authToken,
            audioFactory = {
                val hasRecord = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
                if (hasRecord) {
                    AudioStreamer.createForProjection(projection) { msg -> HostStateStore.addLog(msg) }
                } else {
                    null
                }
            },
            onTouch = {},
            onKey = {},
            onClipboardFromClient = { text -> applyRemoteClipboard(text) },
            onReady = { boundPort, certHex ->
                val addr = formatAddr(ip, boundPort)
                HostStateStore.update {
                    it.copy(
                        running = true,
                        starting = false,
                        ipAddress = ip,
                        vpnIpAddress = vpnIp,
                        port = boundPort,
                        certificateHex = certHex,
                        certificateDer = identity?.certDer ?: it.certificateDer,
                        fingerprintHex = identity?.fingerprintHex ?: it.fingerprintHex,
                        p2pEndpointId = identity?.p2pEndpointId ?: it.p2pEndpointId,
                        statusText = "In ascolto su $addr",
                    )
                }
                registerMdns(boundPort, identity?.fingerprintHex.orEmpty())
                updateNotification("In ascolto su $addr")
            },
            onP2pReady = { endpointId ->
                HostStateStore.update {
                    it.copy(
                        p2pEndpointId = endpointId.ifBlank { it.p2pEndpointId },
                        p2pReady = true,
                    )
                }
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
                unregisterMdns()
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
        unregisterClipboardMonitor()
        unregisterNetworkMonitor()
        unregisterMdns()
        projectionSession?.stop()
        projectionSession = null
        try {
            mediaProjection?.stop()
        } catch (_: Throwable) {
        }
        mediaProjection = null

        sendRootCommand("STOP")
        synchronized(rootStdinLock) {
            rootStdin = null
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

    private fun registerMdns(port: Int, fingerprintHex: String) {
        unregisterMdns()
        try {
            val mgr = getSystemService(Context.NSD_SERVICE) as? NsdManager ?: return
            val safeName = "Phantom-${Build.MODEL.ifBlank { "Android" }}".take(40)
            val info = NsdServiceInfo().apply {
                serviceName = safeName
                serviceType = "_phantom._udp."
                setPort(port)
                if (fingerprintHex.isNotBlank()) {
                    setAttribute("fp", fingerprintHex)
                }
                setAttribute("name", Build.MODEL.ifBlank { "Android" }.take(32))
            }
            val listener = object : NsdManager.RegistrationListener {
                override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
                    HostStateStore.addLog("mDNS attivo: ${serviceInfo.serviceName} (_phantom._udp)")
                }

                override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                    HostStateStore.addLog("mDNS registrazione fallita (code $errorCode)")
                }

                override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {}
                override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
            }
            nsdManager = mgr
            nsdListener = listener
            mgr.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (t: Throwable) {
            HostStateStore.addLog("mDNS non disponibile: ${t.message}")
        }
    }

    private fun unregisterMdns() {
        val mgr = nsdManager
        val listener = nsdListener
        nsdManager = null
        nsdListener = null
        if (mgr != null && listener != null) {
            try {
                mgr.unregisterService(listener)
            } catch (_: Throwable) {
            }
        }
    }

    private fun registerClipboardMonitor() {
        unregisterClipboardMonitor()
        mainHandler.post {
            try {
                val cm = getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return@post
                val listener = ClipboardManager.OnPrimaryClipChangedListener {
                    try {
                        val clip = cm.primaryClip ?: return@OnPrimaryClipChangedListener
                        if (clip.itemCount <= 0) return@OnPrimaryClipChangedListener
                        val text = clip.getItemAt(0).coerceToText(this)?.toString().orEmpty()
                        if (text.isEmpty() || text == lastSyncedClipboard.get()) return@OnPrimaryClipChangedListener
                        lastSyncedClipboard.set(text)
                        projectionSession?.sendClipboardToClient(text)
                        val b64 = PairingCodec.encodeBase64UrlNoPad(text.toByteArray(Charsets.UTF_8))
                        Thread({
                            sendRootCommand("CLIP|$b64")
                        }, "phantom-clip-out").start()
                    } catch (_: Throwable) {
                    }
                }
                clipboardManager = cm
                clipboardListener = listener
                cm.addPrimaryClipChangedListener(listener)
            } catch (_: Throwable) {
            }
        }
    }

    private fun unregisterClipboardMonitor() {
        val cm = clipboardManager
        val listener = clipboardListener
        clipboardManager = null
        clipboardListener = null
        if (cm != null && listener != null) {
            mainHandler.post {
                try {
                    cm.removePrimaryClipChangedListener(listener)
                } catch (_: Throwable) {
                }
            }
        }
    }

    private fun applyRemoteClipboard(text: String) {
        if (text.isEmpty()) return
        lastSyncedClipboard.set(text)
        mainHandler.post {
            try {
                val cm = getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return@post
                cm.setPrimaryClip(ClipData.newPlainText("Phantom", text))
                HostStateStore.addLog("Appunti ricevuti da iPhone (${text.length} car.)")
            } catch (_: Throwable) {
            }
        }
    }

    private fun registerNetworkMonitor() {
        unregisterNetworkMonitor()
        try {
            val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager ?: return
            val cb = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    notifyNetworkChanged()
                }

                override fun onLost(network: Network) {
                    notifyNetworkChanged()
                }
            }
            connectivityManager = cm
            networkCallback = cb
            cm.registerDefaultNetworkCallback(cb)
        } catch (_: Throwable) {
        }
    }

    private fun unregisterNetworkMonitor() {
        val cm = connectivityManager
        val cb = networkCallback
        connectivityManager = null
        networkCallback = null
        if (cm != null && cb != null) {
            try {
                cm.unregisterNetworkCallback(cb)
            } catch (_: Throwable) {
            }
        }
    }

    private fun notifyNetworkChanged() {
        val ifaces = collectActiveIpv4Interfaces()
        val ip = HostFormatHelpers.selectPreferredLanIpv4(ifaces)
        val vpnIp = HostFormatHelpers.selectPreferredVpnIpv4(ifaces)
        HostStateStore.update { cur ->
            cur.copy(ipAddress = ip.ifBlank { cur.ipAddress }, vpnIpAddress = vpnIp)
        }
        projectionSession?.notifyNetworkChanged()
        sendRootCommand("NET_CHANGE")
    }

    private fun markStopped(status: String) {
        HostStateStore.update {
            it.copy(
                running = false,
                starting = false,
                connected = false,
                p2pReady = false,
                fps = 0.0,
                mbps = 0.0,
                activeProfileText = "",
                statusText = status,
            )
        }
        requestTileRefresh()
    }

    private fun requestTileRefresh() {
        try {
            TileService.requestListeningState(
                this,
                ComponentName(this, PhantomTileService::class.java),
            )
        } catch (_: Throwable) {
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

        val temp = batteryTempTenths?.toString() ?: "none"
        sendRootCommand("THERMAL|$temp|$status")
        if (policy.shouldStop && HostStateStore.snapshot().running) {
            HostStateStore.addLog("Temperatura critica (${tempC ?: "?"} °C): arresto host")
            projectionSession?.stop()
        }
    }

    private fun sendRootCommand(command: String) {
        synchronized(rootStdinLock) {
            val writer = rootStdin ?: return
            try {
                writer.write(command)
                writer.write("\n")
                writer.flush()
            } catch (_: Throwable) {
            }
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
            "Phantom",
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
            .setSmallIcon(R.drawable.ic_phantom_tile)
            .setContentTitle("Phantom")
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

        fun collectActiveIpv4Interfaces(): List<Pair<String, String>> {
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
    }
}
