package app.phantom.host

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.StreamProfile
import java.util.Locale

/**
 * UI nativa e leggera (zero Jetpack Compose / AppCompat) pensata per rimanere veloce e reattiva su Snapdragon 695.
 */
class MainActivity : Activity() {
    private lateinit var statusBadge: TextView
    private lateinit var thermalBadge: TextView
    private lateinit var addrText: TextView
    private lateinit var certPreview: TextView
    private lateinit var statsText: TextView
    private lateinit var logsText: TextView
    private lateinit var toggleButton: Button
    private lateinit var copyAddrButton: Button
    private lateinit var copyCertButton: Button

    private lateinit var modeGroup: RadioGroup
    private lateinit var rootRadio: RadioButton
    private lateinit var projRadio: RadioButton
    private lateinit var profileGroup: RadioGroup
    private lateinit var p1080_120Radio: RadioButton
    private lateinit var p1080_60Radio: RadioButton
    private lateinit var p720_60Radio: RadioButton
    private lateinit var hevcSwitch: Switch

    private var selectedMode = CaptureMode.ROOT
    private var selectedProfile = StreamProfile.P1080_120
    private var preferHevc = true

    private val stateListener: (HostUiState) -> Unit = { s ->
        runOnUiThread { render(s) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        loadPreferences()
        buildUi()
        HostStateStore.addListener(stateListener)
    }

    override fun onDestroy() {
        HostStateStore.removeListener(stateListener)
        super.onDestroy()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_MEDIA_PROJECTION) {
            if (resultCode == RESULT_OK && data != null) {
                val intent = Intent(this, HostForegroundService::class.java).apply {
                    action = HostForegroundService.ACTION_START_PROJECTION
                    putExtra(HostForegroundService.EXTRA_PORT, 47800)
                    putExtra(HostForegroundService.EXTRA_PROFILE, selectedProfile.name)
                    putExtra(HostForegroundService.EXTRA_HEVC, preferHevc)
                    putExtra(HostForegroundService.EXTRA_RESULT_CODE, resultCode)
                    putExtra(HostForegroundService.EXTRA_RESULT_DATA, data)
                }
                startForegroundService(intent)
            } else {
                Toast.makeText(this, "Permesso cattura schermo annullato", Toast.LENGTH_SHORT).show()
            }
        }
    }

