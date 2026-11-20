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
 * Worker eseguibile tramite `su` + `app_process64` on-demand.
 * Comunica con `HostForegroundService` via stdout/stdin a righe (`READY|...`, `STATS|...`, `STOP`).
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

        val port = args.getOrNull(0)?.toIntOrNull() ?: 47800
        val profileName = args.getOrNull(1) ?: StreamProfile.P1080_120.name
        val profile = StreamProfile.entries.firstOrNull { it.name == profileName } ?: StreamProfile.P1080_120
        val preferHevc = args.getOrNull(2)?.toBooleanStrictOrNull() ?: true
        val libDir = args.getOrNull(3).orEmpty()
        val certPath = args.getOrNull(4)
        val keyPath = args.getOrNull(5)
        val p2pSecretPath = args.getOrNull(6)
        val authTokenPath = args.getOrNull(7)

        preloadNativeLibs(libDir)
        val identity = HostIdentityStore.loadFromPaths(certPath, keyPath)
        val p2pSecret = HostIdentityStore.loadP2pSecretFromPath(p2pSecretPath)
        val authToken = HostIdentityStore.loadAuthTokenFromPath(authTokenPath)

        val config = HostRunConfig(
            port = port,
            profile = profile,
            preferHevc = preferHevc,
            mode = CaptureMode.ROOT,
        )

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
                        emit("LOG|Schermo fisico host spento (ok=$ok)")
                    }
                    RemoteControlCommand.SCREEN_ON -> {
                        val ok = RootDisplayPower.setPhysicalDisplayPower(true)
                        emit("LOG|Schermo fisico host riacceso (ok=$ok)")
                    }
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
                val reader = BufferedReader(InputStreamReader(System.`in`))
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
        }
        emit("STOPPED")
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

    private fun emit(line: String) {
        synchronized(outLock) {
            println(line)
            System.out.flush()
        }
    }
}
