package app.phantom.host.stream

import android.annotation.SuppressLint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.os.Build
import android.os.SystemClock
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Cattura l'audio di sistema su Android (via `REMOTE_SUBMIX` in modalità Root oppure `AudioPlaybackCapture`
 * in modalità MediaProjection su Android 10+) e lo codifica in AAC-LC 48 kHz stereo a bassa latenza.
 */
class AudioStreamer private constructor(
    private val recorder: AudioRecord,
    private val encoder: MediaCodec,
    val sampleRate: Int,
    val channels: Int,
) : AutoCloseable {
    private val running = AtomicBoolean(false)
    private var worker: Thread? = null

    fun start(onPacket: (data: ByteArray, isConfig: Boolean, captureTsUs: Long) -> Unit) {
        if (!running.compareAndSet(false, true)) return
        worker = Thread({
            runLoop(onPacket)
        }, "phantom-audio-enc").apply {
            isDaemon = true
            start()
        }
    }

    private fun runLoop(onPacket: (data: ByteArray, isConfig: Boolean, captureTsUs: Long) -> Unit) {
        val pcmChunk = ByteArray(SAMPLES_PER_FRAME * channels * 2)
        val info = MediaCodec.BufferInfo()
        var cachedAsc: ByteArray? = null
        var lastAscSentMs = 0L

        try {
            recorder.startRecording()
            encoder.start()
            while (running.get()) {
                val read = recorder.read(pcmChunk, 0, pcmChunk.size)
                if (read <= 0) {
                    if (read < 0) break
                    continue
                }
                val captureTsUs = SystemClock.elapsedRealtimeNanos() / 1_000L
                val inIdx = encoder.dequeueInputBuffer(10_000L)
                if (inIdx >= 0) {
                    val inBuf = encoder.getInputBuffer(inIdx)
                    if (inBuf != null) {
                        inBuf.clear()
                        val toCopy = read.coerceAtMost(inBuf.remaining())
                        inBuf.put(pcmChunk, 0, toCopy)
                        encoder.queueInputBuffer(inIdx, 0, toCopy, captureTsUs, 0)
                    } else {
                        encoder.queueInputBuffer(inIdx, 0, 0, captureTsUs, 0)
                    }
                }

                while (running.get()) {
                    val outIdx = encoder.dequeueOutputBuffer(info, 0L)
                    if (outIdx < 0) break
                    try {
                        val outBuf = encoder.getOutputBuffer(outIdx)
                        if (outBuf != null && info.size > 0) {
                            val bytes = ByteArray(info.size)
                            outBuf.position(info.offset)
                            outBuf.limit(info.offset + info.size)
                            outBuf.get(bytes)

                            val isConfig = (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
                            val nowMs = SystemClock.elapsedRealtime()
                            if (isConfig) {
                                cachedAsc = bytes
                                lastAscSentMs = nowMs
                                onPacket(bytes, true, info.presentationTimeUs)
                            } else {
                                val asc = cachedAsc
                                if (asc != null && nowMs - lastAscSentMs >= 2_000L) {
                                    lastAscSentMs = nowMs
                                    onPacket(asc, true, info.presentationTimeUs)
                                }
                                onPacket(bytes, false, info.presentationTimeUs)
                            }
                        }
                    } finally {
                        encoder.releaseOutputBuffer(outIdx, false)
                    }
                }
            }
        } catch (_: Throwable) {
        } finally {
            running.set(false)
            releaseSilently()
        }
    }

    override fun close() {
        running.set(false)
        try {
            recorder.stop()
        } catch (_: Throwable) {
        }
        try {
            worker?.join(400)
        } catch (_: Throwable) {
        }
        worker = null
        releaseSilently()
    }

    private fun releaseSilently() {
        try {
            recorder.release()
        } catch (_: Throwable) {
        }
        try {
            encoder.stop()
        } catch (_: Throwable) {
        }
        try {
            encoder.release()
        } catch (_: Throwable) {
        }
    }

    companion object {
        const val SAMPLE_RATE = 48_000
        const val CHANNELS = 2
        private const val BITRATE_BPS = 128_000
        private const val SAMPLES_PER_FRAME = 1024

        @SuppressLint("MissingPermission", "PrivateApi")
        fun createForRoot(onLog: (String) -> Unit): AudioStreamer? {
            // In `app_process64` (senza `ActivityThread`/`Context`), chiamare `AudioRecord.Builder.build()`
            // con `REMOTE_SUBMIX` su Android < 11 (incluso LDPlayer 9 / Android 9) provoca un crash nativo
            // in `libandroid_runtime` (`opPackageName == null`) non intercettabili da try/catch Java.
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                onLog("Audio di sistema root richiede Android 11+ (streaming solo video)")
                return null
            }
            if (!hasValidOpPackageName()) {
                onLog("Audio di sistema root non disponibile nel worker standalone (streaming solo video)")
                return null
            }
            return createWithBuilder(onLog) { minBuf ->
                AudioRecord.Builder()
                    .setAudioSource(MediaRecorder.AudioSource.REMOTE_SUBMIX)
                    .setAudioFormat(pcmFormat())
                    .setBufferSizeInBytes(minBuf)
                    .build()
            }
        }

        @SuppressLint("PrivateApi")
        private fun hasValidOpPackageName(): Boolean {
            return try {
                val at = Class.forName("android.app.ActivityThread")
                val pkg = at.getMethod("currentOpPackageName").invoke(null) as? String
                pkg != null && pkg.isNotBlank()
            } catch (_: Throwable) {
                false
            }
        }

        @SuppressLint("MissingPermission")
        fun createForProjection(projection: MediaProjection, onLog: (String) -> Unit): AudioStreamer? {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
            return createWithBuilder(onLog) { minBuf ->
                val cfg = AudioPlaybackCaptureConfiguration.Builder(projection)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                    .addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                    .build()
                AudioRecord.Builder()
                    .setAudioFormat(pcmFormat())
                    .setBufferSizeInBytes(minBuf)
                    .setAudioPlaybackCaptureConfig(cfg)
                    .build()
            }
        }

        private fun pcmFormat(): AudioFormat = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(SAMPLE_RATE)
            .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
            .build()

        private fun createWithBuilder(
            onLog: (String) -> Unit,
            recorderFactory: (minBuf: Int) -> AudioRecord,
        ): AudioStreamer? {
            var rec: AudioRecord? = null
            var codec: MediaCodec? = null
            return try {
                val minBuf = AudioRecord.getMinBufferSize(
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT,
                ).coerceAtLeast(SAMPLES_PER_FRAME * CHANNELS * 4)

                rec = recorderFactory(minBuf)
                if (rec.state != AudioRecord.STATE_INITIALIZED) {
                    rec.release()
                    onLog("Audio di sistema non inizializzato su questo dispositivo (solo video)")
                    return null
                }

                val format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, SAMPLE_RATE, CHANNELS).apply {
                    setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC)
                    setInteger(MediaFormat.KEY_BIT_RATE, BITRATE_BPS)
                    setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, SAMPLES_PER_FRAME * CHANNELS * 4)
                }
                codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                AudioStreamer(rec, codec, SAMPLE_RATE, CHANNELS)
            } catch (t: Throwable) {
                try {
                    rec?.release()
                } catch (_: Throwable) {
                }
                try {
                    codec?.release()
                } catch (_: Throwable) {
                }
                onLog("Audio non disponibile su questo dispositivo (solo video): ${t.javaClass.simpleName}")
                null
            }
        }
    }
}
