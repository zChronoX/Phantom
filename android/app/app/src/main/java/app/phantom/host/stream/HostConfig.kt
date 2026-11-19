package app.phantom.host.stream

enum class CaptureMode(val label: String) {
    ROOT("Root (cattura + tocco/tastiera)"),
    MEDIA_PROJECTION("Standard senza root (solo video)"),
}

enum class StreamProfile(val maxShortSide: Int, val fps: Int, val label: String) {
    P1080_120(1080, 120, "1080p · 120 fps (predefinito)"),
    P1080_60(1080, 60, "1080p · 60 fps (bilanciato)"),
    P720_60(720, 60, "720p · 60 fps (leggero)"),
}

data class HostRunConfig(
    val port: Int = 47800,
    val profile: StreamProfile = StreamProfile.P1080_120,
    val preferHevc: Boolean = true,
    val mode: CaptureMode = CaptureMode.ROOT,
)
