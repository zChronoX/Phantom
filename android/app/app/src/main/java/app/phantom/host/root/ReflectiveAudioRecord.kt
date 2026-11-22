package app.phantom.host.root

import android.annotation.SuppressLint
import android.content.AttributionSource
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.AudioRecord
import android.os.Build
import android.os.Looper
import android.os.Parcel
import java.lang.ref.WeakReference

/**
 * Crea un `AudioRecord` senza passare da `AudioRecord.Builder`, inizializzandolo via riflessione come fa il costruttore
 * di sistema (tecnica di scrcpy, `Workarounds.createAudioRecord`).
 *
 * Serve quando il builder richiede un contesto "da app vera" e fallisce nel worker `app_process` (alcune ROM
 * modificate e Android 17 con `NullPointerException`). Solo Android 12+ (firma di `native_setup` con `Parcel`).
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi", "SoonBlockedPrivateApi", "BlockedPrivateApi", "WrongConstant", "NewApi")
object ReflectiveAudioRecord {
    fun create(
        source: Int,
        sampleRate: Int,
        channelConfig: Int,
        channels: Int,
        channelMask: Int,
        encoding: Int,
        bufferSizeInBytes: Int,
    ): AudioRecord {
        require(Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) { "Richiede Android 12+" }
        val cls = AudioRecord::class.java
        val record = cls.getDeclaredConstructor(Long::class.javaPrimitiveType).apply { isAccessible = true }
            .newInstance(0L)

        fun setField(name: String, value: Any?) {
            cls.getDeclaredField(name).apply { isAccessible = true }.set(record, value)
        }

        setField("mRecordingState", AudioRecord.RECORDSTATE_STOPPED)
        setField("mInitializationLooper", Looper.myLooper() ?: Looper.getMainLooper())

        val attrsBuilder = AudioAttributes.Builder()
        AudioAttributes.Builder::class.java.getMethod("setInternalCapturePreset", Int::class.javaPrimitiveType)
            .invoke(attrsBuilder, source)
        val attributes = attrsBuilder.build()
        setField("mAudioAttributes", attributes)

        cls.getDeclaredMethod(
            "audioParamCheck",
            Int::class.javaPrimitiveType,
            Int::class.javaPrimitiveType,
            Int::class.javaPrimitiveType,
        ).apply { isAccessible = true }.invoke(record, source, sampleRate, encoding)
        setField("mChannelCount", channels)
        setField("mChannelMask", channelMask)

        val minBuf = AudioRecord.getMinBufferSize(sampleRate, channelConfig, encoding)
        val bufferSize = maxOf(bufferSizeInBytes, minBuf)
        cls.getDeclaredMethod("audioBuffSizeCheck", Int::class.javaPrimitiveType).apply { isAccessible = true }
            .invoke(record, bufferSize)

        val sampleRateArray = intArrayOf(sampleRate)
        val session = intArrayOf(AudioManager.AUDIO_SESSION_ID_GENERATE)
        val attribution: AttributionSource = WorkerEnvironment.context().attributionSource
        val asScoped = AttributionSource::class.java.getDeclaredMethod("asScopedParcelState").apply { isAccessible = true }
        val initResult = (asScoped.invoke(attribution) as AutoCloseable).use { state ->
            val parcel = state.javaClass.getDeclaredMethod("getParcel").apply { isAccessible = true }
                .invoke(state) as Parcel
            val intT = Int::class.javaPrimitiveType
            val longT = Long::class.javaPrimitiveType
            val baseTypes = arrayOf<Class<*>?>(
                Any::class.java, Any::class.java, IntArray::class.java,
                intT, intT, intT, intT, IntArray::class.java, Parcel::class.java, longT, intT,
            )
            val baseArgs = arrayOf<Any?>(
                WeakReference(record), attributes, sampleRateArray,
                channelMask, 0, record.audioFormat, bufferSize, session, parcel, 0L, 0,
            )
            // Android 14 ha aggiunto `halInputFlags` (int) in coda: proviamo prima la firma più recente.
            val withFlags = runCatching {
                cls.getDeclaredMethod("native_setup", *baseTypes, intT)
            }.getOrNull()
            if (withFlags != null) {
                withFlags.isAccessible = true
                withFlags.invoke(record, *baseArgs, 0) as Int
            } else {
                val legacy = cls.getDeclaredMethod("native_setup", *baseTypes).apply { isAccessible = true }
                legacy.invoke(record, *baseArgs) as Int
            }
        }
        if (initResult != AudioRecord.SUCCESS) {
            throw IllegalStateException("native_setup AudioRecord fallito (codice $initResult)")
        }

        setField("mSampleRate", sampleRateArray[0])
        setField("mSessionId", session[0])
        setField("mState", AudioRecord.STATE_INITIALIZED)
        return record
    }
}
