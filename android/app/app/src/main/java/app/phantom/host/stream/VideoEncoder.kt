package app.phantom.host.stream

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import android.os.SystemClock
import android.view.Surface
import app.phantom.host.core.CodecConfigCache
import uniffi.phantom_ffi.VideoCodec

/**
 * Encoder hardware `MediaCodec` a superficie con parametri neutri e fallback automatico
 * (120 → 60 fps, HEVC → H.264) per la sicurezza dell'hardware.
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
        val info = MediaCodec.BufferInfo()
        configCache.reset()
        while (isActive()) {
            val outIndex = try {
                codec.dequeueOutputBuffer(info, 10_000L)
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
        private data class Attempt(val codec: VideoCodec, val fps: Int, val setVbr: Boolean)

        fun create(
            width: Int,
            height: Int,
            requestedFps: Int,
            preferHevc: Boolean,
            initialBitrateBps: Int,
            onLog: (String) -> Unit,
        ): VideoEncoder {
            val attempts = ArrayList<Attempt>(4)
            val firstCodec = if (preferHevc) VideoCodec.H265 else VideoCodec.H264
            attempts.add(Attempt(firstCodec, requestedFps, setVbr = true))
            if (requestedFps > 60) {
                attempts.add(Attempt(firstCodec, 60, setVbr = true))
            }
            if (preferHevc) {
                attempts.add(Attempt(VideoCodec.H264, minOf(requestedFps, 60), setVbr = true))
            }
            // Fallback finale ultra-compatibile (utile per emulatori come LDPlayer o encoder limitati)
            attempts.add(Attempt(VideoCodec.H264, 30, setVbr = false))

            var lastError: Throwable? = null
            for (attempt in attempts) {
                try {
                    return openConfigured(width, height, attempt.fps, attempt.codec, initialBitrateBps, attempt.setVbr)
                } catch (t: Throwable) {
                    lastError = t
                    onLog("Fallback encoder dopo rifiuto ${attempt.codec.name}@${attempt.fps}fps: ${t.message}")
                }
            }
            throw IllegalStateException("Impossibile avviare MediaCodec: ${lastError?.message}", lastError)
        }

        private fun openConfigured(
            width: Int,
            height: Int,
            fps: Int,
            codecType: VideoCodec,
            bitrateBps: Int,
            setVbr: Boolean,
        ): VideoEncoder {
            val mime = if (codecType == VideoCodec.H265) {
                MediaFormat.MIMETYPE_VIDEO_HEVC
            } else {
                MediaFormat.MIMETYPE_VIDEO_AVC
            }
            val format = MediaFormat.createVideoFormat(mime, width, height).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                setInteger(MediaFormat.KEY_BIT_RATE, bitrateBps.coerceAtLeast(500_000))
                setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
                setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000L)
                if (setVbr) {
                    setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
                }
            }

            val mc = MediaCodec.createEncoderByType(mime)
            var surface: Surface? = null
            try {
                mc.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                surface = mc.createInputSurface()
                mc.start()
                return VideoEncoder(
                    codec = mc,
                    inputSurface = surface,
                    actualCodec = codecType,
                    actualFps = fps,
                    actualWidth = width,
                    actualHeight = height,
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
    }
}
