import Foundation

func check(_ ok: Bool, _ msg: String) {
    if !ok {
        fputs("FALLITO: \(msg)\n", stderr)
        exit(1)
    }
}

// 1. VideoRect aspect-fit e normalizzazione
do {
    // Video 1080x2400 dentro schermo iPhone 430x932 -> bande ai lati (video più stretto)
    let rect = VideoRect.aspectFit(viewWidth: 430, viewHeight: 932, videoWidth: 1080, videoHeight: 2400)
    let expectedW = 932.0 * (1080.0 / 2400.0)
    check(abs(rect.height - 932.0) < 0.01, "altezza aspectFit: \(rect.height)")
    check(abs(rect.width - expectedW) < 0.01, "larghezza aspectFit: \(rect.width)")
    check(abs(rect.x - (430.0 - expectedW) / 2.0) < 0.01, "offset x: \(rect.x)")

    // Video verticale dentro vista landscape 932x430 -> ampie bande laterali
    let land = VideoRect.aspectFit(viewWidth: 932, viewHeight: 430, videoWidth: 1080, videoHeight: 2400)
    check(abs(land.height - 430.0) < 0.01, "altezza landscape")
    check(land.x > 300, "banda laterale in landscape: \(land.x)")

    // Coordinate normalizzate ai bordi e al centro
    let tl = rect.normalize(x: rect.x, y: rect.y)
    let br = rect.normalize(x: rect.x + rect.width, y: rect.y + rect.height)
    let clamped = rect.normalize(x: -100, y: 5000)
    check(tl == (0, 0), "angolo alto-sinistra: \(tl)")
    check(br == (65535, 65535), "angolo basso-destra: \(br)")
    check(clamped == (0, 65535), "clamp fuori dai bordi: \(clamped)")
    print("ok · aspectFit e normalizzazione")
}

// 2. TouchTracker multi-finger e riutilizzo slot
do {
    let rect = VideoRect(x: 100, y: 0, width: 200, height: 400)
    let tracker = TouchTracker()

    // Tocco iniziale nella banda nera sinistra -> ignorato
    let ignored = tracker.begin(touchID: 1, x: 50, y: 100, in: rect, requireInsideRect: true)
    check(!ignored && tracker.currentFingers.isEmpty, "tocco iniziale in banda nera ignorato")

    // Tocco valido al centro
    let ok1 = tracker.begin(touchID: 10, x: 200, y: 200, pressure: 120, in: rect)
    let ok2 = tracker.begin(touchID: 20, x: 100, y: 400, pressure: 200, in: rect)
    check(ok1 && ok2, "due dita attive")
    check(tracker.currentFingers == [
        NormalizedFinger(id: 0, x: 32768, y: 32768, pressure: 120),
        NormalizedFinger(id: 1, x: 0, y: 65535, pressure: 200),
    ], "stati iniziali: \(tracker.currentFingers)")

    // Trascinamento fuori dal video -> resta ancorato al bordo senza cadere
    tracker.move(touchID: 10, x: 0, y: 100, pressure: 150, in: rect)
    check(tracker.currentFingers[0] == NormalizedFinger(id: 0, x: 0, y: 16384, pressure: 150), "move clampato")

    // Rilascio dito 0 e nuovo tocco -> riutilizza slot 0 mantenendo slot 1 sul secondo dito
    tracker.end(touchID: 10)
    check(tracker.currentFingers.count == 1 && tracker.currentFingers[0].id == 1, "slot 1 invariato")
    check(tracker.begin(touchID: 30, x: 300, y: 0, pressure: 80, in: rect), "nuovo dito prende slot 0")
    check(tracker.currentFingers.map(\.id) == [0, 1], "slot riutilizzato in ordine")

    // Cap a 10 dita
    tracker.reset()
    for i in 1...12 {
        let added = tracker.begin(touchID: UInt(i), x: 150, y: 200, in: rect)
        check(added == (i <= 10), "limite 10 dita al passo \(i)")
    }
    check(tracker.currentFingers.count == 10, "esattamente 10 dita")
    print("ok · TouchTracker multi-finger")
}

// 3. EndpointParser e CertificateCodec
do {
    check(EndpointParser.normalizeAddress("192.168.1.118") == "192.168.1.118:47800", "default port")
    check(EndpointParser.normalizeAddress(" 192.168.1.118:48000 ") == "192.168.1.118:48000", "explicit port")
    check(EndpointParser.normalizeAddress("[::1]:47800") == "[::1]:47800", "ipv6 with port")
    check(EndpointParser.normalizeAddress("::1") == "[::1]:47800", "bare ipv6")
    check(EndpointParser.normalizeAddress("") == nil && EndpointParser.normalizeAddress("1.2.3.4:0") == nil, "invalid")

    let sample = Data([0x30, 0x82, 0x01, 0x0A, 0xFF])
    let hex = CertificateCodec.encodeHex(sample)
    check(hex == "3082010aff", "encodeHex")
    check(CertificateCodec.decode("30:82:01:0A FF") == sample, "decode hex with separators")
    check(CertificateCodec.decode(sample.base64EncodedString()) == sample, "decode base64")
    check(CertificateCodec.decode("   ") == nil, "empty cert nil")
    print("ok · EndpointParser e CertificateCodec")
}

// 4. HidCharacterMapper e RemoteQuickAction
do {
    check(HidCharacterMapper.events(for: "a") == [
        HidKeyEvent(code: 0x04, down: true),
        HidKeyEvent(code: 0x04, down: false),
    ], "minuscola a")
    check(HidCharacterMapper.events(for: "A") == [
        HidKeyEvent(code: HidKeyCode.leftShift, down: true),
        HidKeyEvent(code: 0x04, down: true),
        HidKeyEvent(code: 0x04, down: false),
        HidKeyEvent(code: HidKeyCode.leftShift, down: false),
    ], "maiuscola A con Shift")
    check(HidCharacterMapper.events(for: "\n") == [
        HidKeyEvent(code: HidKeyCode.enter, down: true),
        HidKeyEvent(code: HidKeyCode.enter, down: false),
    ], "invio")
    check(RemoteQuickAction.back.hidSequence.first == HidKeyEvent(code: HidKeyCode.escape, down: true), "back -> escape")
    check(RemoteQuickAction.home.hidSequence.first == HidKeyEvent(code: HidKeyCode.home, down: true), "home")
    check(RemoteQuickAction.recents.hidSequence.count == 4, "recents alt+tab")
    print("ok · HidCharacterMapper e RemoteQuickAction")
}

print("Tutti i test superati.")
