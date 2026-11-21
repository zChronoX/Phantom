package app.phantom.host.stream

import android.os.Process
import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.AdaptiveQuality
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.QualityStep
import app.phantom.host.core.ThermalGuard
import app.phantom.host.core.ThermalLevel
import app.phantom.host.core.ThermalPolicy
import uniffi.phantom_ffi.AudioCodec
import uniffi.phantom_ffi.Capability
import uniffi.phantom_ffi.HostReportInfo
import uniffi.phantom_ffi.KeyInput
import uniffi.phantom_ffi.P2pHostEndpoint
import uniffi.phantom_ffi.PhantomHost
import uniffi.phantom_ffi.QuicConnection
import uniffi.phantom_ffi.QuicHostEndpoint
import uniffi.phantom_ffi.QuicTransportException
import uniffi.phantom_ffi.RemoteControlCommand
import uniffi.phantom_ffi.TouchState
import uniffi.phantom_ffi.buildCloseDatagram
import uniffi.phantom_ffi.capsContains
import uniffi.phantom_ffi.defaultSessionSettings
import uniffi.phantom_ffi.verifyClientHello
import uniffi.phantom_ffi.videoPayloadForDatagram
import java.io.FileInputStream
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
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

private data class PendingClient(
    val transportLabel: String,
    val connection: QuicConnection,
    val initialHello: ByteArray?,
)

/**
 * Gestisce il ciclo completo lato host: endpoint QUIC LAN + P2P (`iroh`), autenticazione con token
 * di accoppiamento e preemption immediata di sessioni stale (cambio Wi-Fi → 5G in ~1–2 s), streaming
 * video hardware `VideoEncoder` + audio AAC-LC, appunti condivisi e comandi controllo display.
 */
