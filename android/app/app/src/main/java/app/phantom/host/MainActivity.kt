package app.phantom.host

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.text.Editable
import android.text.InputType
import android.text.TextUtils
import android.text.TextWatcher
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.PairingCodec
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.StreamProfile
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import java.util.Locale

/**
 * UI nativa e leggera (zero Jetpack Compose / AppCompat) con QR di accoppiamento 1-tap,
 * identità TLS persistente, rilevamento LAN + VPN/Tailscale e controllo remoto 5G/4G.
 */
class MainActivity : Activity() {
    private lateinit var statusBadge: TextView
    private lateinit var thermalBadge: TextView
    private lateinit var addrText: TextView
    private lateinit var remoteHintText: TextView
    private lateinit var remoteInput: EditText
    private lateinit var qrImage: ImageView
    private lateinit var fingerprintText: TextView
    private lateinit var certPreview: TextView
    private lateinit var statsText: TextView
    private lateinit var logsText: TextView
    private lateinit var toggleButton: Button
    private lateinit var copyAddrButton: Button
    private lateinit var copyCertButton: Button
    private lateinit var regenIdentityButton: Button

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
    private var customRemoteHost = ""
    private var lastQrUri = ""

    private val stateListener: (HostUiState) -> Unit = { s ->
        runOnUiThread { render(s) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        loadPreferences()
        buildUi()
        HostStateStore.addListener(stateListener)
        loadIdentityAndNetworkAsync()
    }

    override fun onDestroy() {
        HostStateStore.removeListener(stateListener)
        super.onDestroy()
    }

    private fun loadIdentityAndNetworkAsync() {
        Thread({
            try {
                val id = HostIdentityStore.loadOrCreate(applicationContext)
                val ifaces = HostForegroundService.collectActiveIpv4Interfaces()
                val lanIp = HostFormatHelpers.selectPreferredLanIpv4(ifaces)
                val vpnIp = HostFormatHelpers.selectPreferredVpnIpv4(ifaces)
                HostStateStore.update { cur ->
                    cur.copy(
                        ipAddress = cur.ipAddress.ifBlank { lanIp },
                        vpnIpAddress = vpnIp,
                        certificateDer = id.certDer,
                        certificateHex = HostFormatHelpers.toHex(id.certDer),
                        fingerprintHex = id.fingerprintHex,
                        p2pEndpointId = id.p2pEndpointId,
                    )
                }
            } catch (t: Throwable) {
                HostStateStore.addLog("Errore caricamento identità: ${t.message}")
            }
        }, "phantom-init-id").start()
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

        // Scheda accoppiamento QR & indirizzi LAN + 5G/Remoto
        val connCard = card(px(12))
        connCard.addView(sectionHeader("Accoppiamento rapido iPhone (QR · LAN + 5G/4G)"))

        addrText = TextView(this).apply {
            text = "–"
            textSize = 15f
            setTypeface(Typeface.MONOSPACE, Typeface.BOLD)
            setTextColor(Color.WHITE)
            setPadding(0, px(6), 0, px(2))
        }
        connCard.addView(addrText)

        remoteHintText = TextView(this).apply {
            textSize = 12f
            setTextColor(Color.parseColor("#86EFAC"))
            setPadding(0, 0, 0, px(8))
        }
        connCard.addView(remoteHintText)

        qrImage = ImageView(this).apply {
            scaleType = ImageView.ScaleType.FIT_CENTER
            setBackgroundColor(Color.WHITE)
            setPadding(px(8), px(8), px(8), px(8))
            layoutParams = LinearLayout.LayoutParams(px(210), px(210)).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                topMargin = px(4)
                bottomMargin = px(8)
            }
        }
        connCard.addView(qrImage)

        fingerprintText = TextView(this).apply {
            textSize = 12f
            typeface = Typeface.MONOSPACE
            gravity = Gravity.CENTER_HORIZONTAL
            setTextColor(Color.parseColor("#93C5FD"))
            setPadding(0, 0, 0, px(8))
        }
        connCard.addView(fingerprintText)

        connCard.addView(sectionHeader("Indirizzo remoto 5G/4G (opzionale: Tailscale o DDNS)").apply {
            setPadding(0, px(4), 0, px(4))
        })
        remoteInput = EditText(this).apply {
            hint = "es. 100.x.y.z:47800 o casa.duckdns.org:47800"
            setText(customRemoteHost)
            setTextColor(Color.WHITE)
            setHintTextColor(Color.parseColor("#64748B"))
            textSize = 13f
            typeface = Typeface.MONOSPACE
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            background = pill(Color.parseColor("#0F172A"))
            setPadding(px(10), px(8), px(10), px(8))
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
                override fun afterTextChanged(s: Editable?) {
                    customRemoteHost = s?.toString()?.trim().orEmpty()
                    savePreferences()
                    updateQrIfNeeded(HostStateStore.snapshot())
                }
            })
        }
        connCard.addView(remoteInput, marginBottom(px(10)))

        certPreview = TextView(this).apply {
            text = "Caricamento identità persistente…"
            textSize = 10f
            typeface = Typeface.MONOSPACE
            setTextColor(Color.parseColor("#94A3B8"))
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.MIDDLE
            setPadding(0, 0, 0, px(8))
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
                marginEnd = px(4)
            }
        }
        copyCertButton = Button(this).apply {
            text = "Copia Cert"
            setOnClickListener {
                val cert = HostStateStore.snapshot().certificateHex
                if (cert.isNotBlank()) copyToClipboard("Phantom Cert", cert)
            }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginStart = px(4)
                marginEnd = px(4)
            }
        }
        regenIdentityButton = Button(this).apply {
            text = "Nuova chiave"
            setOnClickListener { regenerateIdentity() }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginStart = px(4)
            }
        }
        copyRow.addView(copyAddrButton)
        copyRow.addView(copyCertButton)
        copyRow.addView(regenIdentityButton)
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

    private fun regenerateIdentity() {
        if (HostStateStore.snapshot().running) return
        Thread({
            try {
                val id = HostIdentityStore.regenerate(applicationContext)
                HostStateStore.update { cur ->
                    cur.copy(
                        certificateDer = id.certDer,
                        certificateHex = HostFormatHelpers.toHex(id.certDer),
                        fingerprintHex = id.fingerprintHex,
                        p2pEndpointId = id.p2pEndpointId,
                        p2pReady = false,
                    )
                }
                HostStateStore.addLog("Nuova identità TLS generata (ID ${id.fingerprintHex})")
                runOnUiThread {
                    Toast.makeText(this, "Nuova chiave generata: riscansiona il QR su iPhone", Toast.LENGTH_SHORT).show()
                }
            } catch (t: Throwable) {
                HostStateStore.addLog("Errore rigenerazione identità: ${t.message}")
            }
        }, "phantom-regen-id").start()
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

        addrText.text = if (s.ipAddress.isNotBlank()) "LAN: ${s.ipAddress}:${s.port}" else "In attesa Wi-Fi (porta ${s.port})"
        remoteHintText.text = when {
            s.p2pReady && s.p2pEndpointId.isNotBlank() ->
                "P2P integrato (5G/4G ovunque): ATTIVO · ID ${s.p2pEndpointId.take(12)}…"
            s.p2pEndpointId.isNotBlank() ->
                "P2P integrato (5G/4G) pronto nel QR · nessuna VPN necessaria"
            else ->
                "Inizializzazione identità P2P…"
        }

        fingerprintText.text = if (s.fingerprintHex.isNotBlank()) {
            "ID stabile: ${s.fingerprintHex}"
        } else {
            "Inizializzazione identità…"
        }
        certPreview.text = if (s.certificateHex.isNotBlank()) s.certificateHex else "Certificato in caricamento…"
        copyCertButton.isEnabled = s.certificateHex.isNotBlank()
        regenIdentityButton.isEnabled = !s.running

        updateQrIfNeeded(s)

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

    private fun effectiveRemoteAddress(s: HostUiState): String? {
        if (customRemoteHost.isNotBlank()) {
            return if (customRemoteHost.contains(':')) customRemoteHost else "$customRemoteHost:${s.port}"
        }
        if (s.vpnIpAddress.isNotBlank()) {
            return "${s.vpnIpAddress}:${s.port}"
        }
        return null
    }

    private fun updateQrIfNeeded(s: HostUiState) {
        if (s.certificateDer.isEmpty()) {
            qrImage.visibility = View.GONE
            return
        }
        val lan = s.ipAddress.takeIf { it.isNotBlank() }?.let { "$it:${s.port}" }
        val wan = effectiveRemoteAddress(s)
        val deviceName = Build.MODEL.ifBlank { "Android" }
        val uri = PairingCodec.buildPairingUri(
            name = deviceName,
            lan = lan,
            wan = wan,
            certDer = s.certificateDer,
            p2pId = s.p2pEndpointId.takeIf { it.isNotBlank() },
        )
        qrImage.visibility = View.VISIBLE
        if (uri == lastQrUri) return
        lastQrUri = uri
        try {
            qrImage.setImageBitmap(renderQrBitmap(uri, 512))
        } catch (_: Throwable) {
        }
    }

    private fun renderQrBitmap(content: String, sizePx: Int): Bitmap {
        val hints = mapOf(
            EncodeHintType.MARGIN to 1,
            EncodeHintType.CHARACTER_SET to "UTF-8",
        )
        val matrix = QRCodeWriter().encode(content, BarcodeFormat.QR_CODE, sizePx, sizePx, hints)
        val w = matrix.width
        val h = matrix.height
        val pixels = IntArray(w * h)
        for (y in 0 until h) {
            val row = y * w
            for (x in 0 until w) {
                pixels[row + x] = if (matrix.get(x, y)) Color.BLACK else Color.WHITE
            }
        }
        return Bitmap.createBitmap(pixels, w, h, Bitmap.Config.ARGB_8888)
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
        customRemoteHost = p.getString("remote_host", "").orEmpty().trim()
    }

    private fun savePreferences() {
        getSharedPreferences(PREFS_NAME, MODE_PRIVATE).edit()
            .putString("mode", selectedMode.name)
            .putString("profile", selectedProfile.name)
            .putBoolean("hevc", preferHevc)
            .putString("remote_host", customRemoteHost)
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
