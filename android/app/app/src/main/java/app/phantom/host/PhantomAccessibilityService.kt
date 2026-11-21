package app.phantom.host

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.ComponentName
import android.content.Context
import android.graphics.Path
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.provider.Settings
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import app.phantom.host.core.GestureStroke
import app.phantom.host.core.GestureTracker
import app.phantom.host.core.HidToAndroidKeyMap
import app.phantom.host.core.KeyModifierState
import app.phantom.host.core.NormalizedInputFinger
import uniffi.phantom_ffi.KeyInput
import uniffi.phantom_ffi.RemoteControlCommand
import uniffi.phantom_ffi.TouchState

/**
 * Controllo di ripiego per la modalità Standard (senza root né Shizuku): l'utente attiva una volta il servizio nelle
 * impostazioni di Accessibilità e l'iPhone può toccare, scorrere, usare Indietro/Home/Recenti/Notifiche e scrivere nei
 * campi di testo.
 *
 * Limiti rispetto a Root/Shizuku: i gesti partono quando l'ultimo dito si alza (l'Accessibilità accetta solo gesti
 * interi), niente scorciatoie da tastiera complesse; il testo si inserisce nel campo che ha il fuoco.
 */
class PhantomAccessibilityService : AccessibilityService() {
    private val lock = Any()
    private val tracker = GestureTracker()
    private val modifiers = KeyModifierState()
    private val keyMap: KeyCharacterMap by lazy { KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD) }

    override fun onServiceConnected() {
        super.onServiceConnected()
        instance = this
        HostStateStore.addLog("Accessibilità collegata: controllo da iPhone disponibile in modalità Standard")
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        if (instance === this) instance = null
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    fun reset() {
        synchronized(lock) {
            tracker.reset()
            modifiers.reset()
        }
    }

    fun handleTouch(state: TouchState) {
        val (w, h) = screenSize()
        val fingers = state.fingers.map {
            NormalizedInputFinger(id = it.id.toInt(), x = it.x.toInt(), y = it.y.toInt(), pressure = it.pressure.toInt())
        }
        val gesture = synchronized(lock) { tracker.update(fingers, w, h, SystemClock.uptimeMillis()) } ?: return
        dispatch(gesture)
    }

    private fun dispatch(strokes: List<GestureStroke>) {
        if (strokes.isEmpty()) return
        val origin = strokes.minOf { it.startMs }
        val builder = GestureDescription.Builder()
        val maxDuration = GestureDescription.getMaxGestureDuration()
        for (s in strokes.take(GestureDescription.getMaxStrokeCount())) {
            val path = Path()
            val first = s.points.first()
            path.moveTo(first.x, first.y)
            for (p in s.points.drop(1)) {
                path.lineTo(p.x, p.y)
            }
            val start = (s.startMs - origin).coerceIn(0L, maxDuration - 1)
            // un tap troppo breve può essere ignorato da alcune app: almeno 40 ms
            val duration = s.durationMs.coerceIn(40L, maxDuration - start)
            builder.addStroke(GestureDescription.StrokeDescription(path, start, duration))
        }
        try {
            dispatchGesture(builder.build(), null, null)
        } catch (t: Throwable) {
            HostStateStore.addLog("Gesto non inviato: ${t.javaClass.simpleName}")
        }
    }

    fun handleKey(key: KeyInput) {
        val hid = key.code.toInt()
        val meta = synchronized(lock) { modifiers.onKey(hid, key.down) }
        if (!key.down) return
        val code = HidToAndroidKeyMap.androidKeyCode(hid) ?: return
        when (code) {
            KeyEvent.KEYCODE_BACK -> performGlobalAction(GLOBAL_ACTION_BACK)
            KeyEvent.KEYCODE_HOME -> performGlobalAction(GLOBAL_ACTION_HOME)
            KeyEvent.KEYCODE_POWER -> handleControl(RemoteControlCommand.POWER)
            KeyEvent.KEYCODE_VOLUME_UP -> handleControl(RemoteControlCommand.VOLUME_UP)
            KeyEvent.KEYCODE_VOLUME_DOWN -> handleControl(RemoteControlCommand.VOLUME_DOWN)
            KeyEvent.KEYCODE_DEL -> editFocusedText { it.dropLast(1) }
            KeyEvent.KEYCODE_ENTER -> pressEnter()
            else -> {
                val ch = keyMap.get(code, meta)
                if (ch != 0 && (ch and KeyCharacterMap.COMBINING_ACCENT) == 0) {
                    val text = String(Character.toChars(ch))
                    editFocusedText { it + text }
                }
            }
        }
    }

    /** `true` se il comando è stato gestito qui. */
    fun handleControl(command: RemoteControlCommand): Boolean {
        when (command) {
            RemoteControlCommand.VOLUME_UP, RemoteControlCommand.VOLUME_DOWN -> {
                adjustVolume(this, command == RemoteControlCommand.VOLUME_UP)
                return true
            }
            else -> {}
        }
        val action = when (command) {
            RemoteControlCommand.BACK -> GLOBAL_ACTION_BACK
            RemoteControlCommand.HOME -> GLOBAL_ACTION_HOME
            RemoteControlCommand.RECENTS -> GLOBAL_ACTION_RECENTS
            RemoteControlCommand.NOTIFICATIONS -> GLOBAL_ACTION_NOTIFICATIONS
            RemoteControlCommand.QUICK_SETTINGS -> GLOBAL_ACTION_QUICK_SETTINGS
            RemoteControlCommand.POWER ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) GLOBAL_ACTION_LOCK_SCREEN else GLOBAL_ACTION_POWER_DIALOG
            RemoteControlCommand.SCREENSHOT ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) GLOBAL_ACTION_TAKE_SCREENSHOT else return false
            else -> return false
        }
        return performGlobalAction(action)
    }

    private fun editFocusedText(edit: (String) -> String) {
        val node = rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return
        if (!node.isEditable) return
        // se il campo mostra il suggerimento (testo vuoto) non lo si considera contenuto
        val current = if (node.isShowingHintText) "" else node.text?.toString().orEmpty()
        val args = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, edit(current))
        }
        node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
    }

    private fun pressEnter() {
        val node = rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
        } else {
            editFocusedText { "$it\n" }
        }
    }

    private fun screenSize(): Pair<Int, Int> {
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val b = wm.maximumWindowMetrics.bounds
            b.width() to b.height()
        } else {
            val dm = android.util.DisplayMetrics()
            @Suppress("DEPRECATION")
            wm.defaultDisplay.getRealMetrics(dm)
            dm.widthPixels to dm.heightPixels
        }
    }

    companion object {
        @Volatile
        var instance: PhantomAccessibilityService? = null
            private set

        /** Volume multimediale su/giù con il pannello di sistema (funziona anche senza Accessibilità). */
        fun adjustVolume(context: Context, up: Boolean) {
            val am = context.getSystemService(Context.AUDIO_SERVICE) as? android.media.AudioManager ?: return
            am.adjustStreamVolume(
                android.media.AudioManager.STREAM_MUSIC,
                if (up) android.media.AudioManager.ADJUST_RAISE else android.media.AudioManager.ADJUST_LOWER,
                android.media.AudioManager.FLAG_SHOW_UI,
            )
        }

        /** `true` se l'utente ha attivato il servizio nelle impostazioni di Accessibilità. */
        fun isEnabled(context: Context): Boolean {
            val expected = ComponentName(context, PhantomAccessibilityService::class.java)
            val enabled = Settings.Secure.getString(
                context.contentResolver,
                Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
            ) ?: return false
            return enabled.split(':').any { ComponentName.unflattenFromString(it) == expected }
        }
    }
}
