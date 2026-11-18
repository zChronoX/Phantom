import Foundation

/// Codici USB HID (Usage Page 0x07 Keyboard/Keypad) usati da `Message::Key` nel protocollo Phantom.
enum HidKeyCode {
    static let enter: UInt16 = 0x28
    static let escape: UInt16 = 0x29
    static let backspace: UInt16 = 0x2A
    static let tab: UInt16 = 0x2B
    static let space: UInt16 = 0x2C
    static let minus: UInt16 = 0x2D
    static let equal: UInt16 = 0x2E
    static let leftBracket: UInt16 = 0x2F
    static let rightBracket: UInt16 = 0x30
    static let backslash: UInt16 = 0x31
    static let semicolon: UInt16 = 0x33
    static let apostrophe: UInt16 = 0x34
    static let grave: UInt16 = 0x35
    static let comma: UInt16 = 0x36
    static let period: UInt16 = 0x37
    static let slash: UInt16 = 0x38

    static let home: UInt16 = 0x4A
    static let deleteForward: UInt16 = 0x4C
    static let rightArrow: UInt16 = 0x4F
    static let leftArrow: UInt16 = 0x50
    static let downArrow: UInt16 = 0x51
    static let upArrow: UInt16 = 0x52

    static let application: UInt16 = 0x65
    static let power: UInt16 = 0x66
    static let volumeUp: UInt16 = 0x80
    static let volumeDown: UInt16 = 0x81

    static let leftControl: UInt16 = 0xE0
    static let leftShift: UInt16 = 0xE1
    static let leftAlt: UInt16 = 0xE2
    static let leftGui: UInt16 = 0xE3
}

/// Singolo evento di tastiera pronto da inviare via `PhantomClient.sendKey`.
struct HidKeyEvent: Equatable {
    var code: UInt16
    var down: Bool
}

/// Azioni rapide Android disponibili nell'overlay di controllo remoto.
enum RemoteQuickAction: CaseIterable {
    case back
    case home
    case recents
    case volumeUp
    case volumeDown
    case power

    var hidSequence: [HidKeyEvent] {
        switch self {
        case .back:
            return [HidKeyEvent(code: HidKeyCode.escape, down: true), HidKeyEvent(code: HidKeyCode.escape, down: false)]
        case .home:
            return [HidKeyEvent(code: HidKeyCode.home, down: true), HidKeyEvent(code: HidKeyCode.home, down: false)]
        case .recents:
            // Alt + Tab apre il selettore app recenti su Android in assenza di tasti consumer dedicati
            return [
                HidKeyEvent(code: HidKeyCode.leftAlt, down: true),
                HidKeyEvent(code: HidKeyCode.tab, down: true),
                HidKeyEvent(code: HidKeyCode.tab, down: false),
                HidKeyEvent(code: HidKeyCode.leftAlt, down: false),
            ]
        case .volumeUp:
            return [HidKeyEvent(code: HidKeyCode.volumeUp, down: true), HidKeyEvent(code: HidKeyCode.volumeUp, down: false)]
        case .volumeDown:
            return [HidKeyEvent(code: HidKeyCode.volumeDown, down: true), HidKeyEvent(code: HidKeyCode.volumeDown, down: false)]
        case .power:
            return [HidKeyEvent(code: HidKeyCode.power, down: true), HidKeyEvent(code: HidKeyCode.power, down: false)]
        }
    }
}

/// Converte caratteri digitati dalla tastiera su schermo di iOS nella sequenza di eventi HID (layout US standard).
enum HidCharacterMapper {
    static func events(for text: String) -> [HidKeyEvent] {
        var out: [HidKeyEvent] = []
        for ch in text {
            out.append(contentsOf: events(for: ch))
        }
        return out
    }

    static func events(for ch: Character) -> [HidKeyEvent] {
        guard let mapped = keyMapping(for: ch) else { return [] }
        if mapped.shift {
            return [
                HidKeyEvent(code: HidKeyCode.leftShift, down: true),
                HidKeyEvent(code: mapped.code, down: true),
                HidKeyEvent(code: mapped.code, down: false),
                HidKeyEvent(code: HidKeyCode.leftShift, down: false),
            ]
        }
        return [
            HidKeyEvent(code: mapped.code, down: true),
            HidKeyEvent(code: mapped.code, down: false),
        ]
    }

    private static func keyMapping(for ch: Character) -> (code: UInt16, shift: Bool)? {
        if let scalar = ch.asciiScalars.first, ch.asciiScalars.count == 1 {
            let v = scalar.value
            if v >= 97 && v <= 122 { // a-z
                return (UInt16(0x04 + (v - 97)), false)
            }
            if v >= 65 && v <= 90 { // A-Z
                return (UInt16(0x04 + (v - 65)), true)
            }
            if v >= 49 && v <= 57 { // 1-9
                return (UInt16(0x1E + (v - 49)), false)
            }
        }

        switch ch {
        case "0": return (0x27, false)
        case "\n", "\r": return (HidKeyCode.enter, false)
        case "\t": return (HidKeyCode.tab, false)
        case " ": return (HidKeyCode.space, false)
        case "-": return (HidKeyCode.minus, false)
        case "_": return (HidKeyCode.minus, true)
        case "=": return (HidKeyCode.equal, false)
        case "+": return (HidKeyCode.equal, true)
        case "[": return (HidKeyCode.leftBracket, false)
        case "{": return (HidKeyCode.leftBracket, true)
        case "]": return (HidKeyCode.rightBracket, false)
        case "}": return (HidKeyCode.rightBracket, true)
        case "\\": return (HidKeyCode.backslash, false)
        case "|": return (HidKeyCode.backslash, true)
        case ";": return (HidKeyCode.semicolon, false)
        case ":": return (HidKeyCode.semicolon, true)
        case "'": return (HidKeyCode.apostrophe, false)
        case "\"": return (HidKeyCode.apostrophe, true)
        case "`": return (HidKeyCode.grave, false)
        case "~": return (HidKeyCode.grave, true)
        case ",": return (HidKeyCode.comma, false)
        case "<": return (HidKeyCode.comma, true)
        case ".": return (HidKeyCode.period, false)
        case ">": return (HidKeyCode.period, true)
        case "/": return (HidKeyCode.slash, false)
        case "?": return (HidKeyCode.slash, true)
        case "!": return (0x1E, true)
        case "@": return (0x1F, true)
        case "#": return (0x20, true)
        case "$": return (0x21, true)
        case "%": return (0x22, true)
        case "^": return (0x23, true)
        case "&": return (0x24, true)
        case "*": return (0x25, true)
        case "(": return (0x26, true)
        case ")": return (0x27, true)
        default: return nil
        }
    }
}

private extension Character {
    var asciiScalars: [UnicodeScalar] {
        unicodeScalars.filter(\.isASCII)
    }
}
