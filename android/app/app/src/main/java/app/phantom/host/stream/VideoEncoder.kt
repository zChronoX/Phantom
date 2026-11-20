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
            val captureTsUs = SystemClock.elapsedRealtimeNanos() / 1_000L
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

        private data class Attempt(
            val codec: VideoCodec,
            val fps: Int,
            val setVbr: Boolean,
            val lowLatencyHints: Boolean,
            val maxLongSideOverride: Int? = null,
        )

        fun create(
            width: Int,
            height: Int,
            requestedFps: Int,
            preferHevc: Boolean,
            initialBitrateBps: Int,
            onLog: (String) -> Unit,
        ): VideoEncoder {
            val attempts = ArrayList<Attempt>(8)
            val firstCodec = if (preferHevc) VideoCodec.H265 else VideoCodec.H264
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

        private fun openConfigured(
            reqWidth: Int,
            reqHeight: Int,
            reqFps: Int,
            codecType: VideoCodec,
            bitrateBps: Int,
            setVbr: Boolean,
            lowLatencyHints: Boolean,
            maxLongSideOverride: Int?,
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
                var baseW = reqWidth
                var baseH = reqHeight
                if (maxLongSideOverride != null && maxOf(baseW, baseH) > maxLongSideOverride) {
                    val (fw, fh) = HostFormatHelpers.fitWithinBounds(baseW, baseH, maxLongSideOverride, maxLongSideOverride)
                    baseW = fw
                    baseH = fh
                }

                val videoCaps = try {
                    mc.codecInfo.getCapabilitiesForType(mime).videoCapabilities
                } catch (_: Throwable) {
                    null
                }

                val (actualW, actualH) = fitSizeToCapabilities(baseW, baseH, videoCaps)
                if (actualW != reqWidth || actualH != reqHeight) {
                    onLog("Risoluzione adattata all'encoder ${codecType.name}: ${reqWidth}×${reqHeight} → ${actualW}×${actualH}")
                }

                val actualFps = clampFpsToCapabilities(actualW, actualH, reqFps, videoCaps)
                val clampedBitrate = clampBitrateToCapabilities(bitrateBps, videoCaps)

                val format = MediaFormat.createVideoFormat(mime, actualW, actualH).apply {
                    setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                    setInteger(MediaFormat.KEY_BIT_RATE, clampedBitrate)
                    setInteger(MediaFormat.KEY_FRAME_RATE, actualFps)
                    setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, KEYFRAME_INTERVAL_SEC)
                    setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000L)
                    if (setVbr) {
                        setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
                    }
                    if (lowLatencyHints) {
                        setInteger(MediaFormat.KEY_PRIORITY, 0) // 0 = realtime
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                            setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
                        }
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            setInteger(MediaFormat.KEY_LATENCY, 1)
                        }
                    }
                }

                mc.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                surface = mc.createInputSurface()
                mc.start()
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
        ): Pair<Int, Int> {
            if (caps == null) {
                return HostFormatHelpers.align16(width) to HostFormatHelpers.align16(height)
            }
            val align = maxOf(16, caps.widthAlignment, caps.heightAlignment)
            val maxW = caps.supportedWidths.upper
            val maxH = caps.supportedHeights.upper
            var (w, h) = HostFormatHelpers.fitWithinBounds(width, height, maxW, maxH, align)

            // Alcuni encoder (es. Qualcomm Snapdragon 695) limitano i macroblocchi totali (W×H)
            // oltre ai singoli lati: scala progressivamente finché `isSizeSupported` è vero.
            var steps = 0
            while (!caps.isSizeSupported(w, h) && minOf(w, h) > 320 && steps < 10) {
                w = HostFormatHelpers.alignDown((w * 0.9).toInt(), align).coerceAtLeast(caps.supportedWidths.lower)
                h = HostFormatHelpers.alignDown((h * 0.9).toInt(), align).coerceAtLeast(caps.supportedHeights.lower)
                steps++
            }
            return w to h
        }

        private fun clampFpsToCapabilities(
            width: Int,
            height: Int,
            requestedFps: Int,
            caps: MediaCodecInfo.VideoCapabilities?,
        ): Int {
            if (caps == null) return requestedFps
            return try {
                val range = caps.getSupportedFrameRatesFor(width, height)
                val maxSupported = range.upper.toInt().coerceAtLeast(15)
                requestedFps.coerceAtMost(maxSupported).coerceAtLeast(15)
            } catch (_: Throwable) {
                requestedFps
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
