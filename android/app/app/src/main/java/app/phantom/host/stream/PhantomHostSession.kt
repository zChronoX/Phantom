package app.phantom.host.stream

import android.os.Process
import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.AdaptiveQuality
import app.phantom.host.core.BitratePlan
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
/** Ogni quanto riassumere i pacchetti iniziali non validi scartati dagli accept. */
private const val STRAY_SUMMARY_MS = 60L * 60L * 1000L

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
    onLog: (String) -> Unit,
    private val onThermalStop: () -> Unit,
    private val onClientDisconnected: () -> Unit = {},
) {
    private val logSink: (String) -> Unit = onLog
    private val recentLog = ArrayDeque<String>()

    /**
     * Ogni riga di log va al chiamante (app o worker) e in un registro delle ultime righe: l'iPhone lo può chiedere
     * per la «diagnosi in un tocco» (`RemoteControlCommand.SEND_LOG`), così basta un file per avere i log di entrambi.
     */
    private fun onLog(message: String) {
        val stamp = java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.ROOT).format(java.util.Date())
        synchronized(recentLog) {
            recentLog.addLast("$stamp · $message")
            while (recentLog.size > MAX_LOG_LINES) recentLog.removeFirst()
        }
        logSink(message)
    }

    /** Testo per la diagnosi: dispositivo, modalità, collegamento attuale e le ultime righe del log dell'host. */
    private fun diagnosticLog(conn: QuicConnection): String {
        val sb = StringBuilder()
        sb.append("Phantom host · ${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL} · Android ")
            .append(android.os.Build.VERSION.RELEASE).append(" (API ").append(android.os.Build.VERSION.SDK_INT).append(")\n")
        sb.append("Modalità ${config.mode} · profilo ${config.profile} · HEVC ${config.preferHevc} · intra-refresh ${config.intraRefresh}\n")
        try {
            val ls = conn.linkStats()
            sb.append(
                "Collegamento ${if (conn.isP2p()) "P2P" else "LAN"}${if (ls.viaRelay) " via relay" else ""} · QUIC RTT ${ls.rttUs / 1000u} ms · " +
                    "finestra ${ls.cwndBytes / 1024u} KB · pacchetti spediti ${ls.sentPackets}, persi ${ls.lostPackets} · " +
                    "congestioni ${ls.congestionEvents} · coda ${conn.queuedSendBytes() / 1024u} KB\n",
            )
        } catch (_: Throwable) {
        }
        sb.append("----\n")
        synchronized(recentLog) {
            for (line in recentLog) sb.append(line).append('\n')
        }
        return sb.toString()
    }

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

            // Un handshake fallito (client con dati vecchi, rete che cambia a metà, pacchetti spuri) è un problema di
            // quella connessione, non dell'endpoint: prima il ciclo usciva con `break` al primo errore e l'host restava
            // irraggiungibile (in LAN o in P2P) fino al riavvio. Ora si esce solo se l'endpoint è chiuso davvero.
            lanAcceptThread = Thread({
                while (running.get()) {
                    try {
                        val conn = lanEp.accept(400uL)
                        authenticateAndEnqueue(conn, "LAN")
                    } catch (_: QuicTransportException.Timeout) {
                        continue
                    } catch (_: QuicTransportException.EndpointClosed) {
                        if (running.get()) onLog("Endpoint LAN chiuso")
                        break
                    } catch (e: QuicTransportException) {
                        if (running.get()) logAcceptFailure("accept-lan", "LAN", e.message)
                    } catch (t: Throwable) {
                        if (running.get()) logThrottled("accept-lan", "Connessione LAN in arrivo fallita: ${t.javaClass.simpleName}: ${t.message}")
                        sleepQuietly(200)
                    }
                }
            }, "phantom-accept-lan").apply {
                isDaemon = true
                start()
            }

            val secret = p2pSecretKey
            if (secret != null && secret.size == 32) {
                p2pAcceptThread = Thread({ runP2pAcceptLoop(secret) { conn -> authenticateAndEnqueue(conn, "P2P") } }, "phantom-accept-p2p").apply {
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

    /**
     * Tiene vivo l'endpoint P2P per tutta la vita dell'host: lo apre (riprovando con attese crescenti se non c'è rete),
     * accetta le connessioni e ogni 15 s controlla di essere collegato al relay. Senza relay l'host non è
     * raggiungibile da fuori casa: dopo 45 s si chiede a iroh di ricontrollare la rete, dopo 3 minuti si chiude e
     * si riapre l'endpoint (stessa chiave, quindi stesso ID: l'iPhone non deve rifare l'associazione).
     */
    private fun runP2pAcceptLoop(secret: ByteArray, onIncoming: (QuicConnection) -> Unit) {
        var backoffMs = 2_000L
        while (running.get()) {
            var ep: P2pHostEndpoint? = null
            try {
                val opened = P2pHostEndpoint.bind(secret, 8_000uL)
                ep = opened
                activeP2pEndpoint.set(opened)
                val eid = opened.endpointId()
                onP2pReady(eid)
                val online = opened.isOnline()
                onLog("P2P integrato (iroh) ${if (online) "online" else "in attesa relay"} · ID ${eid.take(12)}…")
                backoffMs = 2_000L
                var wasOnline = online
                var offlineSinceMs = if (online) -1L else SystemClock.elapsedRealtime()
                var nudged = false
                var lastCheckMs = SystemClock.elapsedRealtime()
                while (running.get()) {
                    try {
                        onIncoming(opened.accept(400uL))
                    } catch (_: QuicTransportException.Timeout) {
                        // normale: nessuna connessione in arrivo
                    } catch (_: QuicTransportException.EndpointClosed) {
                        if (running.get()) onLog("Endpoint P2P chiuso: lo riapro")
                        break
                    } catch (e: QuicTransportException) {
                        if (running.get()) logAcceptFailure("accept-p2p", "P2P", e.message)
                    }
                    val nowMs = SystemClock.elapsedRealtime()
                    if (nowMs - lastCheckMs < 15_000L) continue
                    lastCheckMs = nowMs
                    if (opened.isClosed()) {
                        if (running.get()) onLog("Endpoint P2P chiuso: lo riapro")
                        break
                    }
                    val isOnline = opened.isOnline()
                    if (isOnline) {
                        if (!wasOnline) onLog("P2P di nuovo raggiungibile (relay collegato)")
                        offlineSinceMs = -1L
                        nudged = false
                    } else {
                        if (wasOnline || offlineSinceMs < 0) {
                            offlineSinceMs = nowMs
                            onLog("P2P non raggiungibile da fuori casa: relay scollegato (${opened.relayStatus()})")
                        }
                        val offlineMs = nowMs - offlineSinceMs
                        if (!nudged && offlineMs >= 45_000L) {
                            nudged = true
                            try {
                                opened.networkChange()
                            } catch (_: Throwable) {
                            }
                        } else if (offlineMs >= 180_000L) {
                            onLog("P2P ancora senza relay dopo 3 minuti: riapro l'endpoint")
                            break
                        }
                    }
                    wasOnline = isOnline
                }
            } catch (t: Throwable) {
                if (running.get()) {
                    logThrottled("bind-p2p", "P2P non disponibile, riprovo tra ${backoffMs / 1000} s (LAN resta attiva): ${t.javaClass.simpleName}: ${t.message}")
                }
            } finally {
                activeP2pEndpoint.compareAndSet(ep, null)
                try {
                    ep?.close()
                } catch (_: Throwable) {
                }
            }
            if (!running.get()) break
            sleepQuietly(backoffMs)
            backoffMs = (backoffMs * 2).coerceAtMost(30_000L)
        }
    }

    private val throttledLogs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Pacchetti iniziali non validi per tipo di accept: (quanti, da quando) per il riepilogo all'ora. */
    private val strayInitials = HashMap<String, Pair<Int, Long>>()
    private val strayInitialsExplained = AtomicBoolean(false)

    /** Lo stesso tipo di messaggio al massimo una volta ogni 30 s (un client che riprova non deve riempire il log). */
    /**
     * Connessione in arrivo fallita. Il caso più comune («authentication failed») è il primo pacchetto di una
     * connessione che non si riesce a decifrare: rovinato per strada o non di Phantom (rumore sulla porta aperta verso
     * Internet). QUIC lo scarta senza rispondere e non è un tentativo riuscito di nulla: invece di una riga ogni volta
     * (52 in un giorno nei test della 1.4.0) si contano e si scrive un riepilogo all'ora.
     */
    private fun logAcceptFailure(key: String, label: String, message: String?) {
        if (message?.contains("authentication failed") != true) {
            logThrottled(key, "Connessione $label in arrivo fallita: $message")
            return
        }
        val now = SystemClock.elapsedRealtime()
        synchronized(strayInitials) {
            val (count, since) = strayInitials[key] ?: (0 to now)
            if (!strayInitialsExplained.getAndSet(true)) {
                onLog("$label: scartato un pacchetto iniziale non valido (innocuo: rumore di rete o pacchetto rovinato). " +
                    "Da ora in poi solo un riepilogo all'ora")
            }
            val total = count + 1
            if (now - since >= STRAY_SUMMARY_MS) {
                onLog("$label: $total pacchetti iniziali non validi scartati nell'ultima ora (innocui)")
                strayInitials[key] = 0 to now
            } else {
                strayInitials[key] = total to since
            }
        }
    }

    private fun logThrottled(key: String, message: String) {
        val now = SystemClock.elapsedRealtime()
        val last = throttledLogs[key]
        if (last == null || now - last >= 30_000L) {
            throttledLogs[key] = now
            onLog(message)
        }
    }

    private fun sleepQuietly(ms: Long) {
        try {
            Thread.sleep(ms)
        } catch (_: InterruptedException) {
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
            startBitrateBps = if (isLan) 12_000_000uL else 6_000_000uL,
            // In P2P (5G, relay) la banda può essere molto bassa: meglio un'immagine più piccola e fluida
            // (la qualità adattiva scende a 720p/540p) che una coda di secondi.
            minBitrateBps = if (isLan) 3_000_000uL else 600_000uL,
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
                onLog = ::onLog,
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
            // L'obiettivo del core non sale oltre il bitrate utile del gradino più alto: oltre, l'encoder non lo usa e
            // le riduzioni per congestione taglierebbero un numero che non conta.
            var topUsefulMax = HostFormatHelpers.maxUsefulBitrate(
                enc.actualWidth,
                enc.actualHeight,
                enc.actualFps,
                enc.actualCodec == uniffi.phantom_ffi.VideoCodec.H265,
            )
            phantomHost.setMaxBitrateBps(topUsefulMax.coerceAtLeast(defaults.minBitrateBps.toLong()).toULong())

            val audioLabel = if (audioStreamer != null) " + audio AAC" else ""
            val profileRef = AtomicReference("${enc.actualWidth}×${enc.actualHeight} @ ${enc.actualFps} fps (${enc.actualCodec.name})$audioLabel")
            onLog("Client connesso ($transportLabel) · ${profileRef.get()}")

            val encRef = AtomicReference<VideoEncoder?>(enc)
            val clientActive = AtomicBoolean(true)
            val negotiatedOnce = AtomicBoolean(false)
            // iPhone in background (`PauseVideo`): l'encoder è sospeso e il video non parte
            val videoPaused = AtomicBoolean(false)

            // Contatori dei problemi di rete: prima ogni errore di invio finiva in un `catch {}` vuoto.
            val sendErrors = AtomicLong(0)
            val queueOverflows = AtomicLong(0)
            val framesSkipped = AtomicLong(0)
            val drains = AtomicLong(0)
            val tooLarge = AtomicLong(0)

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
                    if (cmd == RemoteControlCommand.SEND_LOG) {
                        // la diagnosi la serve la sessione stessa: vale in tutte le modalità
                        try {
                            val datagrams = phantomHost.sendLog(diagnosticLog(conn))
                            for (d in datagrams) sendCounted(d)
                            onLog("Log dell'host inviato all'iPhone per la diagnosi (${datagrams.size / 2} pezzi)")
                        } catch (_: Throwable) {
                        }
                        continue
                    }
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
                    var loggedLost = 0uL
                    var loggedCongestion = 0uL
                    var loggedSkipped = 0L
                    var loggedDrains = 0L
                    var loggedTooLarge = 0L
                    var loggedFecGroup = -1
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
                        // Prima la pausa, poi il keyframe: alla ripresa l'encoder va riattivato prima di chiedergliene uno.
                        val pausedNow = phantomHost.videoPaused(nowUs.toULong())
                        if (pausedNow != videoPaused.get()) {
                            videoPaused.set(pausedNow)
                            activeEnc?.setSuspended(pausedNow)
                            onLog(
                                if (pausedNow) {
                                    "iPhone in background: video in pausa (niente dati finché non torna)"
                                } else {
                                    "iPhone in primo piano: video ripreso"
                                },
                            )
                        }
                        if (phantomHost.wantsKeyframe(nowUs.toULong())) {
                            activeEnc?.requestKeyFrame()
                        }

                        val fecGroup = phantomHost.fecGroup().toInt()
                        if (fecGroup != loggedFecGroup) {
                            if (loggedFecGroup >= 0) {
                                onLog(
                                    if (fecGroup < loggedFecGroup) {
                                        "Perdite sparse: parità aumentata a 1 pacchetto ogni $fecGroup"
                                    } else {
                                        "Rete più pulita (o congestionata): parità tornata a 1 pacchetto ogni $fecGroup"
                                    },
                                )
                            }
                            loggedFecGroup = fecGroup
                        }

                        if (nowMs - lastBitrateCheckMs >= 250L) {
                            lastBitrateCheckMs = nowMs
                            // Oltre ~0,15 bit per pixel l'HEVC non migliora più a vista: bit in più servono solo a riempire
                            // il Wi-Fi (a 488×1088 si arrivava a 33 Mbit/s, sei volte il necessario).
                            val usefulMax = activeEnc?.let {
                                HostFormatHelpers.maxUsefulBitrate(it.actualWidth, it.actualHeight, it.actualFps, it.actualCodec == uniffi.phantom_ffi.VideoCodec.H265)
                            } ?: Long.MAX_VALUE
                            // Solo verso l'alto: subito dopo un cambio di gradino l'encoder attivo può essere ancora
                            // quello del gradino precedente, e il tetto non deve scendere per sbaglio.
                            if (synchronized(quality) { quality.index } == 0 && activeEnc != null &&
                                usefulMax > topUsefulMax
                            ) {
                                topUsefulMax = usefulMax
                                phantomHost.setMaxBitrateBps(topUsefulMax.coerceAtLeast(defaults.minBitrateBps.toLong()).toULong())
                            }
                            // L'encoder riceve il bitrate limitato al tetto utile; la qualità adattiva quello della rete
                            // (solo termico e minimo), altrimenti da 720p non si risale più a 1080p.
                            val plan = BitratePlan.compute(
                                phantomHost.targetBitrateBps().toLong(),
                                policy,
                                defaults.minBitrateBps.toLong(),
                                usefulMax,
                            )
                            val desired = plan.encoderBps
                            if (appliedBitrate > 0 && abs(desired - appliedBitrate) * 100 > appliedBitrate * 5) {
                                activeEnc?.setBitrate(desired.toInt())
                                appliedBitrate = desired
                            }
                            // in pausa non passa video: la rete non dice nulla sulla qualità sostenibile
                            val step = if (videoPaused.get()) null else synchronized(quality) { quality.update(nowMs, plan.qualityBps) }
                            if (step != null) {
                                currentStep.set(step)
                                val holdS = synchronized(quality) { quality.nextClimbHoldMs() } / 1000
                                // dopo una risalita fallita si aspetta di più: lo si scrive, così nel log si vede perché
                                val holdNote = if (holdS > 20) " · prossima salita dopo $holdS s di rete buona" else ""
                                onLog(
                                    "Qualità adattata alla rete: ${step.maxShortSide}p · ${step.fps} fps " +
                                        "(rete ${"%.1f".format(java.util.Locale.ROOT, plan.qualityBps / 1_000_000.0)} Mbps)$holdNote",
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
                                framesSkipped = framesSkipped.get().coerceIn(0L, UInt.MAX_VALUE.toLong()).toUInt(),
                            )
                            phantomHost.hostReport(info)?.let { sendCounted(it) }
                        }

                        if (nowMs - lastNetLogMs >= 5_000L) {
                            lastNetLogMs = nowMs
                            val overflows = queueOverflows.get()
                            val errors = sendErrors.get()
                            // Statistiche QUIC degli ultimi 5 s: un RTT che esplode o perdite dicono «rete/Wi-Fi»; una finestra di
                            // congestione piccola con RTT normale dice «controllo di congestione».
                            val quic = try {
                                val ls = conn.linkStats()
                                val lost = ls.lostPackets - loggedLost
                                val cong = ls.congestionEvents - loggedCongestion
                                loggedLost = ls.lostPackets
                                loggedCongestion = ls.congestionEvents
                                " · QUIC RTT ${ls.rttUs / 1000u} ms, finestra ${ls.cwndBytes / 1024u} KB, persi $lost, " +
                                    "congestioni $cong${if (ls.viaRelay) " · via relay" else ""}"
                            } catch (_: Throwable) {
                                ""
                            }
                            val skipped = framesSkipped.get()
                            val drained = drains.get()
                            val large = tooLarge.get()
                            // Anche gli svuotamenti senza pacchetti scartati vanno nel log: in P2P la qualità crollava
                            // (migliaia di frame saltati) senza che comparisse una sola riga.
                            if (overflows != loggedOverflows || errors != loggedErrors || drained != loggedDrains) {
                                val queuedKb = try {
                                    conn.queuedSendBytes().toLong() / 1024
                                } catch (_: Throwable) {
                                    -1L
                                }
                                val largeText = if (large != loggedTooLarge) " (di cui troppo grandi ${large - loggedTooLarge})" else ""
                                onLog(
                                    "Rete in affanno (ultimi 5 s): ${drained - loggedDrains} svuotamenti, ${skipped - loggedSkipped} frame saltati, " +
                                        "${overflows - loggedOverflows} pacchetti scartati per coda piena, " +
                                        "${errors - loggedErrors} invii falliti$largeText · coda $queuedKb KB · " +
                                        "bitrate ${"%.1f".format(java.util.Locale.ROOT, appliedBitrate / 1_000_000.0)} Mbps$quic",
                                )
                                loggedOverflows = overflows
                                loggedErrors = errors
                            }
                            loggedSkipped = skipped
                            loggedDrains = drained
                            loggedTooLarge = large
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
                            onLog = ::onLog,
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
                    if (videoPaused.get()) {
                        // encoder nuovo (es. rotazione) mentre l'iPhone è in background: resta sospeso anche lui
                        enc.setSuspended(true)
                    } else {
                        enc.requestKeyFrame()
                    }

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
                        // Se la coda di invio è troppo piena il core salta il frame (si svuota e riparte da un solo
                        // keyframe) invece di accumulare secondi di ritardo.
                        val report = phantomHost.sendFrameVia(conn, isKey, captureTsUs.toULong(), frameBytes)
                        if (report.skipped) {
                            framesSkipped.incrementAndGet()
                        }
                        if (report.drainStarted) {
                            drains.incrementAndGet()
                        }
                        if (report.tooLarge > 0u) {
                            tooLarge.addAndGet(report.tooLarge.toLong())
                        }
                        if (report.queueOverflows > 0u || report.sendErrors > 0u) {
                            queueOverflows.addAndGet(report.queueOverflows.toLong())
                            sendErrors.addAndGet(report.sendErrors.toLong())
                        }
                        if (report.requestKeyframe) {
                            encRef.get()?.requestKeyFrame()
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

/** Righe di log tenute per la diagnosi chiesta dall'iPhone. */
private const val MAX_LOG_LINES = 400