    private fun buildUi() {
        val dp = resources.displayMetrics.density
        fun px(v: Int): Int = (v * dp).toInt()

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.parseColor("#111317"))
            setPadding(px(16), px(20), px(16), px(24))
        }

        val titleRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        val title = TextView(this).apply {
            text = "Phantom Host"
            textSize = 22f
            setTypeface(Typeface.DEFAULT_BOLD)
            setTextColor(Color.WHITE)
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        thermalBadge = TextView(this).apply {
            textSize = 12f
            setTextColor(Color.parseColor("#A5F3FC"))
            setPadding(px(10), px(4), px(10), px(4))
            background = pill(Color.parseColor("#1E293B"))
        }
        titleRow.addView(title)
        titleRow.addView(thermalBadge)
        root.addView(titleRow)

        statusBadge = TextView(this).apply {
            textSize = 14f
            setTextColor(Color.parseColor("#E2E8F0"))
            setPadding(0, px(6), 0, px(14))
        }
        root.addView(statusBadge)

        // Scheda connessione per iPhone
        val connCard = card(px(12))
        connCard.addView(sectionHeader("Connessione iPhone (QUIC)"))
        addrText = TextView(this).apply {
            text = "–"
            textSize = 16f
            setTypeface(Typeface.MONOSPACE, Typeface.BOLD)
            setTextColor(Color.WHITE)
            setPadding(0, px(4), 0, px(8))
        }
        connCard.addView(addrText)

        certPreview = TextView(this).apply {
            text = "Certificato disponibile dopo l'avvio"
            textSize = 11f
            typeface = Typeface.MONOSPACE
            setTextColor(Color.parseColor("#94A3B8"))
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.MIDDLE
            setPadding(0, 0, 0, px(10))
        }
        connCard.addView(certPreview)

        val copyRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        copyAddrButton = Button(this).apply {
            text = "Copia IP"
            setOnClickListener {
                val s = HostStateStore.snapshot()
                val value = if (s.ipAddress.isNotBlank()) "${s.ipAddress}:${s.port}" else ":${s.port}"
                copyToClipboard("Phantom IP", value)
            }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginEnd = px(6)
            }
        }
        copyCertButton = Button(this).apply {
            text = "Copia Certificato"
            setOnClickListener {
                val cert = HostStateStore.snapshot().certificateHex
                if (cert.isNotBlank()) copyToClipboard("Phantom Cert", cert)
            }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginStart = px(6)
            }
        }
        copyRow.addView(copyAddrButton)
        copyRow.addView(copyCertButton)
        connCard.addView(copyRow)
        root.addView(connCard, marginBottom(px(12)))

        // Scheda controlli avvio
        val cfgCard = card(px(12))
        cfgCard.addView(sectionHeader("Modalità di cattura"))
        rootRadio = RadioButton(this).apply {
            text = CaptureMode.ROOT.label
            setTextColor(Color.WHITE)
            id = View.generateViewId()
        }
        projRadio = RadioButton(this).apply {
            text = CaptureMode.MEDIA_PROJECTION.label
            setTextColor(Color.WHITE)
            id = View.generateViewId()
        }
        modeGroup = RadioGroup(this).apply {
            addView(rootRadio)
            addView(projRadio)
            check(if (selectedMode == CaptureMode.ROOT) rootRadio.id else projRadio.id)
            setOnCheckedChangeListener { _, checkedId ->
                selectedMode = if (checkedId == rootRadio.id) CaptureMode.ROOT else CaptureMode.MEDIA_PROJECTION
                savePreferences()
            }
        }
        cfgCard.addView(modeGroup)

        cfgCard.addView(sectionHeader("Profilo video").apply { setPadding(0, px(10), 0, px(4)) })
        p1080_120Radio = RadioButton(this).apply {
            text = StreamProfile.P1080_120.label
            setTextColor(Color.WHITE)
            id = View.generateViewId()
        }
        p1080_60Radio = RadioButton(this).apply {
            text = StreamProfile.P1080_60.label
            setTextColor(Color.WHITE)
            id = View.generateViewId()
        }
        p720_60Radio = RadioButton(this).apply {
            text = StreamProfile.P720_60.label
            setTextColor(Color.WHITE)
            id = View.generateViewId()
        }
        profileGroup = RadioGroup(this).apply {
            addView(p1080_120Radio)
            addView(p1080_60Radio)
            addView(p720_60Radio)
            val checkedId = when (selectedProfile) {
                StreamProfile.P1080_120 -> p1080_120Radio.id
                StreamProfile.P1080_60 -> p1080_60Radio.id
                StreamProfile.P720_60 -> p720_60Radio.id
            }
            check(checkedId)
            setOnCheckedChangeListener { _, id ->
                selectedProfile = when (id) {
                    p1080_60Radio.id -> StreamProfile.P1080_60
                    p720_60Radio.id -> StreamProfile.P720_60
                    else -> StreamProfile.P1080_120
                }
                savePreferences()
            }
        }
        cfgCard.addView(profileGroup)

        hevcSwitch = Switch(this).apply {
            text = "Preferisci HEVC (fallback automatico ad H.264)"
            setTextColor(Color.WHITE)
            isChecked = preferHevc
            setPadding(0, px(10), 0, px(6))
            setOnCheckedChangeListener { _, checked ->
                preferHevc = checked
                savePreferences()
            }
        }
        cfgCard.addView(hevcSwitch)
        root.addView(cfgCard, marginBottom(px(12)))

        toggleButton = Button(this).apply {
            textSize = 16f
            setTypeface(Typeface.DEFAULT_BOLD)
            setTextColor(Color.WHITE)
            setPadding(px(16), px(14), px(16), px(14))
            setOnClickListener { onTogglePressed() }
        }
        root.addView(toggleButton, marginBottom(px(12)))

        val statsCard = card(px(12))
        statsCard.addView(sectionHeader("Telemetria live"))
        statsText = TextView(this).apply {
            textSize = 13f
            typeface = Typeface.MONOSPACE
            setTextColor(Color.parseColor("#CBD5E1"))
            setPadding(0, px(4), 0, 0)
        }
        statsCard.addView(statsText)
        root.addView(statsCard, marginBottom(px(12)))

        val logsCard = card(px(12))
        logsCard.addView(sectionHeader("Log"))
        logsText = TextView(this).apply {
            textSize = 11f
            typeface = Typeface.MONOSPACE
            setTextColor(Color.parseColor("#94A3B8"))
            setPadding(0, px(4), 0, 0)
        }
        logsCard.addView(logsText)
        root.addView(logsCard)

        val scroll = ScrollView(this).apply {
            setBackgroundColor(Color.parseColor("#111317"))
            addView(root)
        }
        setContentView(scroll)
    }

    private fun onTogglePressed() {
        val state = HostStateStore.snapshot()
        if (state.running) {
            startService(Intent(this, HostForegroundService::class.java).setAction(HostForegroundService.ACTION_STOP))
            return
        }

        requestNotificationPermissionIfNeeded()

        when (selectedMode) {
            CaptureMode.ROOT -> {
                val intent = Intent(this, HostForegroundService::class.java).apply {
                    action = HostForegroundService.ACTION_START_ROOT
                    putExtra(HostForegroundService.EXTRA_PORT, 47800)
                    putExtra(HostForegroundService.EXTRA_PROFILE, selectedProfile.name)
                    putExtra(HostForegroundService.EXTRA_HEVC, preferHevc)
                }
                startForegroundService(intent)
            }
            CaptureMode.MEDIA_PROJECTION -> {
                val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                @Suppress("DEPRECATION")
                startActivityForResult(mpm.createScreenCaptureIntent(), REQ_MEDIA_PROJECTION)
            }
        }
    }

    private fun requestNotificationPermissionIfNeeded() {
        if (Build.VERSION.SDK_INT >= 33) {
            if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIF)
            }
        }
    }

    private fun render(s: HostUiState) {
        statusBadge.text = "Stato: ${s.statusText}"
        val tempStr = s.batteryTempC?.let { String.format(Locale.US, "%.1f °C · ", it) } ?: ""
        thermalBadge.text = "$tempStr${s.thermalLabel}"

        addrText.text = if (s.ipAddress.isNotBlank()) "${s.ipAddress}:${s.port}" else "In attesa Wi-Fi (porta ${s.port})"
        certPreview.text = if (s.certificateHex.isNotBlank()) s.certificateHex else "Certificato effimero generato all'avvio"
        copyCertButton.isEnabled = s.certificateHex.isNotBlank()

        val controlsEnabled = !s.running
        rootRadio.isEnabled = controlsEnabled
        projRadio.isEnabled = controlsEnabled
        p1080_120Radio.isEnabled = controlsEnabled
        p1080_60Radio.isEnabled = controlsEnabled
        p720_60Radio.isEnabled = controlsEnabled
        hevcSwitch.isEnabled = controlsEnabled

        toggleButton.text = if (s.running) "FERMA HOST" else "AVVIA HOST"
        toggleButton.background = pill(
            if (s.running) Color.parseColor("#DC2626") else Color.parseColor("#16A34A"),
        )

        val prof = if (s.activeProfileText.isNotBlank()) s.activeProfileText else selectedProfile.label
        statsText.text = String.format(
            Locale.US,
            "Profilo: %s\nFPS: %.1f · Banda: %.2f Mbps\nTocchi: %d · Tasti: %d",
            prof,
            s.fps,
            s.mbps,
            s.touches,
            s.keys,
        )
        logsText.text = if (s.logs.isEmpty()) "(nessun evento)" else s.logs.takeLast(30).joinToString("\n")
    }

    private fun copyToClipboard(label: String, text: String) {
        val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        cm.setPrimaryClip(ClipData.newPlainText(label, text))
        Toast.makeText(this, "$label copiato", Toast.LENGTH_SHORT).show()
    }

    private fun loadPreferences() {
        val p = getSharedPreferences(PREFS_NAME, MODE_PRIVATE)
        val modeName = p.getString("mode", CaptureMode.ROOT.name)
        selectedMode = CaptureMode.entries.firstOrNull { it.name == modeName } ?: CaptureMode.ROOT
        val profName = p.getString("profile", StreamProfile.P1080_120.name)
        selectedProfile = StreamProfile.entries.firstOrNull { it.name == profName } ?: StreamProfile.P1080_120
        preferHevc = p.getBoolean("hevc", true)
    }

    private fun savePreferences() {
        getSharedPreferences(PREFS_NAME, MODE_PRIVATE).edit()
            .putString("mode", selectedMode.name)
            .putString("profile", selectedProfile.name)
            .putBoolean("hevc", preferHevc)
            .apply()
    }

    private fun card(padPx: Int): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(padPx, padPx, padPx, padPx)
        background = pill(Color.parseColor("#1B1F27"))
    }

    private fun sectionHeader(title: String): TextView = TextView(this).apply {
        text = title
        textSize = 12f
        setTypeface(Typeface.DEFAULT_BOLD)
        setTextColor(Color.parseColor("#93C5FD"))
    }

    private fun pill(color: Int): GradientDrawable = GradientDrawable().apply {
        setColor(color)
        cornerRadius = 14f * resources.displayMetrics.density
    }

    private fun marginBottom(px: Int): LinearLayout.LayoutParams = LinearLayout.LayoutParams(
        LinearLayout.LayoutParams.MATCH_PARENT,
        LinearLayout.LayoutParams.WRAP_CONTENT,
    ).apply {
        bottomMargin = px
    }

    companion object {
        private const val PREFS_NAME = "phantom_host_prefs"
        private const val REQ_MEDIA_PROJECTION = 1001
        private const val REQ_NOTIF = 1002
    }
}
