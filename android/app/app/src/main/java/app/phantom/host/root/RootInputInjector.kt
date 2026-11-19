package app.phantom.host.root

import android.annotation.SuppressLint
import android.os.SystemClock
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import app.phantom.host.core.HidToAndroidKeyMap
import app.phantom.host.core.KeyModifierState
import app.phantom.host.core.NormalizedInputFinger
import app.phantom.host.core.SynthAction
import app.phantom.host.core.TouchEventSynthesizer
import uniffi.phantom_ffi.KeyInput
import uniffi.phantom_ffi.TouchState
import java.lang.reflect.Method

/**
 * Inietta `MotionEvent` (multi-touch fino a 10 dita) e `KeyEvent` (HID) dentro Android tramite
 * `InputManagerGlobal` (Android 14+) o `InputManager` (Android 10–13) eseguiti con privilegi root.
 */
@SuppressLint("PrivateApi", "DiscouragedPrivateApi")
class RootInputInjector(
    private val displayWidth: Int,
    private val displayHeight: Int,
) {
    private val synthesizer = TouchEventSynthesizer()
    private val modifierState = KeyModifierState()
    private val props = Array(10) { MotionEvent.PointerProperties() }
    private val coords = Array(10) { MotionEvent.PointerCoords() }
    private var touchDownTimeMs = 0L

    private val inputManagerInstance: Any?
    private val injectMethod: Method?

    init {
        val (mgr, method) = resolveInjector()
        inputManagerInstance = mgr
        injectMethod = method
    }

    val isAvailable: Boolean get() = inputManagerInstance != null && injectMethod != null

    fun injectTouch(state: TouchState) {
        val fingers = state.fingers.map {
            NormalizedInputFinger(
                id = it.id.toInt(),
                x = it.x.toInt(),
                y = it.y.toInt(),
                pressure = it.pressure.toInt(),
            )
        }
        val steps = synthesizer.update(fingers, displayWidth, displayHeight)
        for (step in steps) {
            val nowMs = SystemClock.uptimeMillis()
            if (step.action == SynthAction.DOWN) {
                touchDownTimeMs = nowMs
            }
            val pointerCount = step.pointers.size.coerceAtMost(10)
            if (pointerCount == 0) continue

            for (i in 0 until pointerCount) {
                val p = step.pointers[i]
                props[i].clear()
                props[i].id = p.id
                props[i].toolType = MotionEvent.TOOL_TYPE_FINGER

                coords[i].clear()
                coords[i].x = p.x
                coords[i].y = p.y
                coords[i].pressure = p.pressure
                coords[i].size = 1.0f
            }

            val androidAction = when (step.action) {
                SynthAction.DOWN -> MotionEvent.ACTION_DOWN
                SynthAction.UP -> MotionEvent.ACTION_UP
                SynthAction.MOVE -> MotionEvent.ACTION_MOVE
                SynthAction.POINTER_DOWN ->
                    MotionEvent.ACTION_POINTER_DOWN or (step.actionIndex shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
                SynthAction.POINTER_UP ->
                    MotionEvent.ACTION_POINTER_UP or (step.actionIndex shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
            }

            @Suppress("DEPRECATION")
            val event = MotionEvent.obtain(
                touchDownTimeMs,
                nowMs,
                androidAction,
                pointerCount,
                props,
                coords,
                0,
                0,
                1.0f,
                1.0f,
                0,
                0,
                InputDevice.SOURCE_TOUCHSCREEN,
                0,
            )
            try {
                inject(event)
            } finally {
                event.recycle()
            }
        }
    }

    fun injectKey(key: KeyInput) {
        val hid = key.code.toInt()
        val meta = modifierState.onKey(hid, key.down)
        val androidCode = HidToAndroidKeyMap.androidKeyCode(hid) ?: return
        val nowMs = SystemClock.uptimeMillis()
        val action = if (key.down) KeyEvent.ACTION_DOWN else KeyEvent.ACTION_UP
        val event = KeyEvent(
            nowMs,
            nowMs,
            action,
            androidCode,
            0,
            meta,
            KeyCharacterMap.VIRTUAL_KEYBOARD,
            0,
            0,
            InputDevice.SOURCE_KEYBOARD,
        )
        inject(event)
    }

    fun reset() {
        synthesizer.reset()
        modifierState.reset()
    }

    private fun inject(event: InputEvent) {
        val mgr = inputManagerInstance ?: return
        val method = injectMethod ?: return
        try {
            method.invoke(mgr, event, INJECT_INPUT_EVENT_MODE_ASYNC)
        } catch (_: Throwable) {
        }
    }

    companion object {
        private const val INJECT_INPUT_EVENT_MODE_ASYNC = 0

        private fun resolveInjector(): Pair<Any?, Method?> {
            for (className in listOf(
                "android.hardware.input.InputManagerGlobal",
                "android.hardware.input.InputManager",
            )) {
                try {
                    val cls = Class.forName(className)
                    val getInstance = cls.getMethod("getInstance")
                    val mgr = getInstance.invoke(null)
                    val inject = cls.getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
                    return mgr to inject
                } catch (_: Throwable) {
                }
            }
            return null to null
        }
    }
}
