package app.phantom.host.stream

import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.ThermalGuard
import app.phantom.host.core.ThermalPolicy
import uniffi.phantom_ffi.KeyInput
import uniffi.phantom_ffi.PhantomHost
import uniffi.phantom_ffi.QuicConnection
import uniffi.phantom_ffi.QuicHostEndpoint
import uniffi.phantom_ffi.QuicTransportException
import uniffi.phantom_ffi.TouchState
import uniffi.phantom_ffi.defaultSessionSettings
import uniffi.phantom_ffi.videoPayloadForDatagram
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs

fun interface SurfaceCaptureFactory {
    fun attachSurface(surface: Surface, width: Int, height: Int): AutoCloseable
}

data class HostLiveStats(
    val connected: Boolean,
    val fps: Double,
    val mbps: Double,
    val touches: Long,
    val keys: Long,
    val activeProfile: String,
)

/**
 * Gestisce il ciclo completo lato host: endpoint QUIC in ascolto, negoziazione `PhantomHost`,
 * streaming hardware `VideoEncoder`, controllo termico del bitrate e consegna tocco/tastiera.
 */
class PhantomHostSession(
    private val config: HostRunConfig,
    private val sourceWidth: Int,
    private val sourceHeight: Int,
    private val captureFactory: SurfaceCaptureFactory,
    private val thermalProvider: () -> ThermalPolicy,
    private val onTouch: (TouchState) -> Unit,
    private val onKey: (KeyInput) -> Unit,
    private val onReady: (port: Int, certHex: String) -> Unit,
    private val onStats: (HostLiveStats) -> Unit,
    private val onLog: (String) -> Unit,
    private val onThermalStop: () -> Unit,
    private val onClientDisconnected: () -> Unit = {},
) {
    private val running = AtomicBoolean(false)
    private val activeConnection = AtomicReference<QuicConnection?>(null)
    private val rng = SecureRandom()

    fun runBlocking() {
        if (!running.compareAndSet(false, true)) return
        var endpoint: QuicHostEndpoint? = null
        try {
            endpoint = QuicHostEndpoint.bind("0.0.0.0:${config.port}")
            val localPort = endpoint.localAddr().substringAfterLast(':').toIntOrNull() ?: config.port
            val certHex = HostFormatHelpers.toHex(endpoint.certificateDer())
            onReady(localPort, certHex)
            onLog("Endpoint QUIC pronto sulla porta $localPort")

            while (running.get()) {
                val policy = thermalProvider()
                if (policy.shouldStop) {
                    onLog("Protezione termica attiva: arresto host (${policy.label})")
                    onThermalStop()
                    break
                }

                val conn = try {
                    endpoint.accept(400uL)
                } catch (_: QuicTransportException.Timeout) {
                    continue
                } catch (e: QuicTransportException) {
                    if (running.get()) {
                        onLog("Errore accept QUIC: ${e.message}")
                    }
                    break
                }
                serveClient(conn)
            }
        } catch (t: Throwable) {
            if (running.get()) {
                val cause = t.cause?.let { " (causa: ${it.javaClass.simpleName}: ${it.message})" }.orEmpty()
                onLog("Errore fatale host: ${t.javaClass.simpleName}: ${t.message}$cause")
            }
        } finally {
            running.set(false)
            try {
                activeConnection.getAndSet(null)?.close(0u, ByteArray(0))
            } catch (_: Throwable) {
            }
            try {
                endpoint?.close()
            } catch (_: Throwable) {
            }
        }
    }

    fun stop() {
        running.set(false)
        try {
            activeConnection.get()?.close(0u, ByteArray(0))
        } catch (_: Throwable) {
        }
    }

    private fun serveClient(conn: QuicConnection) {
        activeConnection.set(conn)
        val (encW, encH) = HostFormatHelpers.scaledDimensions(sourceWidth, sourceHeight, config.profile.maxShortSide)
        val defaults = defaultSessionSettings()
        val initialPolicy = thermalProvider()
        if (initialPolicy.shouldStop) {
            conn.close(0u, ByteArray(0))
            onThermalStop()
            return
        }

        val startBps = ThermalGuard.cappedBitrate(
            defaults.startBitrateBps.toLong(),
            initialPolicy,
            defaults.minBitrateBps.toLong(),
        )
        var encoder: VideoEncoder? = null
        var captureHandle: AutoCloseable? = null
        var host: PhantomHost? = null

        try {
            val enc = VideoEncoder.create(
                width = encW,
                height = encH,
                requestedFps = config.profile.fps,
                preferHevc = config.preferHevc,
                initialBitrateBps = startBps.toInt(),
                onLog = onLog,
            )
            encoder = enc

            val maxDgram = conn.maxDatagramSize() ?: 1162u
            val nonce = ByteArray(16).also { rng.nextBytes(it) }
            val settings = defaults.copy(
                codec = enc.actualCodec,
                nonce = nonce,
                fragSize = videoPayloadForDatagram(maxDgram),
            )
            val phantomHost = PhantomHost(settings)
            host = phantomHost

            val profileSummary = "${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})"
            onLog("Client connesso su QUIC · $profileSummary")

            val clientActive = AtomicBoolean(true)
            val negotiatedOnce = AtomicBoolean(false)

            // Thread ricezione datagrammi dal client (Hello, ping, NACK, tocco, tastiera, Close)
            val recvThread = Thread({
                while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                    try {
                        val dgram = conn.recvDatagram(2048u, 25uL)
                        val nowUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                        val replies = phantomHost.onDatagram(nowUs.toULong(), dgram)
                        for (reply in replies) {
                            try {
                                conn.sendDatagram(reply)
                            } catch (_: Throwable) {
                            }
                        }
                        if (!negotiatedOnce.get() && phantomHost.negotiated() != null) {
                            negotiatedOnce.set(true)
                            onLog("Sessione negoziata (Hello/HelloAck)")
                        }
                        while (true) {
                            val ts = phantomHost.takeTouch() ?: break
                            onTouch(ts)
                        }
                        while (true) {
                            val key = phantomHost.takeKey() ?: break
                            onKey(key)
                        }
                    } catch (_: QuicTransportException.Timeout) {
                        continue
                    } catch (_: Throwable) {
                        break
                    }
                }
                clientActive.set(false)
            }, "phantom-host-recv")
            recvThread.start()

            // Attende l'apertura obbligatoria (`Hello`/`HelloAck`) prima di avviare cattura schermo
            val openDeadline = SystemClock.elapsedRealtime() + 5_000L
            while (running.get() && clientActive.get() && phantomHost.negotiated() == null) {
                if (SystemClock.elapsedRealtime() >= openDeadline) {
                    onLog("Timeout apertura sessione: nessun Hello ricevuto")
                    clientActive.set(false)
                    break
                }
                Thread.sleep(10)
            }

            if (running.get() && clientActive.get() && phantomHost.negotiated() != null) {
                captureHandle = captureFactory.attachSurface(enc.inputSurface, enc.actualWidth, enc.actualHeight)
                var appliedBitrate = startBps
                var lastCheckMs = SystemClock.elapsedRealtime()
                var winStartMs = lastCheckMs
                var framesInWindow = 0
                var bytesInWindow = 0L

                val controlThread = Thread({
                    while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                        val nowMs = SystemClock.elapsedRealtime()
                        val policy = thermalProvider()
                        if (policy.shouldStop) {
                            onLog("Soglia termica critica raggiunta: chiusura stream")
                            onThermalStop()
                            clientActive.set(false)
                            break
                        }

                        val nowUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                        if (phantomHost.wantsKeyframe(nowUs.toULong())) {
                            enc.requestKeyFrame()
                        }

                        if (nowMs - lastCheckMs >= 500L) {
                            lastCheckMs = nowMs
                            val desired = ThermalGuard.cappedBitrate(
                                phantomHost.targetBitrateBps().toLong(),
                                policy,
                                defaults.minBitrateBps.toLong(),
                            )
                            if (appliedBitrate > 0 && abs(desired - appliedBitrate) * 100 > appliedBitrate * 8) {
                                enc.setBitrate(desired.toInt())
                                appliedBitrate = desired
                            }
                            val elapsedS = ((nowMs - winStartMs).coerceAtLeast(1L)) / 1000.0
                            val counters = phantomHost.counters()
                            onStats(
                                HostLiveStats(
                                    connected = true,
                                    fps = framesInWindow / elapsedS,
                                    mbps = bytesInWindow * 8.0 / 1_000_000.0 / elapsedS,
                                    touches = counters.touchStates.toLong(),
                                    keys = counters.keyEvents.toLong(),
                                    activeProfile = profileSummary,
                                ),
                            )
                            winStartMs = nowMs
                            framesInWindow = 0
                            bytesInWindow = 0L
                        }
                        Thread.sleep(50)
                    }
                }, "phantom-host-ctrl")
                controlThread.start()

                enc.drainLoop(
                    isActive = { running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed() },
                ) { frameBytes, isKey, captureTsUs ->
                    framesInWindow++
                    bytesInWindow += frameBytes.size
                    val datagrams = phantomHost.sendFrame(isKey, captureTsUs.toULong(), frameBytes)
                    for (d in datagrams) {
                        try {
                            conn.sendDatagram(d)
                        } catch (_: Throwable) {
                        }
                    }
                }
                clientActive.set(false)
                controlThread.join(500)
            }

            clientActive.set(false)
            recvThread.join(500)
        } catch (t: Throwable) {
            if (running.get()) {
                onLog("Errore sessione client: ${t.message}")
            }
        } finally {
            try {
                host?.close(0u.toUByte())?.let { closeDgram ->
                    try {
                        conn.sendDatagram(closeDgram)
                    } catch (_: Throwable) {
                    }
                }
            } catch (_: Throwable) {
            }
            try {
                captureHandle?.close()
            } catch (_: Throwable) {
            }
            try {
                encoder?.release()
            } catch (_: Throwable) {
            }
            try {
                conn.close(0u, ByteArray(0))
            } catch (_: Throwable) {
            }
            activeConnection.compareAndSet(conn, null)
            try {
                host?.close()
            } catch (_: Throwable) {
            }
            try {
                conn.close()
            } catch (_: Throwable) {
            }
            try {
                onClientDisconnected()
            } catch (_: Throwable) {
            }
            onStats(HostLiveStats(connected = false, fps = 0.0, mbps = 0.0, touches = 0, keys = 0, activeProfile = ""))
            onLog("Client disconnesso")
        }
    }
}
