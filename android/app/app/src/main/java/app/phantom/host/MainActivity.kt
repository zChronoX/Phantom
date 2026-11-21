package app.phantom.host

import android.Manifest
import android.app.Activity
import android.app.Dialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.text.Editable
import android.text.InputType
import android.text.TextUtils
import android.text.TextWatcher
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import app.phantom.host.core.HostFormatHelpers
import app.phantom.host.core.PairingCodec
import app.phantom.host.root.ShizukuLauncher
import app.phantom.host.stream.CaptureMode
import app.phantom.host.stream.StreamProfile
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import java.util.Locale

/**
 * UI nativa e leggera (zero Jetpack Compose / AppCompat) con tema monocromatico
 * Light/Dark (ADR-011), scheda stato principale, QR in modale dedicata e diagnostica collassabile.
 */
class MainActivity : Activity() {
    private lateinit var theme: HostTheme
    private var dp: Float = 1f

    // Header
    private lateinit var thermalBadge: TextView

    // Hero card
    private lateinit var statusDot: View
    private lateinit var statusHeadline: TextView
    private lateinit var statusSubline: TextView
    private lateinit var metricsBox: LinearLayout
    private lateinit var metricFpsValue: TextView
    private lateinit var metricMbpsValue: TextView
    private lateinit var metricInputValue: TextView
    private lateinit var toggleButton: Button
    private lateinit var pairButton: Button

    // Config card
    private lateinit var modeRootBtn: TextView
    private lateinit var modeShizukuBtn: TextView
    private lateinit var modeStandardBtn: TextView
    private lateinit var controlRow: ActionRow
    private lateinit var prof1080_120Btn: TextView
    private lateinit var prof1080_60Btn: TextView
    private lateinit var prof720_60Btn: TextView
    private lateinit var hevcSwitch: Switch
    private lateinit var intraSwitch: Switch
    private lateinit var dozeRow: ActionRow

    // Diagnostics card (chiusa di default)
    private lateinit var diagChevron: TextView
    private lateinit var diagBody: LinearLayout
    private lateinit var addrText: TextView
    private lateinit var remoteHintText: TextView
    private lateinit var fingerprintText: TextView
    private lateinit var certPreview: TextView
    private lateinit var copyAddrButton: TextView
    private lateinit var copyCertButton: TextView
    private lateinit var regenIdentityButton: TextView
    private lateinit var logsText: TextView

    // Dialog QR attivo (se aperto)
    private var pairingDialog: Dialog? = null
    private var dialogQrImage: ImageView? = null
    private var dialogMetaText: TextView? = null
    private var dialogP2pText: TextView? = null

    private var selectedMode = CaptureMode.ROOT
    private var selectedProfile = StreamProfile.P1080_120
    private var preferHevc = true
    private var intraRefresh = false
    private var customRemoteHost = ""
    private var diagnosticsExpanded = false
    private var lastQrUri = ""
    private var cachedQrBitmap: Bitmap? = null

