package app.phantom.host.root

import android.os.Looper
import app.phantom.host.core.ThermalGuard
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.HostRunConfig
import app.phantom.host.stream.PhantomHostSession
import app.phantom.host.stream.StreamProfile
import app.phantom.host.stream.SurfaceCaptureFactory
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.io.PrintWriter
import java.io.StringWriter
import java.util.concurrent.atomic.AtomicReference
import kotlin.system.exitProcess

/**
 * Worker eseguibile tramite `su` + `app_process` on-demand.
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

        preloadNativeLibs(libDir)

        val config = HostRunConfig(
            port = port,
            profile = profile,
            preferHevc = preferHevc,
            mode = CaptureMode.ROOT,
        )

        val (srcW, srcH) = RootDisplayCapture.readPrimaryDisplaySize()
        val injector = RootInputInjector(srcW, srcH)
        emit("LOG|Display rilevato ${srcW}×${srcH} · input injector=${injector.isAvailable}")

        val currentPolicy = AtomicReference(ThermalGuard.evaluate(null, 0))
        val session = PhantomHostSession(
            config = config,
            sourceWidth = srcW,
            sourceHeight = srcH,
            captureFactory = SurfaceCaptureFactory { surface, width, height ->
                RootDisplayCapture.attachVirtualDisplay(surface, width, height, srcW, srcH)
            },
            thermalProvider = { currentPolicy.get() },
            onTouch = { injector.injectTouch(it) },
            onKey = { injector.injectKey(it) },
            onReady = { boundPort, certHex ->
                emit("READY|$boundPort|$certHex")
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

        session.runBlocking()
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
