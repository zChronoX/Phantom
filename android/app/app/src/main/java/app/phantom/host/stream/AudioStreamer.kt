package app.phantom.host.stream

import android.annotation.SuppressLint
import android.content.AttributionSource
import android.content.Context
import android.content.ContextWrapper
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
import android.os.Process
import android.os.SystemClock
import app.phantom.host.core.AudioSampleClock
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
        // Priorità audio: con l'encoder video a URGENT_DISPLAY, un thread a priorità normale arrivava in ritardo a
        // leggere l'AudioRecord, che perdeva campioni (buchi e gracchii).
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        } catch (_: Throwable) {
        }
        val frameBytes = SAMPLES_PER_FRAME * channels * 2
        val pcmChunk = ByteArray(frameBytes)
        val info = MediaCodec.BufferInfo()
        val clock = AudioSampleClock(sampleRate)
        var cachedAsc: ByteArray? = null
        var lastAscSentMs = 0L
        // L'AudioSpecificConfig è un datagramma come gli altri e si può perdere: senza, l'iPhone non decodifica.
        // Lo si ripete subito con i primi pacchetti e poi ogni secondo.
        var ascRepeatsLeft = 3

        fun drainOutput() {
            while (running.get()) {
                val outIdx = encoder.dequeueOutputBuffer(info, 0L)
                if (outIdx < 0) return
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
                            if (asc != null && (ascRepeatsLeft > 0 || nowMs - lastAscSentMs >= ASC_REPEAT_MS)) {
                                if (ascRepeatsLeft > 0) ascRepeatsLeft--
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

        try {
            recorder.startRecording()
            encoder.start()
            outer@ while (running.get()) {
                // Un pacchetto AAC = esattamente 1024 campioni: si legge il blocco intero.
                var filled = 0
                while (filled < frameBytes) {
                    if (!running.get()) break@outer
                    val n = recorder.read(pcmChunk, filled, frameBytes - filled)
                    if (n < 0) break@outer
                    filled += n
                }
                val ptsUs = clock.onChunk(SAMPLES_PER_FRAME, SystemClock.elapsedRealtimeNanos() / 1_000L)

                // Il blocco va consegnato all'encoder per intero: prima, se l'encoder non aveva un buffer libero
                // entro 10 ms, i campioni venivano buttati (un buco di 21 ms nell'audio).
                var queued = false
                while (!queued && running.get()) {
                    val inIdx = encoder.dequeueInputBuffer(5_000L)
                    if (inIdx >= 0) {
                        val inBuf = encoder.getInputBuffer(inIdx)
                        if (inBuf != null) {
                            inBuf.clear()
                            val toCopy = frameBytes.coerceAtMost(inBuf.remaining())
                            inBuf.put(pcmChunk, 0, toCopy)
                            encoder.queueInputBuffer(inIdx, 0, toCopy, ptsUs, 0)
                        } else {
                            encoder.queueInputBuffer(inIdx, 0, 0, ptsUs, 0)
                        }
                        queued = true
                    }
                    drainOutput()
                }
                drainOutput()
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
        private const val ASC_REPEAT_MS = 1_000L

        /** Buffer interno dell'`AudioRecord`: 250 ms di margine se il thread arriva in ritardo (non aggiunge latenza). */
        private const val RECORD_BUFFER_BYTES = SAMPLE_RATE / 4 * CHANNELS * 2

        @SuppressLint("MissingPermission", "NewApi")
        fun createForRoot(onLog: (String) -> Unit): AudioStreamer? {
            // In `app_process64` (senza `ActivityThread`/`Context`), `AudioRecord.Builder.setContext`
            // con `AttributionSource` è disponibile da Android 12 (API 31): su Android < 12
            // (incluso LDPlayer 9 / Android 9) la chiamata nativa va in crash con `opPackageName == null`.
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
                onLog("Audio di sistema root richiede Android 12+ (streaming solo video)")
                return null
            }
            return createWithBuilder(onLog) { minBuf ->
                AudioRecord.Builder()
                    .setContext(RootAudioContext())
                    .setAudioSource(MediaRecorder.AudioSource.REMOTE_SUBMIX)
                    .setAudioFormat(pcmFormat())
                    .setBufferSizeInBytes(minBuf)
                    .build()
            }
        }

        @SuppressLint("NewApi")
        private class RootAudioContext : ContextWrapper(null) {
            private val pkgName: String = if (Process.myUid() == 0) "root" else "com.android.shell"

            override fun getPackageName(): String = pkgName

            override fun getOpPackageName(): String = pkgName

            override fun getAttributionTag(): String? = null

            override fun getApplicationContext(): Context = this

            override fun getAttributionSource(): AttributionSource {
                return AttributionSource.Builder(Process.myUid())
                    .setPackageName(pkgName)
                    .build()
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
                ).coerceAtLeast(SAMPLES_PER_FRAME * CHANNELS * 4).coerceAtLeast(RECORD_BUFFER_BYTES)

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
