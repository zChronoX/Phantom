package app.phantom.host

import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.StreamProfile
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.CopyOnWriteArraySet

data class HostUiState(
    val running: Boolean = false,
    val starting: Boolean = false,
    val connected: Boolean = false,
    val mode: CaptureMode = CaptureMode.ROOT,
    val profile: StreamProfile = StreamProfile.P1080_120,
    val preferHevc: Boolean = true,
    val statusText: String = "Fermo",
    val ipAddress: String = "",
    val port: Int = 47800,
    val certificateHex: String = "",
    val activeProfileText: String = "",
    val fps: Double = 0.0,
    val mbps: Double = 0.0,
    val touches: Long = 0,
    val keys: Long = 0,
    val batteryTempC: Double? = null,
    val thermalLabel: String = "Normale",
    val logs: List<String> = emptyList(),
)

object HostStateStore {
    private val lock = Any()
    private val listeners = CopyOnWriteArraySet<(HostUiState) -> Unit>()
    private val timeFmt = SimpleDateFormat("HH:mm:ss", Locale.US)

    @Volatile
    private var state = HostUiState()

    fun snapshot(): HostUiState = state

    fun addListener(listener: (HostUiState) -> Unit) {
        listeners.add(listener)
        listener(state)
    }

    fun removeListener(listener: (HostUiState) -> Unit) {
        listeners.remove(listener)
    }

    fun update(transform: (HostUiState) -> HostUiState) {
        val next: HostUiState
        synchronized(lock) {
            next = transform(state)
            state = next
        }
        for (l in listeners) {
            l(next)
        }
    }

    fun addLog(message: String) {
        val line = "${timeFmt.format(Date())} · $message"
        update { cur ->
            val updated = (cur.logs + line).takeLast(60)
            cur.copy(logs = updated)
        }
    }
}
