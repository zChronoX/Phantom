package app.phantom.host.root

import android.os.Looper
import android.view.KeyEvent
import app.phantom.host.HostIdentityStore
import app.phantom.host.core.PairingCodec
import app.phantom.host.core.ThermalGuard
import app.phantom.host.stream.AudioStreamer
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.HostRunConfig
import app.phantom.host.stream.PhantomHostSession
import app.phantom.host.stream.StreamProfile
import app.phantom.host.stream.SurfaceCaptureFactory
import uniffi.phantom_ffi.RemoteControlCommand
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.io.PrintWriter
import java.io.StringWriter
import java.util.concurrent.atomic.AtomicReference
import kotlin.system.exitProcess

/**
 * Worker eseguibile tramite `app_process64` on-demand, avviato con `su` (modalità Root) oppure con Shizuku come utente
 * `shell` (modalità Shizuku). Comunica con `HostForegroundService` via stdout/stdin a righe (`READY|...`, `STATS|...`,
 * `STOP`).
 *
 * Con Shizuku l'utente `shell` non può leggere i file privati dell'app: l'identità (certificato, chiave, segreto P2P,
 * token) arriva allora come prima riga su stdin (`IDENTITY|...`, base64url) e l'argomento dei percorsi vale
 * [IDENTITY_STDIN].
 */
object RootHostMain {
    @JvmStatic
    fun main(args: Array<String>) {
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            emitError("Uncaught su ${thread.name}", throwable)
            exitProcess(1)
        }

