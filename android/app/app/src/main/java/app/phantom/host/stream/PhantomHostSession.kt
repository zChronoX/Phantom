package app.phantom.host.stream

import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.ThermalGuard
import app.phantom.host.core.ThermalPolicy
import uniffi.phantom_ffi.KeyInput
import uniffi.phantom_ffi.P2pHostEndpoint
import uniffi.phantom_ffi.PhantomHost
import uniffi.phantom_ffi.QuicConnection
import uniffi.phantom_ffi.QuicHostEndpoint
import uniffi.phantom_ffi.QuicTransportException
import uniffi.phantom_ffi.TouchState
import uniffi.phantom_ffi.defaultSessionSettings
import uniffi.phantom_ffi.videoPayloadForDatagram
import java.io.FileInputStream
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs
import kotlin.random.Random

fun interface SurfaceCaptureFactory {
    fun attachSurface(surface: Surface, width: Int, height: Int, srcWidth: Int, srcHeight: Int): AutoCloseable
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
 * Gestisce il ciclo completo lato host: endpoint QUIC in ascolto (con identità persistente),
 * negoziazione `PhantomHost`, streaming hardware `VideoEncoder` con adattamento live alla rotazione
 * Portrait ↔ Landscape, controllo termico del bitrate e consegna tocco/tastiera.
 */
class PhantomHostSession(
    private val config: HostRunConfig,
    private val sourceSizeProvider: () -> Pair<Int, Int>,
    private val captureFactory: SurfaceCaptureFactory,
    private val thermalProvider: () -> ThermalPolicy,
    private val identityCertDer: ByteArray? = null,
    private val identityKeyDer: ByteArray? = null,
    private val p2pSecretKey: ByteArray? = null,
    private val onSourceSizeChanged: (Int, Int) -> Unit = { _, _ -> },
    private val onTouch: (TouchState) -> Unit,
    private val onKey: (KeyInput) -> Unit,
    private val onReady: (port: Int, certHex: String) -> Unit,
    private val onP2pReady: (endpointId: String) -> Unit = {},
    private val onStats: (HostLiveStats) -> Unit,
    private val onLog: (String) -> Unit,
    private val onThermalStop: () -> Unit,
    private val onClientDisconnected: () -> Unit = {},
) {
    private val running = AtomicBoolean(false)
    private val activeConnection = AtomicReference<QuicConnection?>(null)
    private val activeP2pEndpoint = AtomicReference<P2pHostEndpoint?>(null)

    private fun randomNonce16(): ByteArray {
        val out = ByteArray(16)
        try {
            FileInputStream("/dev/urandom").use { stream ->
                var off = 0
                while (off < out.size) {
                    val n = stream.read(out, off, out.size - off)
                    if (n <= 0) break
                    off += n
                }
                if (off == out.size) return out
            }
        } catch (_: Throwable) {
        }
        return Random.nextBytes(16)
    }

    fun runBlocking() {
        if (!running.compareAndSet(false, true)) return
        var endpoint: QuicHostEndpoint? = null
        var lanAcceptThread: Thread? = null
        var p2pAcceptThread: Thread? = null
        val incomingQueue = LinkedBlockingQueue<Pair<String, QuicConnection>>()

        try {
            val cert = identityCertDer
            val key = identityKeyDer
            val lanEp = if (cert != null && key != null && cert.isNotEmpty() && key.isNotEmpty()) {
                QuicHostEndpoint.bindWithIdentity("0.0.0.0:${config.port}", cert, key)
            } else {
                QuicHostEndpoint.bind("0.0.0.0:${config.port}")
            }
            endpoint = lanEp
            val localPort = lanEp.localAddr().substringAfterLast(':').toIntOrNull() ?: config.port
            val certHex = HostFormatHelpers.toHex(lanEp.certificateDer())
            onReady(localPort, certHex)
            onLog("Endpoint QUIC LAN pronto sulla porta $localPort")

            lanAcceptThread = Thread({
                while (running.get()) {
                    try {
                        val conn = lanEp.accept(400uL)
                        incomingQueue.offer("LAN" to conn)
                    } catch (_: QuicTransportException.Timeout) {
                        continue
                    } catch (e: QuicTransportException) {
                        if (running.get()) {
                            onLog("Errore accept LAN: ${e.message}")
                        }
                        break
                    } catch (_: Throwable) {
                        break
                    }
                }
            }, "phantom-accept-lan").apply {
                isDaemon = true
                start()
            }

            val secret = p2pSecretKey
            if (secret != null && secret.size == 32) {
                p2pAcceptThread = Thread({
                    var p2pEp: P2pHostEndpoint? = null
                    try {
                        val ep = P2pHostEndpoint.bind(secret, 8_000uL)
                        p2pEp = ep
                        activeP2pEndpoint.set(ep)
                        val eid = ep.endpointId()
                        onP2pReady(eid)
                        val p2pState = if (ep.isOnline()) "online" else "in attesa relay"
                        onLog("P2P integrato (iroh) $p2pState · ID ${eid.take(12)}…")
                        while (running.get()) {
                            try {
                                val conn = ep.accept(400uL)
                                incomingQueue.offer("P2P" to conn)
                            } catch (_: QuicTransportException.Timeout) {
                                continue
                            } catch (e: QuicTransportException) {
                                if (running.get()) {
                                    onLog("Errore accept P2P: ${e.message}")
                                }
                                break
                            }
                        }
                    } catch (t: Throwable) {
                        if (running.get()) {
                            onLog("P2P integrato non disponibile (LAN resta attiva): ${t.javaClass.simpleName}: ${t.message}")
                        }
                    } finally {
                        activeP2pEndpoint.compareAndSet(p2pEp, null)
                        try {
                            p2pEp?.close()
                        } catch (_: Throwable) {
                        }
                    }
                }, "phantom-accept-p2p").apply {
                    isDaemon = true
                    start()
                }
            }

            while (running.get()) {
                val policy = thermalProvider()
                if (policy.shouldStop) {
                    onLog("Protezione termica attiva: arresto host (${policy.label})")
                    onThermalStop()
                    break
                }

                val item = incomingQueue.poll(400L, TimeUnit.MILLISECONDS) ?: continue
                serveClient(item.second, item.first)
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
            while (true) {
                val leftover = incomingQueue.poll() ?: break
                try {
                    leftover.second.close(0u, ByteArray(0))
                    leftover.second.close()
                } catch (_: Throwable) {
                }
            }
            try {
                lanAcceptThread?.join(700)
            } catch (_: Throwable) {
            }
            try {
                p2pAcceptThread?.join(900)
            } catch (_: Throwable) {
            }
            try {
                endpoint?.close()
            } catch (_: Throwable) {
            }
        }
    }

    fun notifyNetworkChanged() {
        val ep = activeP2pEndpoint.get() ?: return
        Thread({
            try {
                ep.networkChange()
                onLog("Cambio rete rilevato: percorsi P2P aggiornati")
            } catch (_: Throwable) {
            }
        }, "phantom-p2p-netchg").start()
    }

    fun stop() {
        running.set(false)
        try {
            activeConnection.get()?.close(0u, ByteArray(0))
        } catch (_: Throwable) {
        }
    }

    private fun serveClient(conn: QuicConnection, transportLabel: String) {
        activeConnection.set(conn)
        val defaults = defaultSessionSettings().copy(minBitrateBps = 1_000_000uL)
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
        var currentEncoder: VideoEncoder? = null
        var currentCapture: AutoCloseable? = null
        var host: PhantomHost? = null

        try {
            var (curSrcW, curSrcH) = sourceSizeProvider()
            onSourceSizeChanged(curSrcW, curSrcH)
            val (initEncW, initEncH) = HostFormatHelpers.scaledDimensions(curSrcW, curSrcH, config.profile.maxShortSide)

            var enc = VideoEncoder.create(
                width = initEncW,
                height = initEncH,
                requestedFps = config.profile.fps,
                preferHevc = config.preferHevc,
                initialBitrateBps = startBps.toInt(),
                onLog = onLog,
            )
            currentEncoder = enc

            val maxDgram = conn.maxDatagramSize() ?: 1162u
            val nonce = randomNonce16()
            val settings = defaults.copy(
                codec = enc.actualCodec,
                nonce = nonce,
                fragSize = videoPayloadForDatagram(maxDgram),
            )
            val phantomHost = PhantomHost(settings)
            host = phantomHost

            val profileRef = AtomicReference("${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})")
            onLog("Client connesso ($transportLabel) · ${profileRef.get()}")

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
                var appliedBitrate = startBps
                val encRef = AtomicReference(enc)
                val reconfigureRequested = AtomicBoolean(false)
                val pendingSize = AtomicReference(curSrcW to curSrcH)
                val statsLock = Any()
                var framesInWindow = 0
                var bytesInWindow = 0L

                val controlThread = Thread({
                    var lastCheckMs = SystemClock.elapsedRealtime()
                    var lastRotCheckMs = lastCheckMs
                    var winStartMs = lastCheckMs
                    while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                        val nowMs = SystemClock.elapsedRealtime()
                        val policy = thermalProvider()
                        if (policy.shouldStop) {
                            onLog("Soglia termica critica raggiunta: chiusura stream")
                            onThermalStop()
                            clientActive.set(false)
                            break
                        }

                        if (nowMs - lastRotCheckMs >= 300L) {
                            lastRotCheckMs = nowMs
                            val (newW, newH) = sourceSizeProvider()
                            val prev = pendingSize.get()
                            if (newW > 0 && newH > 0 && (newW != prev.first || newH != prev.second)) {
                                pendingSize.set(newW to newH)
                                onSourceSizeChanged(newW, newH)
                                reconfigureRequested.set(true)
                            }
                        }

                        val activeEnc = encRef.get()
                        val nowUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                        if (phantomHost.wantsKeyframe(nowUs.toULong())) {
                            activeEnc?.requestKeyFrame()
                        }

                        if (nowMs - lastCheckMs >= 500L) {
                            lastCheckMs = nowMs
                            val desired = ThermalGuard.cappedBitrate(
                                phantomHost.targetBitrateBps().toLong(),
                                policy,
                                defaults.minBitrateBps.toLong(),
                            )
                            if (appliedBitrate > 0 && abs(desired - appliedBitrate) * 100 > appliedBitrate * 8) {
                                activeEnc?.setBitrate(desired.toInt())
                                appliedBitrate = desired
                            }
                            val elapsedS = ((nowMs - winStartMs).coerceAtLeast(1L)) / 1000.0
                            val fCount: Int
                            val bCount: Long
                            synchronized(statsLock) {
                                fCount = framesInWindow
                                bCount = bytesInWindow
                                framesInWindow = 0
                                bytesInWindow = 0L
                            }
                            val counters = phantomHost.counters()
                            onStats(
                                HostLiveStats(
                                    connected = true,
                                    fps = fCount / elapsedS,
                                    mbps = bCount * 8.0 / 1_000_000.0 / elapsedS,
                                    touches = counters.touchStates.toLong(),
                                    keys = counters.keyEvents.toLong(),
                                    activeProfile = profileRef.get(),
                                ),
                            )
                            winStartMs = nowMs
                        }
                        Thread.sleep(50)
                    }
                }, "phantom-host-ctrl")
                controlThread.start()

                // Ciclo video: si riconfigura automaticamente se cambia orientamento (Portrait ↔ Landscape)
                var firstPass = true
                while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                    if (!firstPass) {
                        val (newW, newH) = pendingSize.get()
                        curSrcW = newW
                        curSrcH = newH
                        val (encW, encH) = HostFormatHelpers.scaledDimensions(curSrcW, curSrcH, config.profile.maxShortSide)
                        val preferHevcNow = enc.actualCodec == uniffi.phantom_ffi.VideoCodec.H265
                        enc = VideoEncoder.create(
                            width = encW,
                            height = encH,
                            requestedFps = config.profile.fps,
                            preferHevc = preferHevcNow,
                            initialBitrateBps = appliedBitrate.toInt(),
                            onLog = onLog,
                        )
                        currentEncoder = enc
                        encRef.set(enc)
                        profileRef.set("${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})")
                        onLog("Rotazione display rilevata · ${profileRef.get()}")
                    }
                    firstPass = false
                    reconfigureRequested.set(false)

                    val cap = captureFactory.attachSurface(enc.inputSurface, enc.actualWidth, enc.actualHeight, curSrcW, curSrcH)
                    currentCapture = cap
                    enc.requestKeyFrame()

                    enc.drainLoop(
                        isActive = {
                            running.get() &&
                                clientActive.get() &&
                                !conn.isClosed() &&
                                !phantomHost.isClosed() &&
                                !reconfigureRequested.get()
                        },
                    ) { frameBytes, isKey, captureTsUs ->
                        synchronized(statsLock) {
                            framesInWindow++
                            bytesInWindow += frameBytes.size
                        }
                        val datagrams = phantomHost.sendFrame(isKey, captureTsUs.toULong(), frameBytes)
                        for (d in datagrams) {
                            try {
                                conn.sendDatagram(d)
                            } catch (_: Throwable) {
                            }
                        }
                    }

                    try {
                        cap.close()
                    } catch (_: Throwable) {
                    }
                    currentCapture = null
                    encRef.set(null)
                    try {
                        enc.release()
                    } catch (_: Throwable) {
                    }
                    currentEncoder = null

                    if (!reconfigureRequested.get()) {
                        break
                    }
                }

                clientActive.set(false)
                controlThread.join(500)
            }

            clientActive.set(false)
            recvThread.join(500)
        } catch (t: Throwable) {
            if (running.get()) {
                val cause = t.cause?.let { " (causa: ${it.javaClass.simpleName}: ${it.message})" }.orEmpty()
                onLog("Errore sessione client: ${t.javaClass.simpleName}: ${t.message}$cause")
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
                currentCapture?.close()
            } catch (_: Throwable) {
            }
            try {
                currentEncoder?.release()
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