    private val stateListener: (HostUiState) -> Unit = { s ->
        runOnUiThread { render(s) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        dp = resources.displayMetrics.density
        theme = HostTheme.from(this)
        loadPreferences()
        buildUi()
        HostStateStore.addListener(stateListener)
        loadIdentityAndNetworkAsync()
    }

    override fun onResume() {
        super.onResume()
        refreshDozeRow()
        refreshControlRow()
    }

    override fun onDestroy() {
        pairingDialog?.dismiss()
        pairingDialog = null
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
                        authToken = id.authToken,
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
                    putExtra(HostForegroundService.EXTRA_INTRA_REFRESH, intraRefresh)
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
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(theme.canvas)
            setPadding(px(20), px(22), px(20), px(32))
        }

        // 1. Header minimale: marchio Ghost + "Phantom" + pillola termica
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        val iconPlate = FrameLayout(this).apply {
            background = theme.iconPlateDrawable(dp)
            layoutParams = LinearLayout.LayoutParams(px(38), px(38)).apply {
                marginEnd = px(12)
            }
        }
        val ghostMark = ImageView(this).apply {
            setImageResource(R.drawable.ic_phantom_tile)
            setColorFilter(Color.WHITE)
            layoutParams = FrameLayout.LayoutParams(px(22), px(22), Gravity.CENTER)
        }
        iconPlate.addView(ghostMark)
        header.addView(iconPlate)

        val titleText = TextView(this).apply {
            text = "Phantom"
            textSize = 24f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(theme.textPrimary)
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        header.addView(titleText)

        thermalBadge = TextView(this).apply {
            text = "Normale"
            textSize = 12f
            fontFeatureSettings = "tnum"
            setTextColor(theme.textSecondary)
            setPadding(px(12), px(6), px(12), px(6))
            background = theme.capsuleDrawable(theme.card, theme.hairline, dp)
        }
        header.addView(thermalBadge)
        root.addView(header, marginBottom(px(20)))

        // 2. Hero Card: stato live, metriche compatte, CTA principale, QR in modale
        val heroCard = card(px(20))

        val statusRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        statusDot = View(this).apply {
            background = theme.dotDrawable(theme.textSecondary)
            layoutParams = LinearLayout.LayoutParams(px(10), px(10)).apply {
                marginEnd = px(10)
            }
        }
        statusHeadline = TextView(this).apply {
            text = "Pronto"
            textSize = 21f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(theme.textPrimary)
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        statusRow.addView(statusDot)
        statusRow.addView(statusHeadline)
        heroCard.addView(statusRow)

        statusSubline = TextView(this).apply {
            text = "Avvia l'host per controllare questo dispositivo da iPhone"
            textSize = 13f
            setTextColor(theme.textSecondary)
            setPadding(0, px(6), 0, px(16))
        }
        heroCard.addView(statusSubline)

        // Striscia metriche (visibile quando l'host è attivo)
        metricsBox = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            background = theme.elevatedDrawable(dp, 16f)
            setPadding(px(12), px(12), px(12), px(12))
            visibility = View.GONE
        }
        metricFpsValue = addMetricColumn(metricsBox, "FPS", "0")
        metricMbpsValue = addMetricColumn(metricsBox, "BANDA", "0.0")
        metricInputValue = addMetricColumn(metricsBox, "INPUT", "0")
        heroCard.addView(metricsBox, marginBottom(px(16)))

        toggleButton = Button(this).apply {
            text = "Avvia Host"
            isAllCaps = false
            textSize = 15f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            stateListAnimator = null
            elevation = 0f
            setTextColor(theme.ctaText)
            background = theme.capsuleDrawable(theme.ctaFill, null, dp)
            minHeight = px(52)
            minimumHeight = px(52)
            setOnClickListener { onTogglePressed() }
        }
        heroCard.addView(toggleButton, marginBottom(px(10)))

        pairButton = Button(this).apply {
            text = "Collega iPhone · QR"
            isAllCaps = false
            textSize = 14f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            stateListAnimator = null
            elevation = 0f
            setTextColor(theme.textPrimary)
            background = theme.capsuleDrawable(theme.elevated, theme.hairline, dp)
            minHeight = px(46)
            minimumHeight = px(46)
            setOnClickListener { showPairingDialog() }
        }
        heroCard.addView(pairButton)
        root.addView(heroCard, marginBottom(px(16)))

        // 3. Scheda Configurazione essenziale
        val cfgCard = card(px(20))
        cfgCard.addView(sectionLabel("MODALITÀ"))

        val modeTrack = segmentedTrack()
        modeRootBtn = segmentOption("Root") {
            if (!HostStateStore.snapshot().running) {
                selectedMode = CaptureMode.ROOT
                savePreferences()
                refreshConfigSegments(controlsEnabled = true)
            }
        }
        modeShizukuBtn = segmentOption("Shizuku") {
            if (!HostStateStore.snapshot().running) {
                selectedMode = CaptureMode.SHIZUKU
                savePreferences()
                refreshConfigSegments(controlsEnabled = true)
            }
        }
        modeStandardBtn = segmentOption("Standard") {
            if (!HostStateStore.snapshot().running) {
                selectedMode = CaptureMode.MEDIA_PROJECTION
                savePreferences()
                refreshConfigSegments(controlsEnabled = true)
            }
        }
        modeTrack.addView(modeRootBtn)
        modeTrack.addView(modeShizukuBtn)
        modeTrack.addView(modeStandardBtn)
        cfgCard.addView(modeTrack, marginBottom(px(8)))

        // Stato del controllo per la modalità scelta: Shizuku (permesso) o Standard (Accessibilità)
        controlRow = actionRow("Controllo dall'iPhone", "") { onControlRowAction() }
        cfgCard.addView(controlRow.row, marginBottom(px(16)))

        cfgCard.addView(sectionLabel("PROFILO VIDEO"))
        val profileTrack = segmentedTrack()
        prof1080_120Btn = segmentOption("1080p · 120") { selectProfile(StreamProfile.P1080_120) }
        prof1080_60Btn = segmentOption("1080p · 60") { selectProfile(StreamProfile.P1080_60) }
        prof720_60Btn = segmentOption("720p · 60") { selectProfile(StreamProfile.P720_60) }
        profileTrack.addView(prof1080_120Btn)
        profileTrack.addView(prof1080_60Btn)
        profileTrack.addView(prof720_60Btn)
        cfgCard.addView(profileTrack, marginBottom(px(16)))

        // Riga HEVC
        val hevcRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, px(4), 0, px(12))
        }
        val hevcTextCol = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginEnd = px(12)
            }
        }
        hevcTextCol.addView(
            TextView(this).apply {
                text = "Codec HEVC"
                textSize = 15f
                typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                setTextColor(theme.textPrimary)
            },
        )
        hevcTextCol.addView(
            TextView(this).apply {
                text = "Fallback automatico ad H.264"
                textSize = 12f
                setTextColor(theme.textSecondary)
            },
        )
        hevcSwitch = Switch(this).apply {
            isChecked = preferHevc
            setOnCheckedChangeListener { _, checked ->
                preferHevc = checked
                savePreferences()
            }
        }
        hevcRow.addView(hevcTextCol)
        hevcRow.addView(hevcSwitch)
        cfgCard.addView(hevcRow)

        // Intra-refresh (sperimentale)
        val intraRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, px(12), 0, 0)
        }
        val intraTextCol = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginEnd = px(12)
            }
        }
        intraTextCol.addView(
            TextView(this).apply {
                text = "Intra-refresh (sperimentale)"
                textSize = 15f
                typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                setTextColor(theme.textPrimary)
            },
        )
        intraTextCol.addView(
            TextView(this).apply {
                text = "Meno scatti dai keyframe; se l'encoder non lo supporta si disattiva da solo"
                textSize = 12f
                setTextColor(theme.textSecondary)
            },
        )
        intraSwitch = Switch(this).apply {
            isChecked = intraRefresh
            setOnCheckedChangeListener { _, checked ->
                intraRefresh = checked
                savePreferences()
            }
        }
        intraRow.addView(intraTextCol)
        intraRow.addView(intraSwitch)
        cfgCard.addView(intraRow)

        cfgCard.addView(divider())

        // Riga Doze / risparmio batteria
        dozeRow = actionRow("Raggiungibile a schermo spento", "Escludi da Doze per 5G continuo") {
            requestBatteryOptimizationExemption()
        }
        cfgCard.addView(dozeRow.row)

        root.addView(cfgCard, marginBottom(px(16)))

        // 4. Scheda Diagnostica collassabile (chiusa di default)
        val diagCard = card(px(20))
        val diagHeader = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setOnClickListener { toggleDiagnostics() }
        }
        diagHeader.addView(
            TextView(this).apply {
                text = "Diagnostica"
                textSize = 15f
                typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                setTextColor(theme.textPrimary)
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            },
        )
        diagChevron = TextView(this).apply {
            text = "▾"
            textSize = 16f
            setTextColor(theme.textSecondary)
        }
        diagHeader.addView(diagChevron)
        diagCard.addView(diagHeader)

        diagBody = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = View.GONE
            setPadding(0, px(14), 0, 0)
        }

        addrText = TextView(this).apply {
            text = "–"
            textSize = 13f
            typeface = Typeface.MONOSPACE
            setTextColor(theme.textPrimary)
        }
        diagBody.addView(addrText)

        remoteHintText = TextView(this).apply {
            textSize = 12f
            setTextColor(theme.textSecondary)
            setPadding(0, px(4), 0, px(8))
        }
        diagBody.addView(remoteHintText)

        fingerprintText = TextView(this).apply {
            textSize = 12f
            typeface = Typeface.MONOSPACE
            setTextColor(theme.textSecondary)
            setPadding(0, 0, 0, px(4))
        }
        diagBody.addView(fingerprintText)

        certPreview = TextView(this).apply {
            text = "Caricamento identità…"
            textSize = 11f
            typeface = Typeface.MONOSPACE
            setTextColor(theme.textSecondary)
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.MIDDLE
            setPadding(0, 0, 0, px(12))
        }
        diagBody.addView(certPreview)

        val actionRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        copyAddrButton = smallPillAction("Copia IP") {
            val s = HostStateStore.snapshot()
            val value = if (s.ipAddress.isNotBlank()) "${s.ipAddress}:${s.port}" else ":${s.port}"
            copyToClipboard("Phantom IP", value)
        }
        copyCertButton = smallPillAction("Copia Cert") {
            val cert = HostStateStore.snapshot().certificateHex
            if (cert.isNotBlank()) copyToClipboard("Phantom Cert", cert)
        }
        regenIdentityButton = smallPillAction("Nuova chiave") { regenerateIdentity() }
        actionRow.addView(copyAddrButton, weightedActionParams(marginEndPx = px(6)))
        actionRow.addView(copyCertButton, weightedActionParams(marginStartPx = px(3), marginEndPx = px(3)))
        actionRow.addView(regenIdentityButton, weightedActionParams(marginStartPx = px(6)))
        diagBody.addView(actionRow, marginBottom(px(14)))

        diagBody.addView(sectionLabel("LOG EVENTI"))
        logsText = TextView(this).apply {
            textSize = 11f
            typeface = Typeface.MONOSPACE
            setTextColor(theme.textSecondary)
            background = theme.elevatedDrawable(dp, 14f)
            setPadding(px(12), px(10), px(12), px(10))
        }
        diagBody.addView(logsText)
        diagBody.addView(
            smallPillAction("Esporta log") { exportLogs() },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                topMargin = px(10)
            },
        )

        diagCard.addView(diagBody)
        root.addView(diagCard)

        refreshConfigSegments(controlsEnabled = true)
        refreshDozeRow()

        val scroll = ScrollView(this).apply {
            setBackgroundColor(theme.canvas)
            isVerticalScrollBarEnabled = false
            addView(root)
        }
        setContentView(scroll)
    }

    /** Condivide il log completo con dati di dispositivo e configurazione, da allegare a una segnalazione. */
    private fun exportLogs() {
        val s = HostStateStore.snapshot()
        val version = try {
            packageManager.getPackageInfo(packageName, 0).versionName
        } catch (_: Throwable) {
            "?"
        }
        val header = buildString {
            appendLine("Phantom $version · ${Build.MANUFACTURER} ${Build.MODEL} · Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})")
            appendLine("SoC: ${if (Build.VERSION.SDK_INT >= 31) Build.SOC_MODEL else Build.HARDWARE} · ABI: ${Build.SUPPORTED_ABIS.joinToString()}")
            appendLine("Modalità: $selectedMode · profilo: ${selectedProfile.label} · HEVC: $preferHevc · intra-refresh: $intraRefresh")
            appendLine("Shizuku: ${ShizukuLauncher.status().label} · Accessibilità: ${PhantomAccessibilityService.isEnabled(this@MainActivity)}")
            appendLine("Stato: ${s.statusText} · ${s.thermalLabel}")
            appendLine("----")
        }
        val text = header + s.logs.joinToString("\n")
        val send = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_SUBJECT, "Log Phantom $version")
            putExtra(Intent.EXTRA_TEXT, text)
        }
        try {
            startActivity(Intent.createChooser(send, "Esporta log"))
        } catch (_: Throwable) {
            copyToClipboard("Log", text)
        }
    }

    private fun selectProfile(profile: StreamProfile) {
        if (HostStateStore.snapshot().running) return
        selectedProfile = profile
        savePreferences()
        refreshConfigSegments(controlsEnabled = true)
    }

    private fun toggleDiagnostics() {
        diagnosticsExpanded = !diagnosticsExpanded
        diagBody.visibility = if (diagnosticsExpanded) View.VISIBLE else View.GONE
        diagChevron.text = if (diagnosticsExpanded) "▴" else "▾"
    }

    private fun showPairingDialog() {
        pairingDialog?.dismiss()

        val dialog = Dialog(this)
        dialog.requestWindowFeature(Window.FEATURE_NO_TITLE)
        dialog.window?.setBackgroundDrawable(ColorDrawable(Color.TRANSPARENT))

        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = theme.cardDrawable(dp, 28f)
            setPadding(px(22), px(22), px(22), px(20))
        }

        container.addView(
            TextView(this).apply {
                text = "Collega iPhone"
                textSize = 20f
                typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                setTextColor(theme.textPrimary)
                gravity = Gravity.CENTER_HORIZONTAL
            },
        )
        container.addView(
            TextView(this).apply {
                text = "Apri Phantom su iPhone, tocca + e inquadra il QR"
                textSize = 13f
                setTextColor(theme.textSecondary)
                gravity = Gravity.CENTER_HORIZONTAL
                setPadding(0, px(6), 0, px(16))
            },
        )

        val qrFrame = FrameLayout(this).apply {
            background = GradientDrawable().apply {
                setColor(Color.WHITE)
                cornerRadius = 20f * dp
            }
            setPadding(px(14), px(14), px(14), px(14))
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                bottomMargin = px(14)
            }
        }
        val qrView = ImageView(this).apply {
            scaleType = ImageView.ScaleType.FIT_CENTER
            layoutParams = FrameLayout.LayoutParams(px(216), px(216), Gravity.CENTER)
        }
        qrFrame.addView(qrView)
        container.addView(qrFrame)

        val metaText = TextView(this).apply {
            textSize = 12f
            typeface = Typeface.MONOSPACE
            setTextColor(theme.textPrimary)
            gravity = Gravity.CENTER_HORIZONTAL
        }
        container.addView(metaText)

        val p2pText = TextView(this).apply {
            textSize = 12f
            setTextColor(theme.textSecondary)
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(0, px(4), 0, px(14))
        }
        container.addView(p2pText)

        container.addView(sectionLabel("INDIRIZZO REMOTO OPZIONALE (TAILSCALE / DDNS)"))
        val remoteField = EditText(this).apply {
            hint = "es. 100.x.y.z:47800"
            setText(customRemoteHost)
            setTextColor(theme.textPrimary)
            setHintTextColor(theme.textSecondary)
            textSize = 13f
            typeface = Typeface.MONOSPACE
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            background = theme.elevatedDrawable(dp, 14f)
            setPadding(px(12), px(10), px(12), px(10))
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
                override fun afterTextChanged(s: Editable?) {
                    customRemoteHost = s?.toString()?.trim().orEmpty()
                    savePreferences()
                    updateDialogQr(HostStateStore.snapshot())
                }
            })
        }
        container.addView(remoteField, marginBottom(px(16)))

        val closeBtn = Button(this).apply {
            text = "Chiudi"
            isAllCaps = false
            textSize = 14f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            stateListAnimator = null
            elevation = 0f
            setTextColor(theme.ctaText)
            background = theme.capsuleDrawable(theme.ctaFill, null, dp)
            minHeight = px(46)
            minimumHeight = px(46)
            setOnClickListener { dialog.dismiss() }
        }
        container.addView(closeBtn)

        val wrapper = FrameLayout(this).apply {
            setPadding(px(16), px(16), px(16), px(16))
            addView(
                container,
                FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                ),
            )
        }
        dialog.setContentView(wrapper)
        dialog.window?.setLayout(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        )
        dialog.setOnDismissListener {
            dialogQrImage = null
            dialogMetaText = null
            dialogP2pText = null
            pairingDialog = null
        }

        pairingDialog = dialog
        dialogQrImage = qrView
        dialogMetaText = metaText
        dialogP2pText = p2pText
        updateDialogQr(HostStateStore.snapshot())
        dialog.show()
    }

    private fun updateDialogQr(s: HostUiState) {
        val qrView = dialogQrImage ?: return
        dialogMetaText?.text = if (s.fingerprintHex.isNotBlank()) {
            "ID ${s.fingerprintHex}"
        } else {
            "Generazione identità…"
        }
        dialogP2pText?.text = when {
            s.p2pReady && s.ipAddress.isNotBlank() -> "LAN ${s.ipAddress}:${s.port} · P2P 5G attivo"
            s.p2pEndpointId.isNotBlank() -> "LAN + P2P 5G inclusi nel QR"
            else -> "Preparazione canale P2P…"
        }

        val bitmap = ensureQrBitmap(s)
        if (bitmap != null) {
            qrView.visibility = View.VISIBLE
            qrView.setImageBitmap(bitmap)
        } else {
            qrView.visibility = View.INVISIBLE
        }
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
                        authToken = id.authToken,
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
                    putExtra(HostForegroundService.EXTRA_INTRA_REFRESH, intraRefresh)
                }
                startForegroundService(intent)
            }
            CaptureMode.SHIZUKU -> when (ShizukuLauncher.status()) {
                ShizukuLauncher.Status.READY -> startShizukuHost()
                ShizukuLauncher.Status.NO_PERMISSION -> ShizukuLauncher.requestPermission { granted ->
                    runOnUiThread {
                        refreshControlRow()
                        if (granted) {
                            startShizukuHost()
                        } else {
                            Toast.makeText(this, "Permesso Shizuku negato", Toast.LENGTH_SHORT).show()
                        }
                    }
                }
                ShizukuLauncher.Status.TOO_OLD, ShizukuLauncher.Status.NOT_RUNNING -> {
                    Toast.makeText(this, ShizukuLauncher.status().label, Toast.LENGTH_LONG).show()
                    openShizukuApp()
                }
            }
            CaptureMode.MEDIA_PROJECTION -> {
                val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                @Suppress("DEPRECATION")
                startActivityForResult(mpm.createScreenCaptureIntent(), REQ_MEDIA_PROJECTION)
            }
        }
    }

    private fun startShizukuHost() {
        val intent = Intent(this, HostForegroundService::class.java).apply {
            action = HostForegroundService.ACTION_START_SHIZUKU
            putExtra(HostForegroundService.EXTRA_PORT, 47800)
            putExtra(HostForegroundService.EXTRA_PROFILE, selectedProfile.name)
            putExtra(HostForegroundService.EXTRA_HEVC, preferHevc)
            putExtra(HostForegroundService.EXTRA_INTRA_REFRESH, intraRefresh)
        }
        startForegroundService(intent)
    }

    private fun refreshDozeRow() {
        if (!::dozeRow.isInitialized) return
        val pm = getSystemService(Context.POWER_SERVICE) as? PowerManager
        val exempt = pm?.isIgnoringBatteryOptimizations(packageName) == true
        if (exempt) {
            dozeRow.set("Escluso dal risparmio batteria", theme.liveGreen, "Attivo", enabled = false)
        } else {
            dozeRow.set("Escludi da Doze per 5G continuo", theme.textSecondary, "Escludi", enabled = true)
        }
    }

    /** Riga di stato del controllo: permesso Shizuku in modalità Shizuku, Accessibilità in modalità Standard. */
    private fun refreshControlRow() {
        if (!::controlRow.isInitialized) return
        when (selectedMode) {
            CaptureMode.ROOT -> controlRow.row.visibility = View.GONE
            CaptureMode.SHIZUKU -> {
                controlRow.row.visibility = View.VISIBLE
                controlRow.title.text = "Shizuku"
                when (ShizukuLauncher.status()) {
                    ShizukuLauncher.Status.READY ->
                        controlRow.set("Pronto: tocco, tastiera, audio e schermo spento", theme.liveGreen, "Pronto", enabled = false)
                    ShizukuLauncher.Status.NO_PERMISSION ->
                        controlRow.set("Concedi a Phantom il permesso in Shizuku", theme.warnAmber, "Concedi", enabled = true)
                    ShizukuLauncher.Status.TOO_OLD ->
                        controlRow.set("Aggiorna Shizuku (serve la versione 11+)", theme.warnAmber, "Apri", enabled = true)
                    ShizukuLauncher.Status.NOT_RUNNING ->
                        controlRow.set("Avvia Shizuku con il Debug wireless", theme.warnAmber, "Apri", enabled = true)
                }
            }
            CaptureMode.MEDIA_PROJECTION -> {
                controlRow.row.visibility = View.VISIBLE
                controlRow.title.text = "Controllo con Accessibilità"
                if (PhantomAccessibilityService.isEnabled(this)) {
                    controlRow.set("Attivo: tap, swipe, tasti di sistema, testo", theme.liveGreen, "Attivo", enabled = false)
                } else {
                    controlRow.set("Senza: solo visione dall'iPhone", theme.textSecondary, "Attiva", enabled = true)
                }
            }
        }
    }

    private fun onControlRowAction() {
        when (selectedMode) {
            CaptureMode.ROOT -> {}
            CaptureMode.SHIZUKU -> when (ShizukuLauncher.status()) {
                ShizukuLauncher.Status.NO_PERMISSION -> ShizukuLauncher.requestPermission { granted ->
                    runOnUiThread {
                        refreshControlRow()
                        if (!granted) Toast.makeText(this, "Permesso Shizuku negato", Toast.LENGTH_SHORT).show()
                    }
                }
                ShizukuLauncher.Status.READY -> {}
                else -> openShizukuApp()
            }
            CaptureMode.MEDIA_PROJECTION -> {
                Toast.makeText(this, "Attiva «Phantom · controllo remoto» in Accessibilità", Toast.LENGTH_LONG).show()
                try {
                    startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
                } catch (_: Throwable) {
                }
            }
        }
    }

    private fun openShizukuApp() {
        val launch = packageManager.getLaunchIntentForPackage(SHIZUKU_PACKAGE)
        if (launch != null) {
            startActivity(launch)
        } else {
            Toast.makeText(this, "Installa Shizuku (Play Store o GitHub) e avvialo con il Debug wireless", Toast.LENGTH_LONG).show()
            try {
                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://shizuku.rikka.app/download/")))
            } catch (_: Throwable) {
            }
        }
    }

    /** Riga con titolo, stato colorato e pulsante a capsula (Doze, Shizuku, Accessibilità). */
    private inner class ActionRow(
        val row: LinearLayout,
        val title: TextView,
        val status: TextView,
        val button: TextView,
    ) {
        fun set(statusText: String, statusColor: Int, buttonText: String, enabled: Boolean) {
            status.text = statusText
            status.setTextColor(statusColor)
            button.text = buttonText
            button.setTextColor(if (enabled) theme.textPrimary else theme.liveGreen)
            button.isEnabled = enabled
        }
    }

    private fun actionRow(titleText: String, statusText: String, onClick: () -> Unit): ActionRow {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, px(12), 0, 0)
        }
        val textCol = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                marginEnd = px(12)
            }
        }
        val title = TextView(this).apply {
            text = titleText
            textSize = 14f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(theme.textPrimary)
        }
        val status = TextView(this).apply {
            text = statusText
            textSize = 12f
            setTextColor(theme.textSecondary)
        }
        textCol.addView(title)
        textCol.addView(status)
        val button = TextView(this).apply {
            text = ""
            textSize = 13f
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(theme.textPrimary)
            setPadding(px(14), px(8), px(14), px(8))
            background = theme.capsuleDrawable(theme.elevated, theme.hairline, dp)
            setOnClickListener { onClick() }
        }
        row.addView(textCol)
        row.addView(button)
        return ActionRow(row, title, status, button)
    }

    @android.annotation.SuppressLint("BatteryLife")
    private fun requestBatteryOptimizationExemption() {
        try {
            val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                data = Uri.parse("package:$packageName")
            }
            startActivity(intent)
        } catch (_: Throwable) {
            try {
                startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
            } catch (_: Throwable) {
            }
        }
    }

    private fun requestNotificationPermissionIfNeeded() {
        val needed = ArrayList<String>(2)
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            needed.add(Manifest.permission.POST_NOTIFICATIONS)
        }
        if (selectedMode == CaptureMode.MEDIA_PROJECTION &&
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q &&
            checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED
        ) {
            needed.add(Manifest.permission.RECORD_AUDIO)
        }
        if (needed.isNotEmpty()) {
            requestPermissions(needed.toTypedArray(), REQ_NOTIF)
        }
    }

    private fun render(s: HostUiState) {
        val tempStr = s.batteryTempC?.let { String.format(Locale.US, "%.1f °C · ", it) } ?: ""
        thermalBadge.text = "$tempStr${s.thermalLabel}"
        val thermalColor = when {
            s.thermalLabel.contains("Critica", ignoreCase = true) -> theme.dangerRed
            !s.thermalLabel.equals("Normale", ignoreCase = true) -> theme.warnAmber
            else -> theme.textSecondary
        }
        thermalBadge.setTextColor(thermalColor)

        val dotColor = when {
            s.connected -> theme.liveGreen
            s.starting -> theme.warnAmber
            s.running -> theme.textPrimary
            else -> theme.textSecondary
        }
        statusDot.background = theme.dotDrawable(dotColor)

        statusHeadline.text = when {
            s.connected -> "Connesso"
            s.starting -> "Avvio in corso…"
            s.running -> "In ascolto"
            else -> "Pronto"
        }

        statusSubline.text = when {
            s.connected -> {
                val prof = s.activeProfileText.ifBlank { selectedProfile.label }
                "Sessione attiva · $prof"
            }
            s.running -> {
                val lan = if (s.ipAddress.isNotBlank()) "LAN ${s.ipAddress}:${s.port}" else "Porta ${s.port}"
                val p2p = if (s.p2pReady) " · P2P 5G attivo" else " · P2P in preparazione"
                "$lan$p2p"
            }
            else -> {
                val modeName = when (selectedMode) {
                    CaptureMode.ROOT -> "Root"
                    CaptureMode.SHIZUKU -> "Shizuku"
                    CaptureMode.MEDIA_PROJECTION -> "Standard"
                }
                "Modalità $modeName · ${selectedProfile.label}"
            }
        }

        metricsBox.visibility = if (s.running) View.VISIBLE else View.GONE
        metricFpsValue.text = String.format(Locale.US, "%.0f", s.fps)
        metricMbpsValue.text = String.format(Locale.US, "%.1f Mbps", s.mbps)
        metricInputValue.text = (s.touches + s.keys).toString()

        if (s.running) {
            toggleButton.text = "Ferma Host"
            toggleButton.setTextColor(Color.WHITE)
            toggleButton.background = theme.capsuleDrawable(theme.dangerRed, null, dp)
        } else {
            toggleButton.text = "Avvia Host"
            toggleButton.setTextColor(theme.ctaText)
            toggleButton.background = theme.capsuleDrawable(theme.ctaFill, null, dp)
        }

        val controlsEnabled = !s.running
        refreshConfigSegments(controlsEnabled)
        hevcSwitch.isEnabled = controlsEnabled
        intraSwitch.isEnabled = controlsEnabled

        // Diagnostica
        addrText.text = if (s.ipAddress.isNotBlank()) {
            "LAN: ${s.ipAddress}:${s.port}"
        } else {
            "In attesa Wi-Fi (porta ${s.port})"
        }
        remoteHintText.text = when {
            s.p2pReady && s.p2pEndpointId.isNotBlank() ->
                "P2P (iroh) attivo · ID ${s.p2pEndpointId.take(12)}…"
            s.p2pEndpointId.isNotBlank() ->
                "P2P (iroh) pronto nel QR · nessuna VPN necessaria"
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
        copyCertButton.alpha = if (s.certificateHex.isNotBlank()) 1f else 0.4f
        regenIdentityButton.isEnabled = controlsEnabled
        regenIdentityButton.alpha = if (controlsEnabled) 1f else 0.4f
        logsText.text = if (s.logs.isEmpty()) "(nessun evento)" else s.logs.takeLast(30).joinToString("\n")

        // Pre-genera QR in cache e aggiorna eventuale modale aperta
        ensureQrBitmap(s)
        updateDialogQr(s)
    }

    private fun refreshConfigSegments(controlsEnabled: Boolean) {
        styleSegment(modeRootBtn, selectedMode == CaptureMode.ROOT, controlsEnabled)
        styleSegment(modeShizukuBtn, selectedMode == CaptureMode.SHIZUKU, controlsEnabled)
        styleSegment(modeStandardBtn, selectedMode == CaptureMode.MEDIA_PROJECTION, controlsEnabled)
        refreshControlRow()
        styleSegment(prof1080_120Btn, selectedProfile == StreamProfile.P1080_120, controlsEnabled)
        styleSegment(prof1080_60Btn, selectedProfile == StreamProfile.P1080_60, controlsEnabled)
        styleSegment(prof720_60Btn, selectedProfile == StreamProfile.P720_60, controlsEnabled)
    }

    private fun styleSegment(view: TextView, selected: Boolean, enabled: Boolean) {
        view.background = theme.segmentDrawable(selected, dp)
        view.setTextColor(if (selected) theme.ctaText else theme.textSecondary)
        view.alpha = if (enabled) 1f else 0.5f
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

    private fun ensureQrBitmap(s: HostUiState): Bitmap? {
        if (s.certificateDer.isEmpty()) return null
        val lan = s.ipAddress.takeIf { it.isNotBlank() }?.let { "$it:${s.port}" }
        val wan = effectiveRemoteAddress(s)
        val deviceName = Build.MODEL.ifBlank { "Android" }
        val uri = PairingCodec.buildPairingUri(
            name = deviceName,
            lan = lan,
            wan = wan,
            certDer = s.certificateDer,
            p2pId = s.p2pEndpointId.takeIf { it.isNotBlank() },
            authToken = s.authToken.takeIf { it.size == 16 },
        )
        if (uri == lastQrUri && cachedQrBitmap != null) return cachedQrBitmap
        lastQrUri = uri
        cachedQrBitmap = try {
            renderQrBitmap(uri, 512)
        } catch (_: Throwable) {
            null
        }
        return cachedQrBitmap
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
        val modeName = p.getString(KEY_MODE, CaptureMode.ROOT.name)
        selectedMode = CaptureMode.entries.firstOrNull { it.name == modeName } ?: CaptureMode.ROOT
        val profName = p.getString(KEY_PROFILE, StreamProfile.P1080_120.name)
        selectedProfile = StreamProfile.entries.firstOrNull { it.name == profName } ?: StreamProfile.P1080_120
        preferHevc = p.getBoolean(KEY_HEVC, true)
        intraRefresh = p.getBoolean(KEY_INTRA_REFRESH, false)
        customRemoteHost = p.getString(KEY_REMOTE_HOST, "").orEmpty().trim()
    }

    private fun savePreferences() {
        getSharedPreferences(PREFS_NAME, MODE_PRIVATE).edit()
            .putString(KEY_MODE, selectedMode.name)
            .putString(KEY_PROFILE, selectedProfile.name)
            .putBoolean(KEY_HEVC, preferHevc)
            .putBoolean(KEY_INTRA_REFRESH, intraRefresh)
            .putString(KEY_REMOTE_HOST, customRemoteHost)
            .apply()
    }

    // --- Helper UI leggeri ---

    private fun px(v: Int): Int = (v * dp).toInt()

    private fun card(padPx: Int): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(padPx, padPx, padPx, padPx)
        background = theme.cardDrawable(dp, 24f)
    }

    private fun sectionLabel(title: String): TextView = TextView(this).apply {
        text = title
        textSize = 11f
        letterSpacing = 0.08f
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        setTextColor(theme.textSecondary)
        setPadding(0, 0, 0, px(8))
    }

    private fun segmentedTrack(): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        background = theme.capsuleDrawable(theme.elevated, theme.hairline, dp)
        setPadding(px(4), px(4), px(4), px(4))
    }

    private fun segmentOption(label: String, onClick: () -> Unit): TextView = TextView(this).apply {
        text = label
        textSize = 13f
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        gravity = Gravity.CENTER
        setPadding(px(8), px(9), px(8), px(9))
        layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        setOnClickListener { onClick() }
    }

    private fun smallPillAction(label: String, onClick: () -> Unit): TextView = TextView(this).apply {
        text = label
        textSize = 12f
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        gravity = Gravity.CENTER
        setTextColor(theme.textPrimary)
        background = theme.capsuleDrawable(theme.elevated, theme.hairline, dp)
        setPadding(px(8), px(9), px(8), px(9))
        setOnClickListener { onClick() }
    }

    private fun weightedActionParams(marginStartPx: Int = 0, marginEndPx: Int = 0): LinearLayout.LayoutParams =
        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
            marginStart = marginStartPx
            marginEnd = marginEndPx
        }

    private fun addMetricColumn(parent: LinearLayout, label: String, initialValue: String): TextView {
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        val valueView = TextView(this).apply {
            text = initialValue
            textSize = 16f
            fontFeatureSettings = "tnum"
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            setTextColor(theme.textPrimary)
        }
        val labelView = TextView(this).apply {
            text = label
            textSize = 10f
            letterSpacing = 0.08f
            setTextColor(theme.textSecondary)
            setPadding(0, px(2), 0, 0)
        }
        col.addView(valueView)
        col.addView(labelView)
        parent.addView(col)
        return valueView
    }

    private fun divider(): View = View(this).apply {
        setBackgroundColor(theme.hairline)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, px(1).coerceAtLeast(1))
    }

    private fun marginBottom(px: Int): LinearLayout.LayoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
    ).apply {
        bottomMargin = px
    }

    companion object {
        internal const val PREFS_NAME = "phantom_host_prefs"
        internal const val KEY_MODE = "mode"
        private const val SHIZUKU_PACKAGE = "moe.shizuku.privileged.api"
        internal const val KEY_PROFILE = "profile"
        internal const val KEY_HEVC = "hevc"
        internal const val KEY_INTRA_REFRESH = "intra_refresh"
        internal const val KEY_REMOTE_HOST = "remote_host"

        private const val REQ_MEDIA_PROJECTION = 1001
        private const val REQ_NOTIF = 1002
    }
}
