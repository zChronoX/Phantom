package app.phantom.host.stream

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.CodecConfigCache
import app.phantom.host.core.HostFormatHelpers
import uniffi.phantom_ffi.VideoCodec

/**
 * Encoder hardware `MediaCodec` a superficie con adattamento automatico alle `VideoCapabilities`
 * reali del SoC (es. limite lato lungo 1920 px su Snapdragon 695), parametri a bassa latenza
 * (priorità realtime, zero B-frame, I-frame distanziati) e fallback progressivo per la sicurezza.
 */
class VideoEncoder private constructor(
    private val codec: MediaCodec,
    val inputSurface: Surface,
    val actualCodec: VideoCodec,
    val actualFps: Int,
    val actualWidth: Int,
    val actualHeight: Int,
) {
    private val configCache = CodecConfigCache()

    fun requestKeyFrame() {
        try {
            val params = Bundle().apply {
                putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
            }
            codec.setParameters(params)
        } catch (_: Throwable) {
        }
    }

    /**
     * Sospende (`true`) o riprende la codifica: da sospeso l'encoder scarta i frame che arrivano dalla Surface invece di
     * codificarli (iPhone in background: niente lavoro e niente batteria spesi per un video che nessuno guarda). Alla
     * ripresa serve un keyframe, che chiede la sessione.
     */
    fun setSuspended(suspended: Boolean) {
        try {
            val params = Bundle().apply {
                putInt(MediaCodec.PARAMETER_KEY_SUSPEND, if (suspended) 1 else 0)
            }
            codec.setParameters(params)
        } catch (_: Throwable) {
        }
    }

    fun setBitrate(bps: Int) {
        try {
            val params = Bundle().apply {
                putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bps.coerceAtLeast(500_000))
            }
            codec.setParameters(params)
        } catch (_: Throwable) {
        }
    }

    fun drainLoop(
        isActive: () -> Boolean,
        onFrame: (data: ByteArray, isKeyFrame: Boolean, captureTsUs: Long) -> Unit,
    ) {
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_DISPLAY)
        } catch (_: Throwable) {
        }
        val info = MediaCodec.BufferInfo()
        configCache.reset()
        while (isActive()) {
            val outIndex = try {
                codec.dequeueOutputBuffer(info, 5_000L)
            } catch (_: Throwable) {
                break
            }
            if (outIndex < 0) continue

            var bytes: ByteArray? = null
            val flags = info.flags
            try {
                if (info.size > 0) {
                    val buf = codec.getOutputBuffer(outIndex)
                    if (buf != null) {
                        buf.position(info.offset)
                        buf.limit(info.offset + info.size)
                        val copy = ByteArray(info.size)
                        buf.get(copy)
                        bytes = copy
                    }
                }
            } finally {
                try {
                    codec.releaseOutputBuffer(outIndex, false)
                } catch (_: Throwable) {
                }
            }

            val raw = bytes ?: continue
            val isConfig = (flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
            val isKey = (flags and MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0
            val prepared = configCache.prepareFrame(raw, isConfig = isConfig, isKeyFrame = isKey) ?: continue
            val captureTsUs = HostFormatHelpers.captureTimeUs(
                presentationTimeUs = info.presentationTimeUs,
                nowMonotonicUs = System.nanoTime() / 1_000L,
                nowElapsedUs = SystemClock.elapsedRealtimeNanos() / 1_000L,
            )
            onFrame(prepared, isKey, captureTsUs)
        }
    }

    fun release() {
        try {
            codec.stop()
        } catch (_: Throwable) {
        }
        try {
            inputSurface.release()
        } catch (_: Throwable) {
        }
        try {
            codec.release()
        } catch (_: Throwable) {
        }
    }

    companion object {
        // I-frame periodici lunghi (10 s, stile scrcpy): Phantom usa già FEC XOR + NACK + keyframe
        // on-demand, evitando i micro-lag e gli sprechi di banda dei keyframe ogni 2 secondi.
        private const val KEYFRAME_INTERVAL_SEC = 10
        /** Limiti di quantizzazione (H.264 e HEVC, scala 0–51) con gli hint a bassa latenza, da Android 12. */
        private const val QP_MIN = 12
        private const val QP_MAX = 45

        private data class Attempt(
            val codec: VideoCodec,
            val fps: Int,
            val setVbr: Boolean,
            val lowLatencyHints: Boolean,
            val maxLongSideOverride: Int? = null,
            val intraRefresh: Boolean = false,
            /** Limiti dell'encoder presi alla lettera (orizzontali): ultima spiaggia se la misura ruotata è rifiutata. */
            val strictOrientation: Boolean = false,
        )

        fun create(
            width: Int,
            height: Int,
            requestedFps: Int,
            preferHevc: Boolean,
            initialBitrateBps: Int,
            intraRefresh: Boolean = false,
            onLog: (String) -> Unit,
        ): VideoEncoder {
            val attempts = ArrayList<Attempt>(9)
            val firstCodec = if (preferHevc) VideoCodec.H265 else VideoCodec.H264
            if (intraRefresh) {
                // Se l'encoder non accetta l'intra-refresh si ricade sui tentativi normali
                attempts.add(Attempt(firstCodec, requestedFps, setVbr = true, lowLatencyHints = true, intraRefresh = true))
            }
            attempts.add(Attempt(firstCodec, requestedFps, setVbr = true, lowLatencyHints = true))
            if (requestedFps > 60) {
                attempts.add(Attempt(firstCodec, 60, setVbr = true, lowLatencyHints = true))
            }
            // Se il driver rifiuta i flag low-latency su HEVC/H264, ritenta pulito con lo stesso codec
            attempts.add(Attempt(firstCodec, minOf(requestedFps, 60), setVbr = true, lowLatencyHints = false))
            if (preferHevc) {
                attempts.add(Attempt(VideoCodec.H264, minOf(requestedFps, 60), setVbr = true, lowLatencyHints = true))
                attempts.add(Attempt(VideoCodec.H264, minOf(requestedFps, 60), setVbr = true, lowLatencyHints = false))
            }
            // Fallback compatibili con encoder limitati (es. Qualcomm 1080p max o emulatori)
            attempts.add(Attempt(VideoCodec.H264, 60, setVbr = false, lowLatencyHints = false, maxLongSideOverride = 1920))
            attempts.add(Attempt(VideoCodec.H264, 30, setVbr = false, lowLatencyHints = false, maxLongSideOverride = 1600))
            attempts.add(Attempt(VideoCodec.H264, 30, setVbr = false, lowLatencyHints = false, maxLongSideOverride = 1280))
            // Ultima spiaggia: i limiti dichiarati presi alla lettera (risoluzione più bassa ma sicuramente accettata)
            attempts.add(Attempt(firstCodec, minOf(requestedFps, 60), setVbr = true, lowLatencyHints = false, strictOrientation = true))
            if (preferHevc) {
                attempts.add(Attempt(VideoCodec.H264, minOf(requestedFps, 60), setVbr = true, lowLatencyHints = false, strictOrientation = true))
            }

            var lastError: Throwable? = null
            for (attempt in attempts) {
                try {
                    return openConfigured(
                        reqWidth = width,
                        reqHeight = height,
                        reqFps = attempt.fps,
                        codecType = attempt.codec,
                        bitrateBps = initialBitrateBps,
                        setVbr = attempt.setVbr,
                        lowLatencyHints = attempt.lowLatencyHints,
                        maxLongSideOverride = attempt.maxLongSideOverride,
                        intraRefresh = attempt.intraRefresh,
                        strictOrientation = attempt.strictOrientation,
                        onLog = onLog,
                    )
                } catch (t: Throwable) {
                    lastError = t
                    val detail = t.message?.takeIf { it.isNotBlank() } ?: t.javaClass.simpleName
                    onLog("Fallback encoder dopo rifiuto ${attempt.codec.name}@${attempt.fps}fps: $detail")
                }
            }
            throw IllegalStateException("Impossibile avviare MediaCodec: ${lastError?.javaClass?.simpleName}: ${lastError?.message}", lastError)
        }

        @Volatile
        private var recoveryCapsLogged = false

        /**
         * Una volta per processo, scrive nel log i parametri «vendor» dell'encoder che servono a recuperare le perdite
         * senza keyframe interi (LTR, riferimenti a lungo termine) e quelli di intra-refresh e QP: è il dato che manca
         * per la prossima voce della roadmap (recupero con LTR), da leggere nella diagnosi.
         */
        private fun logRecoveryCapabilities(mc: MediaCodec, onLog: (String) -> Unit) {
            if (recoveryCapsLogged || Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
            recoveryCapsLogged = true
            val params = try {
                mc.supportedVendorParameters
            } catch (_: Throwable) {
                emptyList()
            }
            val keys = listOf("ltr", "refresh", "qp", "low-latency", "intra", "slice")
            val useful = params.filter { p -> keys.any { p.contains(it, ignoreCase = true) } }
            onLog(
                "Parametri dell'encoder ${mc.name} per il recupero (LTR, intra-refresh, QP): " +
                    useful.joinToString(", ").ifEmpty { "nessuno" } + " · ${params.size} parametri vendor in tutto",
            )
        }

        internal fun isQualcommEncoder(name: String): Boolean =
            name.startsWith("c2.qti.", ignoreCase = true) || name.startsWith("OMX.qcom.", ignoreCase = true)

        private fun openConfigured(
            reqWidth: Int,
            reqHeight: Int,
            reqFps: Int,
            codecType: VideoCodec,
            bitrateBps: Int,
            setVbr: Boolean,
            lowLatencyHints: Boolean,
            maxLongSideOverride: Int?,
            intraRefresh: Boolean,
            strictOrientation: Boolean,
            onLog: (String) -> Unit,
        ): VideoEncoder {
            val mime = if (codecType == VideoCodec.H265) {
                MediaFormat.MIMETYPE_VIDEO_HEVC
            } else {
                MediaFormat.MIMETYPE_VIDEO_AVC
            }

            val mc = MediaCodec.createEncoderByType(mime)
            var surface: Surface? = null
            try {
                val videoCaps = try {
                    mc.codecInfo.getCapabilitiesForType(mime).videoCapabilities
                } catch (_: Throwable) {
                    null
                }

                val (actualW, actualH) = fitSizeToCapabilities(reqWidth, reqHeight, videoCaps, maxLongSideOverride, strictOrientation)
                if (actualW != reqWidth || actualH != reqHeight) {
                    onLog("Risoluzione adattata all'encoder ${codecType.name}: ${reqWidth}×${reqHeight} → ${actualW}×${actualH}")
                }

                val actualFps = clampFpsToCapabilities(actualW, actualH, reqFps, videoCaps)
                if (actualFps < reqFps) {
                    onLog("Frequenza adattata all'encoder ${codecType.name} a ${actualW}×${actualH}: $reqFps → $actualFps fps")
                }
                val clampedBitrate = clampBitrateToCapabilities(bitrateBps, videoCaps)

                val format = MediaFormat.createVideoFormat(mime, actualW, actualH).apply {
                    setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                    setInteger(MediaFormat.KEY_BIT_RATE, clampedBitrate)
                    setInteger(MediaFormat.KEY_FRAME_RATE, actualFps)
                    setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, KEYFRAME_INTERVAL_SEC)
                    setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000L)
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        // Lo schermo del Redmi va a 120 Hz: senza tetto il display virtuale manda fino a 120 frame al
                        // secondo a un encoder configurato per 60, che ne regge ~70 a 864×1920. I frame in più si
                        // accodavano: 232 ms di «codifica» nella diagnosi del 25/11. Con il tetto la sorgente scarta
                        // quelli di troppo prima dell'encoder (come `--max-fps` di scrcpy).
                        setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, actualFps.toFloat())
                    }
                    if (setVbr) {
                        setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
                    }
                    if (intraRefresh) {
                        // Rinfresco intra distribuito su un secondo di frame: niente picchi dei keyframe periodici
                        setInteger(MediaFormat.KEY_INTRA_REFRESH_PERIOD, actualFps)
                    }
                    if (lowLatencyHints) {
                        setInteger(MediaFormat.KEY_PRIORITY, 0) // 0 = realtime
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                            setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
                        }
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            setInteger(MediaFormat.KEY_LATENCY, 1)
                        }
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                            // Limiti di quantizzazione: sotto QP_MIN l'immagine non migliora a vista e un'interfaccia
                            // ferma si mangerebbe bit (e coda) per niente; sopra QP_MAX compaiono i blocchi grossi. Il
                            // tetto è largo apposta: più stretto, su una rete debole l'encoder sforerebbe il bitrate.
                            // Gli encoder che non conoscono le chiavi le ignorano.
                            setInteger(MediaFormat.KEY_VIDEO_QP_MIN, QP_MIN)
                            setInteger(MediaFormat.KEY_VIDEO_QP_MAX, QP_MAX)
                        }
                        if (isQualcommEncoder(mc.name)) {
                            // Modalità a bassa latenza degli encoder Qualcomm (Snapdragon, es. Redmi Note 11 Pro 5G):
                            // consegna ogni frame appena codificato invece di trattenerlo per il rate control.
                            // Se il codec rifiuta la chiave, il tentativo successivo senza hint la omette.
                            setInteger("vendor.qti-ext-enc-low-latency.enable", 1)
                        }
                    }
                }

                mc.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                surface = mc.createInputSurface()
                mc.start()
                val qpNote = if (lowLatencyHints && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) " · QP $QP_MIN–$QP_MAX" else ""
                onLog("Encoder ${mc.name} · ${actualW}×${actualH} @ $actualFps fps${if (lowLatencyHints) " · bassa latenza" else ""}$qpNote")
                if (intraRefresh) onLog("Intra-refresh attivo (${codecType.name}, periodo $actualFps frame)")
                logRecoveryCapabilities(mc, onLog)
                return VideoEncoder(
                    codec = mc,
                    inputSurface = surface,
                    actualCodec = codecType,
                    actualFps = actualFps,
                    actualWidth = actualW,
                    actualHeight = actualH,
                )
            } catch (t: Throwable) {
                try {
                    surface?.release()
                } catch (_: Throwable) {
                }
                try {
                    mc.release()
                } catch (_: Throwable) {
                }
                throw t
            }
        }

        private fun fitSizeToCapabilities(
            width: Int,
            height: Int,
            caps: MediaCodecInfo.VideoCapabilities?,
            maxLongSideOverride: Int?,
            strictOrientation: Boolean = false,
        ): Pair<Int, Int> {
            // I limiti dell'encoder sono dichiarati «in orizzontale» (es. 1920×1088 sullo Snapdragon 695): applicarli
            // così a uno schermo verticale (1080×2400) limitava l'altezza a 1088 e dava un video di 488×1088.
            // Si ragiona invece per lato lungo / lato corto, come fa scrcpy, e si accetta anche la misura ruotata.
            var maxLong = maxLongSideOverride ?: Int.MAX_VALUE
            var maxShort = maxLongSideOverride ?: Int.MAX_VALUE
            if (caps == null) {
                val (w, h) = orient(width, height, maxLong, maxShort)
                return HostFormatHelpers.fitEncoderSize(width, height, w, h, alignment = 16)
            }
            val align = maxOf(8, caps.widthAlignment, caps.heightAlignment)
            val upperW = caps.supportedWidths.upper
            val upperH = caps.supportedHeights.upper
            if (strictOrientation) {
                return HostFormatHelpers.fitEncoderSize(
                    width,
                    height,
                    minOf(maxLong, upperW),
                    minOf(maxLong, upperH),
                    align,
                ) { w, h -> safeSupported(caps, w, h) }
            }
            maxLong = minOf(maxLong, maxOf(upperW, upperH))
            maxShort = minOf(maxShort, minOf(upperW, upperH))
            val (maxW, maxH) = orient(width, height, maxLong, maxShort)
            return HostFormatHelpers.fitEncoderSize(width, height, maxW, maxH, align) { w, h ->
                try {
                    caps.isSizeSupported(w, h) || caps.isSizeSupported(h, w)
                } catch (_: Throwable) {
                    true
                }
            }
        }

        /** Limiti (larghezza, altezza) per una sorgente orientata come `width`×`height`. */
        private fun orient(width: Int, height: Int, maxLong: Int, maxShort: Int): Pair<Int, Int> =
            if (height > width) maxShort to maxLong else maxLong to maxShort

        private fun clampFpsToCapabilities(
            width: Int,
            height: Int,
            requestedFps: Int,
            caps: MediaCodecInfo.VideoCapabilities?,
        ): Int {
            if (caps == null) return requestedFps
            // Le frequenze dichiarate valgono per le misure «in orizzontale»: per uno schermo verticale si interroga
            // la misura ruotata, altrimenti la chiamata fallisce e si perde l'informazione.
            val rotated = !safeSupported(caps, width, height) && safeSupported(caps, height, width)
            val qw = if (rotated) height else width
            val qh = if (rotated) width else height
            val declaredMax = try {
                caps.getSupportedFrameRatesFor(qw, qh).upper.toInt().coerceAtLeast(15)
            } catch (_: Throwable) {
                requestedFps
            }
            val capped = requestedFps.coerceAtMost(declaredMax).coerceAtLeast(15)
            return HostFormatHelpers.realisticFps(capped) { fps -> coversPerformance(caps, qw, qh, fps) }
        }

        private fun safeSupported(caps: MediaCodecInfo.VideoCapabilities, w: Int, h: Int): Boolean = try {
            caps.isSizeSupported(w, h)
        } catch (_: Throwable) {
            false
        }

        /**
         * `true`/`false` se il SoC dichiara (o misura) di reggere `fps` a questa risoluzione, `null` se non lo dice.
         * Prima i `PerformancePoint` di Android 10+, poi le frequenze misurate dal produttore (`achievable`).
         */
        private fun coversPerformance(caps: MediaCodecInfo.VideoCapabilities, width: Int, height: Int, fps: Int): Boolean? {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                try {
                    val points = caps.supportedPerformancePoints
                    if (!points.isNullOrEmpty()) {
                        val wanted = MediaCodecInfo.VideoCapabilities.PerformancePoint(width, height, fps)
                        return points.any { it.covers(wanted) }
                    }
                } catch (_: Throwable) {
                }
            }
            return try {
                val achievable = caps.getAchievableFrameRatesFor(width, height) ?: return null
                achievable.upper >= fps * 0.9
            } catch (_: Throwable) {
                null
            }
        }

        private fun clampBitrateToCapabilities(
            bitrateBps: Int,
            caps: MediaCodecInfo.VideoCapabilities?,
        ): Int {
            val safe = bitrateBps.coerceAtLeast(500_000)
            if (caps == null) return safe
            return try {
                val range = caps.bitrateRange
                safe.coerceIn(range.lower.coerceAtLeast(500_000), range.upper.coerceAtLeast(500_000))
            } catch (_: Throwable) {
                safe
            }
        }
    }
}
