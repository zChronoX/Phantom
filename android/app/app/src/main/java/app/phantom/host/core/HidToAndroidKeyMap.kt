package app.phantom.host.core

/**
 * Traduzione da codici USB HID (Usage Page 0x07) ricevuti dal protocollo Phantom a codici `KeyEvent` Android.
 * Scritto senza dipendenze Android runtime per poter essere provato su JVM.
 */
object HidToAndroidKeyMap {
    const val KEYCODE_HOME = 3
    const val KEYCODE_BACK = 4
    const val KEYCODE_0 = 7
    const val KEYCODE_DPAD_UP = 19
    const val KEYCODE_DPAD_DOWN = 20
    const val KEYCODE_DPAD_LEFT = 21
    const val KEYCODE_DPAD_RIGHT = 22
    const val KEYCODE_VOLUME_UP = 24
    const val KEYCODE_VOLUME_DOWN = 25
    const val KEYCODE_POWER = 26
    const val KEYCODE_A = 29
    const val KEYCODE_COMMA = 55
    const val KEYCODE_PERIOD = 56
    const val KEYCODE_ALT_LEFT = 57
    const val KEYCODE_ALT_RIGHT = 58
    const val KEYCODE_SHIFT_LEFT = 59
    const val KEYCODE_SHIFT_RIGHT = 60
    const val KEYCODE_TAB = 61
    const val KEYCODE_SPACE = 62
    const val KEYCODE_ENTER = 66
    const val KEYCODE_DEL = 67
    const val KEYCODE_GRAVE = 68
    const val KEYCODE_MINUS = 69
    const val KEYCODE_EQUALS = 70
    const val KEYCODE_LEFT_BRACKET = 71
    const val KEYCODE_RIGHT_BRACKET = 72
    const val KEYCODE_BACKSLASH = 73
    const val KEYCODE_SEMICOLON = 74
    const val KEYCODE_APOSTROPHE = 75
    const val KEYCODE_SLASH = 76
    const val KEYCODE_MENU = 82
    const val KEYCODE_PAGE_UP = 92
    const val KEYCODE_PAGE_DOWN = 93
    const val KEYCODE_FORWARD_DEL = 112
    const val KEYCODE_CTRL_LEFT = 113
    const val KEYCODE_CTRL_RIGHT = 114
    const val KEYCODE_CAPS_LOCK = 115
    const val KEYCODE_META_LEFT = 117
    const val KEYCODE_META_RIGHT = 118
    const val KEYCODE_MOVE_END = 123
    const val KEYCODE_F1 = 131

    const val META_ALT_ON = 0x02
    const val META_ALT_LEFT_ON = 0x10
    const val META_ALT_RIGHT_ON = 0x20
    const val META_SHIFT_ON = 0x01
    const val META_SHIFT_LEFT_ON = 0x40
    const val META_SHIFT_RIGHT_ON = 0x80
    const val META_CTRL_ON = 0x1000
    const val META_CTRL_LEFT_ON = 0x2000
    const val META_CTRL_RIGHT_ON = 0x4000
    const val META_META_ON = 0x10000
    const val META_META_LEFT_ON = 0x20000
    const val META_META_RIGHT_ON = 0x40000