        try {
            runWorker(args)
        } catch (t: Throwable) {
            emitError("Errore worker root", t)
            exitProcess(1)
        }
    }

    private fun runWorker(args: Array<String>) {
        @Suppress("DEPRECATION")
        if (Looper.getMainLooper() == null) {
            Looper.prepareMainLooper()
        }
        // Android 14–17: ActivityThread/contesto "shell" finti, necessari ad audio e display virtuale (vedi scrcpy).
        WorkerEnvironment.apply()

        val port = args.getOrNull(0)?.toIntOrNull() ?: 47800
        val profileName = args.getOrNull(1) ?: StreamProfile.P1080_120.name
        val profile = StreamProfile.entries.firstOrNull { it.name == profileName } ?: StreamProfile.P1080_120
        val preferHevc = args.getOrNull(2)?.toBooleanStrictOrNull() ?: true
        val libDir = args.getOrNull(3).orEmpty()
        val certPath = args.getOrNull(4)
        val keyPath = args.getOrNull(5)
        val p2pSecretPath = args.getOrNull(6)
        val authTokenPath = args.getOrNull(7)
        val stdin = BufferedReader(InputStreamReader(System.`in`))

        preloadNativeLibs(libDir)
        val identity: Pair<ByteArray, ByteArray>?
        val p2pSecret: ByteArray?
        val authToken: ByteArray?
        if (certPath == IDENTITY_STDIN) {
            val parts = (stdin.readLine() ?: "").split('|')
            fun field(i: Int): ByteArray? = parts.getOrNull(i)?.takeIf { it.isNotEmpty() }?.let {
                PairingCodec.decodeBase64UrlNoPad(it)
            }?.takeIf { it.isNotEmpty() }
            val cert = if (parts.firstOrNull() == "IDENTITY") field(1) else null
            val key = if (parts.firstOrNull() == "IDENTITY") field(2) else null
            identity = if (cert != null && key != null) cert to key else null
            p2pSecret = field(3)?.takeIf { it.size == 32 }
            authToken = field(4)?.takeIf { it.size == 16 }
        } else {
            identity = HostIdentityStore.loadFromPaths(certPath, keyPath)
            p2pSecret = HostIdentityStore.loadP2pSecretFromPath(p2pSecretPath)
            authToken = HostIdentityStore.loadAuthTokenFromPath(authTokenPath)
        }
        emit("LOG|Worker avviato come uid ${android.os.Process.myUid()} · Android ${android.os.Build.VERSION.RELEASE} (API ${android.os.Build.VERSION.SDK_INT})${if (android.os.Process.myUid() == 0) " (root)" else " (shell/Shizuku)"}")

        val config = HostRunConfig(
            port = port,
            profile = profile,
            preferHevc = preferHevc,
            mode = CaptureMode.ROOT,
            intraRefresh = System.getProperty("phantom.intraRefresh") == "true",
        )

        WorkerEnvironment.notes.forEach { emit("LOG|Ambiente worker: $it") }
        tuneNetworkForStreaming(enable = true)
        RootDisplayCapture.onLog = { msg -> emit("LOG|$msg") }
        val (srcW, srcH) = RootDisplayCapture.readPrimaryDisplaySize()
        val injector = RootInputInjector(srcW, srcH)
        emit("LOG|Display rilevato ${srcW}×${srcH} · input injector=${injector.isAvailable} · identità=${identity != null} · p2p=${p2pSecret != null} · auth=${authToken != null}")

        val currentPolicy = AtomicReference(ThermalGuard.evaluate(null, 0))
        val session = PhantomHostSession(
            config = config,
            sourceSizeProvider = { RootDisplayCapture.readPrimaryDisplaySize() },
            captureFactory = SurfaceCaptureFactory { surface, width, height, curSrcW, curSrcH ->
                RootDisplayCapture.attachVirtualDisplay(surface, width, height, curSrcW, curSrcH)
            },
            thermalProvider = { currentPolicy.get() },
            identityCertDer = identity?.first,
            identityKeyDer = identity?.second,
            p2pSecretKey = p2pSecret,
            authToken = authToken,
            audioFactory = { AudioStreamer.createForRoot { msg -> emit("LOG|${msg.replace('\n', ' ')}") } },
            onSourceSizeChanged = { w, h -> injector.setDisplaySize(w, h) },
            onTouch = { injector.injectTouch(it) },
            onKey = { injector.injectKey(it) },
            onClipboardFromClient = { text ->
                val b64 = PairingCodec.encodeBase64UrlNoPad(text.toByteArray(Charsets.UTF_8))
                emit("SET_CLIP|$b64")
            },
            onControlCommand = { cmd ->
                when (cmd) {
                    RemoteControlCommand.WAKE_SCREEN -> {
                        RootDisplayPower.setPhysicalDisplayPower(true)
                        injector.injectKeyCode(KeyEvent.KEYCODE_WAKEUP)
                        emit("LOG|Risveglio schermo richiesto dal client")
                    }
                    RemoteControlCommand.SCREEN_OFF -> {
                        val ok = RootDisplayPower.setPhysicalDisplayPower(false)
                        emit("LOG|Schermo fisico host spento (ok=$ok)" + powerError(ok))
                    }
                    RemoteControlCommand.SCREEN_ON -> {
                        val ok = RootDisplayPower.setPhysicalDisplayPower(true)
                        emit("LOG|Schermo fisico host riacceso (ok=$ok)" + powerError(ok))
                    }
                    RemoteControlCommand.BACK -> injector.injectKeyCode(KeyEvent.KEYCODE_BACK)
                    RemoteControlCommand.HOME -> injector.injectKeyCode(KeyEvent.KEYCODE_HOME)
                    RemoteControlCommand.RECENTS -> injector.injectKeyCode(KeyEvent.KEYCODE_APP_SWITCH)
                    RemoteControlCommand.NOTIFICATIONS -> runShell("cmd statusbar expand-notifications")
                    RemoteControlCommand.QUICK_SETTINGS -> runShell("cmd statusbar expand-settings")
                    RemoteControlCommand.POWER -> injector.injectKeyCode(KeyEvent.KEYCODE_POWER)
                    RemoteControlCommand.VOLUME_UP -> injector.injectKeyCode(KeyEvent.KEYCODE_VOLUME_UP)
                    RemoteControlCommand.VOLUME_DOWN -> injector.injectKeyCode(KeyEvent.KEYCODE_VOLUME_DOWN)
                    RemoteControlCommand.SCREENSHOT -> injector.injectKeyCode(KeyEvent.KEYCODE_SYSRQ)
                    // gestito dalla sessione stessa (log per la diagnosi): qui non arriva
                    RemoteControlCommand.SEND_LOG -> Unit
                    // la pausa del video la gestiscono il core e la sessione: qui non arriva
                    RemoteControlCommand.PAUSE_VIDEO, RemoteControlCommand.RESUME_VIDEO -> Unit
                }
            },
            onClientConnected = {
                injector.injectKeyCode(KeyEvent.KEYCODE_WAKEUP)
            },
            onReady = { boundPort, certHex ->
                emit("READY|$boundPort|$certHex")
            },
            onP2pReady = { endpointId ->
                emit("P2P_READY|$endpointId")
            },
            onStats = { s ->
                emit("STATS|${if (s.connected) 1 else 0}|${s.fps}|${s.mbps}|${s.touches}|${s.keys}|${s.activeProfile}")
            },
            onLog = { msg ->
                emit("LOG|${msg.replace('\n', ' ')}")
            },
            onThermalStop = {
                emit("THERMAL_STOP")
            },
            onClientDisconnected = {
                injector.reset()
                RootDisplayPower.restoreIfTurnedOff()
            },
        )

        val stdinThread = Thread({
            try {
                val reader = stdin
                while (true) {
                    val line = reader.readLine() ?: break
                    when {
                        line == "STOP" -> {
                            session.stop()
                            break
                        }
                        line == "NET_CHANGE" -> {
                            session.notifyNetworkChanged()
                        }
                        line.startsWith("CLIP|") -> {
                            val raw = PairingCodec.decodeBase64UrlNoPad(line.removePrefix("CLIP|"))
                            if (raw != null && raw.isNotEmpty()) {
                                session.sendClipboardToClient(String(raw, Charsets.UTF_8))
                            }
                        }
                        line.startsWith("THERMAL|") -> {
                            val parts = line.split('|')
                            val tempTenths = parts.getOrNull(1)?.toIntOrNull()
                            val status = parts.getOrNull(2)?.toIntOrNull() ?: 0
                            currentPolicy.set(ThermalGuard.evaluate(tempTenths, status))
                        }
                    }
                }
            } catch (_: Throwable) {
            } finally {
                session.stop()
            }
        }, "phantom-root-stdin")
        stdinThread.isDaemon = true
        stdinThread.start()

        try {
            session.runBlocking()
        } finally {
            RootDisplayPower.restoreIfTurnedOff()
            tuneNetworkForStreaming(enable = false)
        }
        emit("STOPPED")
    }

    /** Argomento al posto dei percorsi dell'identità: l'identità arriva come prima riga su stdin. */
    const val IDENTITY_STDIN = "--identity-stdin"

    /**
     * Regolazioni di sistema utili allo streaming, possibili con i permessi del worker (root o shell):
     * - **Wi-Fi a bassa latenza forzata** (`cmd wifi force-low-latency-mode`): spegne il risparmio energetico della
     *   radio. Il Wi-Fi lock dell'app non basta: da Android 14 `HIGH_PERF` non fa più nulla e `LOW_LATENCY` vale solo
     *   con Phantom in primo piano e schermo acceso, cioè quasi mai durante l'uso remoto. Su alcune ROM il comando è
     *   riservato a root: l'esito va nel log.
     * - **Esclusione dall'ottimizzazione batteria** (`cmd deviceidle whitelist`): il servizio dell'app resta attivo e
     *   in rete anche in Doze, così l'host resta raggiungibile a telefono fermo.
     */
    private fun tuneNetworkForStreaming(enable: Boolean) {
        val mode = if (enable) "enabled" else "disabled"
        val wifi = runShellSync("cmd wifi force-low-latency-mode $mode")
        if (enable) {
            emit("LOG|Wi-Fi a bassa latenza forzata: ${describeShellResult(wifi)}")
            val idle = runShellSync("cmd deviceidle whitelist +$APP_PACKAGE")
            emit("LOG|Esclusione dal risparmio batteria (Doze): ${describeShellResult(idle)}")
        }
    }

    private const val APP_PACKAGE = "app.phantom.host"

    /** Esito leggibile di un comando di sistema: «attiva» se è andato bene, altrimenti la prima riga dell'errore. */
    private fun describeShellResult(result: Pair<Int, String>?): String {
        if (result == null) return "non eseguibile"
        val (code, out) = result
        val firstLine = out.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }.orEmpty()
        val failed = code != 0 || listOf("Exception", "denied", "not allowed", "Unknown", "Error", "error:")
            .any { firstLine.contains(it, ignoreCase = false) }
        return if (failed) "non consentita (${firstLine.take(140).ifEmpty { "exit $code" }})" else "attiva"
    }

    /** Esegue un comando e ne restituisce exit code e output (al massimo 3 s), o `null` se non parte. */
    private fun runShellSync(command: String): Pair<Int, String>? = try {
        val p = ProcessBuilder("sh", "-c", command).redirectErrorStream(true).start()
        val out = StringBuilder()
        val reader = Thread({
            try {
                out.append(p.inputStream.bufferedReader().readText())
            } catch (_: Throwable) {
            }
        }, "phantom-shell-out").apply {
            isDaemon = true
            start()
        }
        val done = p.waitFor(3, java.util.concurrent.TimeUnit.SECONDS)
        if (!done) p.destroy()
        reader.join(500)
        (if (done) p.exitValue() else -1) to out.toString()
    } catch (_: Throwable) {
        null
    }

    /** Comando di sistema eseguito con i permessi del worker (root o shell), senza attendere l'esito. */
    private fun runShell(command: String) {
        Thread({
            try {
                val p = ProcessBuilder("sh", "-c", command).redirectErrorStream(true).start()
                p.inputStream.readBytes()
                p.waitFor()
            } catch (t: Throwable) {
                emit("LOG|Comando fallito ($command): ${t.javaClass.simpleName}")
            }
        }, "phantom-shell-cmd").apply {
            isDaemon = true
            start()
        }
    }

    private fun preloadNativeLibs(libDir: String) {
        if (libDir.isBlank()) return
        System.setProperty("jna.library.path", libDir)
        System.setProperty("jna.boot.library.path", libDir)
        System.setProperty("jna.nounpack", "true")
        for (name in listOf("libjnidispatch.so", "libphantom_ffi.so")) {
            val file = File(libDir, name)
            if (file.isFile) {
                try {
                    System.load(file.absolutePath)
                } catch (t: Throwable) {
                    emit("LOG|Preload $name fallito (${file.absolutePath}): ${t.javaClass.simpleName}: ${t.message}")
                }
            } else {
                emit("LOG|Libreria non trovata: ${file.absolutePath}")
            }
        }
    }

    private fun emitError(prefix: String, t: Throwable) {
        val sw = StringWriter()
        t.printStackTrace(PrintWriter(sw))
        val lines = sw.toString().lines().filter { it.isNotBlank() }.take(8)
        emit("LOG|$prefix: ${t.javaClass.name}: ${t.message}")
        for (line in lines) {
            emit("LOG|  $line")
        }
    }

    private val outLock = Any()

    /** Motivo del fallimento di spegnimento/accensione da aggiungere al log (vuoto se è andata bene). */
    private fun powerError(ok: Boolean): String =
        if (ok) "" else RootDisplayPower.lastError.takeIf { it.isNotBlank() }?.let { " · $it" }.orEmpty()

    private fun emit(line: String) {
        synchronized(outLock) {
            println(line)
            System.out.flush()
        }
    }
}
