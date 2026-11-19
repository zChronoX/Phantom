package app.phantom.host.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class HidToAndroidKeyMapTest {
    @Test
    fun mapsLettersDigitsArrowsAndAndroidSystemKeys() {
        assertEquals(HidToAndroidKeyMap.KEYCODE_A, HidToAndroidKeyMap.androidKeyCode(0x04))
        assertEquals(HidToAndroidKeyMap.KEYCODE_A + 25, HidToAndroidKeyMap.androidKeyCode(0x1D))
        assertEquals(8, HidToAndroidKeyMap.androidKeyCode(0x1E)) // 1
        assertEquals(HidToAndroidKeyMap.KEYCODE_0, HidToAndroidKeyMap.androidKeyCode(0x27))
        assertEquals(HidToAndroidKeyMap.KEYCODE_ENTER, HidToAndroidKeyMap.androidKeyCode(0x28))
        assertEquals(HidToAndroidKeyMap.KEYCODE_BACK, HidToAndroidKeyMap.androidKeyCode(0x29))
        assertEquals(HidToAndroidKeyMap.KEYCODE_HOME, HidToAndroidKeyMap.androidKeyCode(0x4A))
        assertEquals(HidToAndroidKeyMap.KEYCODE_DPAD_RIGHT, HidToAndroidKeyMap.androidKeyCode(0x4F))
        assertEquals(HidToAndroidKeyMap.KEYCODE_DPAD_UP, HidToAndroidKeyMap.androidKeyCode(0x52))
        assertEquals(HidToAndroidKeyMap.KEYCODE_VOLUME_UP, HidToAndroidKeyMap.androidKeyCode(0x80))
        assertEquals(HidToAndroidKeyMap.KEYCODE_POWER, HidToAndroidKeyMap.androidKeyCode(0x66))
        assertNull(HidToAndroidKeyMap.androidKeyCode(0x9999))
    }

    @Test
    fun tracksModifierBitmaskAcrossPressAndRelease() {
        val state = KeyModifierState()
        val withShift = state.onKey(0xE1, down = true)
        assertEquals(HidToAndroidKeyMap.META_SHIFT_ON or HidToAndroidKeyMap.META_SHIFT_LEFT_ON, withShift)

        val withCtrlShift = state.onKey(0xE0, down = true)
        assertEquals(
            HidToAndroidKeyMap.META_SHIFT_ON or
                HidToAndroidKeyMap.META_SHIFT_LEFT_ON or
                HidToAndroidKeyMap.META_CTRL_ON or
                HidToAndroidKeyMap.META_CTRL_LEFT_ON,
            withCtrlShift,
        )

        val onlyCtrl = state.onKey(0xE1, down = false)
        assertEquals(HidToAndroidKeyMap.META_CTRL_ON or HidToAndroidKeyMap.META_CTRL_LEFT_ON, onlyCtrl)

        assertEquals(0, state.onKey(0xE0, down = false))
    }
}