    fun androidKeyCode(hidCode: Int): Int? {
        if (hidCode in 0x04..0x1D) return KEYCODE_A + (hidCode - 0x04)
        if (hidCode in 0x1E..0x26) return 8 + (hidCode - 0x1E)
        if (hidCode in 0x3A..0x45) return KEYCODE_F1 + (hidCode - 0x3A)

        return when (hidCode) {
            0x27 -> KEYCODE_0
            0x28 -> KEYCODE_ENTER
            0x29 -> KEYCODE_BACK
            0x2A -> KEYCODE_DEL
            0x2B -> KEYCODE_TAB
            0x2C -> KEYCODE_SPACE
            0x2D -> KEYCODE_MINUS
            0x2E -> KEYCODE_EQUALS
            0x2F -> KEYCODE_LEFT_BRACKET
            0x30 -> KEYCODE_RIGHT_BRACKET
            0x31 -> KEYCODE_BACKSLASH
            0x33 -> KEYCODE_SEMICOLON
            0x34 -> KEYCODE_APOSTROPHE
            0x35 -> KEYCODE_GRAVE
            0x36 -> KEYCODE_COMMA
            0x37 -> KEYCODE_PERIOD
            0x38 -> KEYCODE_SLASH
            0x39 -> KEYCODE_CAPS_LOCK
            0x4A -> KEYCODE_HOME
            0x4B -> KEYCODE_PAGE_UP
            0x4C -> KEYCODE_FORWARD_DEL
            0x4D -> KEYCODE_MOVE_END
            0x4E -> KEYCODE_PAGE_DOWN
            0x4F -> KEYCODE_DPAD_RIGHT
            0x50 -> KEYCODE_DPAD_LEFT
            0x51 -> KEYCODE_DPAD_DOWN
            0x52 -> KEYCODE_DPAD_UP
            0x65 -> KEYCODE_MENU
            0x66 -> KEYCODE_POWER
            0x80 -> KEYCODE_VOLUME_UP
            0x81 -> KEYCODE_VOLUME_DOWN
            0xE0 -> KEYCODE_CTRL_LEFT
            0xE1 -> KEYCODE_SHIFT_LEFT
            0xE2 -> KEYCODE_ALT_LEFT
            0xE3 -> KEYCODE_META_LEFT
            0xE4 -> KEYCODE_CTRL_RIGHT
            0xE5 -> KEYCODE_SHIFT_RIGHT
            0xE6 -> KEYCODE_ALT_RIGHT
            0xE7 -> KEYCODE_META_RIGHT
            else -> null
        }
    }
}

class KeyModifierState {
    private var ctrlLeft = false
    private var ctrlRight = false
    private var shiftLeft = false
    private var shiftRight = false
    private var altLeft = false
    private var altRight = false
    private var metaLeft = false
    private var metaRight = false

    fun reset() {
        ctrlLeft = false
        ctrlRight = false
        shiftLeft = false
        shiftRight = false
        altLeft = false
        altRight = false
        metaLeft = false
        metaRight = false
    }

    fun onKey(hidCode: Int, down: Boolean): Int {
        when (hidCode) {
            0xE0 -> ctrlLeft = down
            0xE1 -> shiftLeft = down
            0xE2 -> altLeft = down
            0xE3 -> metaLeft = down
            0xE4 -> ctrlRight = down
            0xE5 -> shiftRight = down
            0xE6 -> altRight = down
            0xE7 -> metaRight = down
        }
        return currentMetaState()
    }

    fun currentMetaState(): Int {
        var meta = 0
        if (shiftLeft || shiftRight) {
            meta = meta or HidToAndroidKeyMap.META_SHIFT_ON
            if (shiftLeft) meta = meta or HidToAndroidKeyMap.META_SHIFT_LEFT_ON
            if (shiftRight) meta = meta or HidToAndroidKeyMap.META_SHIFT_RIGHT_ON
        }
        if (altLeft || altRight) {
            meta = meta or HidToAndroidKeyMap.META_ALT_ON
            if (altLeft) meta = meta or HidToAndroidKeyMap.META_ALT_LEFT_ON
            if (altRight) meta = meta or HidToAndroidKeyMap.META_ALT_RIGHT_ON
        }
        if (ctrlLeft || ctrlRight) {
            meta = meta or HidToAndroidKeyMap.META_CTRL_ON
            if (ctrlLeft) meta = meta or HidToAndroidKeyMap.META_CTRL_LEFT_ON
            if (ctrlRight) meta = meta or HidToAndroidKeyMap.META_CTRL_RIGHT_ON
        }
        if (metaLeft || metaRight) {
            meta = meta or HidToAndroidKeyMap.META_META_ON
            if (metaLeft) meta = meta or HidToAndroidKeyMap.META_META_LEFT_ON
            if (metaRight) meta = meta or HidToAndroidKeyMap.META_META_RIGHT_ON
        }
        return meta
    }
}