class PhantomHostSession(
    private val config: HostRunConfig,
    private val sourceSizeProvider: () -> Pair<Int, Int>,
    private val captureFactory: SurfaceCaptureFactory,
    private val thermalProvider: () -> ThermalPolicy,
    private val identityCertDer: ByteArray? = null,
    private val identityKeyDer: ByteArray? = null,
    private val p2pSecretKey: ByteArray? = null,
    private val authToken: ByteArray? = null,
    private val audioFactory: (() -> AudioStreamer?)? = null,
    private val onSourceSizeChanged: (Int, Int) -> Unit = { _, _ -> },
    private val onTouch: (TouchState) -> Unit,
    private val onKey: (KeyInput) -> Unit,
    private val onClipboardFromClient: (String) -> Unit = {},
    private val onControlCommand: (RemoteControlCommand) -> Unit = {},
    private val onClientConnected: () -> Unit = {},
    private val onReady: (port: Int, certHex: String) -> Unit,
    private val onP2pReady: (endpointId: String) -> Unit = {},
    private val onStats: (HostLiveStats) -> Unit,
    private val onLog: (String) -> Unit,
    private val onThermalStop: () -> Unit,
    private val onClientDisconnected: () -> Unit = {},
) {
    private val running = AtomicBoolean(false)
    private val activeConnection = AtomicReference<QuicConnection?>(null)
    private val activeHost = AtomicReference<PhantomHost?>(null)
    private val activeP2pEndpoint = AtomicReference<P2pHostEndpoint?>(null)

    private fun longOverShort(w: Int, h: Int): Double {
        val short = minOf(w, h)
        return if (short > 0) maxOf(w, h).toDouble() / short else 2.0
    }

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
        val incomingQueue = LinkedBlockingQueue<PendingClient>()
        val expectedConnId = defaultSessionSettings().connId
        val expectedToken = authToken?.takeIf { it.size == 16 } ?: ByteArray(0)

        fun authenticateAndEnqueue(conn: QuicConnection, label: String) {
            Thread({
                var accepted = false
                try {
                    val deadline = SystemClock.elapsedRealtime() + 2_500L
                    var validHello: ByteArray? = null
                    while (running.get() && !conn.isClosed() && SystemClock.elapsedRealtime() < deadline) {
                        val dgram = try {
                            conn.recvDatagram(2048u, 100uL)
                        } catch (_: QuicTransportException.Timeout) {
                            continue
                        }
                        if (verifyClientHello(expectedConnId, expectedToken, dgram)) {
                            validHello = dgram
                            break
                        }
                        if (expectedToken.isNotEmpty()) {
                            try {
                                val closeDgram = buildCloseDatagram(expectedConnId, 2u.toUByte())
                                if (closeDgram.isNotEmpty()) {
                                    conn.sendDatagram(closeDgram)
                                    Thread.sleep(40)
                                }
                            } catch (_: Throwable) {
                            }
                            onLog("Connessione ($label) rifiutata: token di accoppiamento non valido")
                            return@Thread
                        }
                    }
                    if (!running.get() || validHello == null) return@Thread

                    accepted = true
                    incomingQueue.offer(PendingClient(label, conn, validHello))
                    // Preemption immediata: se c'era una vecchia sessione stale (es. Wi-Fi caduto),
                    // la chiude subito così il nuovo link P2P/LAN subentra in ~1–2 s invece di 15 s.
                    val previous = activeConnection.get()
                    if (previous != null && previous !== conn) {
                        onLog("Nuova connessione autenticata ($label): sostituzione sessione precedente")
                        try {
                            previous.close(0u, ByteArray(0))
                        } catch (_: Throwable) {
                        }
                    }
                } catch (_: Throwable) {
                } finally {
                    if (!accepted) {
                        try {
                            conn.close(2u, ByteArray(0))
                            conn.close()
                        } catch (_: Throwable) {
                        }
                    }
                }
            }, "phantom-auth-$label").apply {
                isDaemon = true
                start()
            }
        }

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
                        authenticateAndEnqueue(conn, "LAN")
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
                                authenticateAndEnqueue(conn, "P2P")
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
                serveClient(item)
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
                    leftover.connection.close(0u, ByteArray(0))
                    leftover.connection.close()
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

    fun sendClipboardToClient(text: String) {
        if (text.isEmpty()) return
        val h = activeHost.get() ?: return
        val conn = activeConnection.get() ?: return
        try {
            val dgram = h.sendClipboard(text) ?: return
            conn.sendDatagram(dgram)
        } catch (_: Throwable) {
        }
    }

    fun stop() {
        running.set(false)
        try {
            activeConnection.get()?.close(0u, ByteArray(0))
        } catch (_: Throwable) {
        }
    }

    private fun serveClient(pending: PendingClient) {
        val conn = pending.connection
        val transportLabel = pending.transportLabel
        activeConnection.set(conn)
        val isLan = transportLabel == "LAN"
        val defaults = defaultSessionSettings().copy(
            startBitrateBps = if (isLan) 12_000_000uL else 8_000_000uL,
            minBitrateBps = if (isLan) 3_000_000uL else 1_500_000uL,
            maxBitrateBps = if (isLan) 32_000_000uL else 20_000_000uL,
        )
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
        var currentAudio: AudioStreamer? = null
        var host: PhantomHost? = null

        try {
            try {
                onClientConnected()
            } catch (_: Throwable) {
            }

            var (curSrcW, curSrcH) = sourceSizeProvider()
            onSourceSizeChanged(curSrcW, curSrcH)
            // Qualità adattiva: si parte dal profilo scelto e si scende a 720p/540p se la rete concede pochi bit
            val quality = AdaptiveQuality(
                QualityStep(config.profile.maxShortSide, config.profile.fps),
                aspect = longOverShort(curSrcW, curSrcH),
            )
            val currentStep = AtomicReference(quality.current)
            val (initEncW, initEncH) = HostFormatHelpers.scaledDimensions(curSrcW, curSrcH, config.profile.maxShortSide)

            var enc = VideoEncoder.create(
                width = initEncW,
                height = initEncH,
                requestedFps = config.profile.fps,
                preferHevc = config.preferHevc,
                initialBitrateBps = startBps.toInt(),
                intraRefresh = config.intraRefresh,
                onLog = onLog,
            )
            currentEncoder = enc
            synchronized(quality) { quality.fpsCap = enc.actualFps }

            val maxDgram = conn.maxDatagramSize() ?: 1162u
            val nonce = randomNonce16()
            val settings = defaults.copy(
                codec = enc.actualCodec,
                nonce = nonce,
                fragSize = videoPayloadForDatagram(maxDgram),
            )
            val audioStreamer = try {
                audioFactory?.invoke()
            } catch (_: Throwable) {
                null
            }
            currentAudio = audioStreamer

            val tokenBytes = authToken?.takeIf { it.size == 16 } ?: ByteArray(0)
            val phantomHost = PhantomHost.newWithOptions(settings, tokenBytes, audioStreamer != null)
            host = phantomHost
            activeHost.set(phantomHost)

            val audioLabel = if (audioStreamer != null) " + audio AAC" else ""
            val profileRef = AtomicReference("${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})$audioLabel")
            onLog("Client connesso ($transportLabel) · ${profileRef.get()}")

            val encRef = AtomicReference<VideoEncoder?>(enc)
            val clientActive = AtomicBoolean(true)
            val negotiatedOnce = AtomicBoolean(false)

            // Contatori dei problemi di rete: prima ogni errore di invio finiva in un `catch {}` vuoto.
            val sendErrors = AtomicLong(0)
            val queueOverflows = AtomicLong(0)

            fun sendCounted(dgram: ByteArray) {
                try {
                    conn.sendDatagram(dgram)
                } catch (_: Throwable) {
                    sendErrors.incrementAndGet()
                }
            }

            fun processIncoming(dgram: ByteArray) {
                val nowUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                val replies = phantomHost.onDatagram(nowUs.toULong(), dgram)
                for (reply in replies) {
                    sendCounted(reply)
                }
                if (phantomHost.wantsKeyframe(nowUs.toULong())) {
                    encRef.get()?.requestKeyFrame()
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
                phantomHost.takeClipboard()?.let { clipText ->
                    try {
                        onClipboardFromClient(clipText)
                    } catch (_: Throwable) {
                    }
                }
                while (true) {
                    val cmd = phantomHost.takeControl() ?: break
                    try {
                        onControlCommand(cmd)
                    } catch (_: Throwable) {
                    }
                }
            }

            pending.initialHello?.let { processIncoming(it) }

            // Thread ricezione datagrammi dal client (Hello, ping, NACK, tocco, tastiera, appunti, controllo, Close)
            val recvThread = Thread({
                try {
                    Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_DISPLAY)
                } catch (_: Throwable) {
                }
                while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                    try {
                        val dgram = conn.recvDatagram(2048u, 25uL)
                        processIncoming(dgram)
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

            val negotiatedInfo = phantomHost.negotiated()
            if (running.get() && clientActive.get() && negotiatedInfo != null) {
                if (audioStreamer != null && capsContains(negotiatedInfo.caps, Capability.AUDIO_AAC)) {
                    // Ridondanza: ogni pacchetto audio parte due volte, la seconda insieme al successivo (+21 ms).
                    // Costa ~130 kbit/s, niente rispetto al video, e copre le perdite isolate del Wi-Fi: l'iPhone
                    // scarta il doppione o usa la copia se l'originale si è perso.
                    var previousAudio: ByteArray? = null
                    audioStreamer.start { data, isConfig, captureTsUs ->
                        if (!running.get() || !clientActive.get() || conn.isClosed() || phantomHost.isClosed()) return@start
                        val dgram = phantomHost.sendAudio(
                            AudioCodec.AAC_LC,
                            audioStreamer.sampleRate.toUInt(),
                            audioStreamer.channels.toUByte(),
                            isConfig,
                            captureTsUs.toULong(),
                            data,
                        )
                        if (dgram != null) {
                            sendCounted(dgram)
                            if (!isConfig) {
                                previousAudio?.let { sendCounted(it) }
                                previousAudio = dgram
                            }
                        }
                    }
                } else {
                    try {
                        audioStreamer?.close()
                    } catch (_: Throwable) {
                    }
                    currentAudio = null
                }

                var appliedBitrate = startBps
                val reconfigureRequested = AtomicBoolean(false)
                val pendingSize = AtomicReference(curSrcW to curSrcH)
                val statsLock = Any()
                var framesInWindow = 0
                var bytesInWindow = 0L
                // Finestra del rapporto per la telemetria dell'iPhone (protetta da `statsLock`)
                var reportFrames = 0
                var reportBytes = 0L
                var encodeLatencySumUs = 0L
                var encodeLatencyCount = 0

                val controlThread = Thread({
                    var lastBitrateCheckMs = SystemClock.elapsedRealtime()
                    var lastStatsMs = lastBitrateCheckMs
                    var lastRotCheckMs = lastBitrateCheckMs
                    var winStartMs = lastBitrateCheckMs
                    var lastNetLogMs = lastBitrateCheckMs
                    var lastReportMs = lastBitrateCheckMs
                    var loggedOverflows = 0L
                    var loggedErrors = 0L
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
                                synchronized(quality) { quality.setAspect(longOverShort(newW, newH)) }
                                onSourceSizeChanged(newW, newH)
                                reconfigureRequested.set(true)
                            }
                        }

                        val activeEnc = encRef.get()
                        val nowUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                        if (phantomHost.wantsKeyframe(nowUs.toULong())) {
                            activeEnc?.requestKeyFrame()
                        }

                        if (nowMs - lastBitrateCheckMs >= 250L) {
                            lastBitrateCheckMs = nowMs
                            val desired = ThermalGuard.cappedBitrate(
                                phantomHost.targetBitrateBps().toLong(),
                                policy,
                                defaults.minBitrateBps.toLong(),
                            )
                            if (appliedBitrate > 0 && abs(desired - appliedBitrate) * 100 > appliedBitrate * 5) {
                                activeEnc?.setBitrate(desired.toInt())
                                appliedBitrate = desired
                            }
                            val step = synchronized(quality) { quality.update(nowMs, desired) }
                            if (step != null) {
                                currentStep.set(step)
                                onLog(
                                    "Qualità adattata alla rete: ${step.maxShortSide}p · ${step.fps} fps " +
                                        "(bitrate obiettivo ${desired / 1_000_000} Mbps)",
                                )
                                reconfigureRequested.set(true)
                            }
                        }

                        if (nowMs - lastStatsMs >= 500L) {
                            lastStatsMs = nowMs
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

                        // Rapporto per l'HUD dell'iPhone: fps e bitrate reali dell'encoder, tempo di codifica, coda di
                        // invio, pacchetti scartati, keyframe e stato termico.
                        if (nowMs - lastReportMs >= 1_000L) {
                            val elapsedS = ((nowMs - lastReportMs).coerceAtLeast(1L)) / 1000.0
                            lastReportMs = nowMs
                            val rFrames: Int
                            val rBytes: Long
                            val latAvgUs: Long
                            synchronized(statsLock) {
                                rFrames = reportFrames
                                rBytes = reportBytes
                                latAvgUs = if (encodeLatencyCount > 0) encodeLatencySumUs / encodeLatencyCount else 0L
                                reportFrames = 0
                                reportBytes = 0L
                                encodeLatencySumUs = 0L
                                encodeLatencyCount = 0
                            }
                            val e = encRef.get()
                            val queued = try {
                                conn.queuedSendBytes().toLong()
                            } catch (_: Throwable) {
                                0L
                            }
                            val thermal = when (policy.level) {
                                ThermalLevel.NORMAL -> 0
                                ThermalLevel.WARM -> 1
                                ThermalLevel.HOT -> 2
                                ThermalLevel.CRITICAL -> 3
                            }
                            val info = HostReportInfo(
                                encoderFps = rFrames / elapsedS,
                                encodedKbps = (rBytes * 8 / 1000 / elapsedS).toLong().coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                targetKbps = (appliedBitrate / 1000).coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                width = (e?.actualWidth ?: 0).coerceIn(0, 65_535).toUShort(),
                                height = (e?.actualHeight ?: 0).coerceIn(0, 65_535).toUShort(),
                                encodeLatencyUs = latAvgUs.coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                sendQueueBytes = queued.coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                queueOverflows = queueOverflows.get().coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                sendErrors = sendErrors.get().coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                                keyframes = 0u,
                                thermalLevel = thermal.toUByte(),
                            )
                            phantomHost.hostReport(info)?.let { sendCounted(it) }
                        }

                        if (nowMs - lastNetLogMs >= 5_000L) {
                            lastNetLogMs = nowMs
                            val overflows = queueOverflows.get()
                            val errors = sendErrors.get()
                            if (overflows != loggedOverflows || errors != loggedErrors) {
                                val queuedKb = try {
                                    conn.queuedSendBytes().toLong() / 1024
                                } catch (_: Throwable) {
                                    -1L
                                }
                                onLog(
                                    "Rete in affanno (ultimi 5 s): ${overflows - loggedOverflows} pacchetti scartati per coda piena, " +
                                        "${errors - loggedErrors} invii falliti · coda $queuedKb KB · bitrate ${appliedBitrate / 1_000_000} Mbps",
                                )
                                loggedOverflows = overflows
                                loggedErrors = errors
                            }
                        }
                        Thread.sleep(25)
                    }
                }, "phantom-host-ctrl")
                controlThread.start()

                // Ciclo video: si riconfigura automaticamente se cambia orientamento (Portrait ↔ Landscape)
                var firstPass = true
                while (running.get() && clientActive.get() && !conn.isClosed() && !phantomHost.isClosed()) {
                    if (!firstPass) {
                        val (newW, newH) = pendingSize.get()
                        val rotated = newW != curSrcW || newH != curSrcH
                        curSrcW = newW
                        curSrcH = newH
                        val step = currentStep.get()
                        val (encW, encH) = HostFormatHelpers.scaledDimensions(curSrcW, curSrcH, step.maxShortSide)
                        val preferHevcNow = enc.actualCodec == uniffi.phantom_ffi.VideoCodec.H265
                        enc = VideoEncoder.create(
                            width = encW,
                            height = encH,
                            requestedFps = step.fps,
                            preferHevc = preferHevcNow,
                            initialBitrateBps = appliedBitrate.toInt(),
                            intraRefresh = config.intraRefresh,
                            onLog = onLog,
                        )
                        currentEncoder = enc
                        encRef.set(enc)
                        synchronized(quality) {
                            if (quality.index == 0) quality.fpsCap = enc.actualFps
                        }
                        profileRef.set("${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})$audioLabel")
                        onLog((if (rotated) "Rotazione display rilevata · " else "Encoder riconfigurato · ") + profileRef.get())
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
                            reportFrames++
                            reportBytes += frameBytes.size
                            val encodeUs = SystemClock.elapsedRealtimeNanos() / 1_000L - captureTsUs
                            if (encodeUs in 0L..2_000_000L) {
                                encodeLatencySumUs += encodeUs
                                encodeLatencyCount++
                            }
                        }
                        // Frammentazione e invio in una sola chiamata Rust: niente più un passaggio JNA per pacchetto.
                        val report = phantomHost.sendFrameVia(conn, isKey, captureTsUs.toULong(), frameBytes)
                        if (report.queueOverflows > 0u || report.sendErrors > 0u) {
                            queueOverflows.addAndGet(report.queueOverflows.toLong())
                            sendErrors.addAndGet(report.sendErrors.toLong())
                            // Il core ha già chiesto un keyframe (il frame precedente arriverà rotto): lo si
                            // passa subito all'encoder invece di aspettare il giro del thread di controllo.
                            if (phantomHost.wantsKeyframe((SystemClock.elapsedRealtimeNanos() / 1_000L).toULong())) {
                                encRef.get()?.requestKeyFrame()
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
            activeHost.compareAndSet(host, null)
            try {
                currentAudio?.close()
            } catch (_: Throwable) {
            }
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
