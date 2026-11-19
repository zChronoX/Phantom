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
    check(!tracker.move(touchID: 1, x: 150, y: 150, in: rect), "move su tocco mai tracciato restituisce false")
    check(!tracker.end(touchID: 1), "end su tocco mai tracciato restituisce false")

    // Tocco valido al centro
    let ok1 = tracker.begin(touchID: 10, x: 200, y: 200, pressure: 120, in: rect)
    let ok2 = tracker.begin(touchID: 20, x: 100, y: 400, pressure: 200, in: rect)
    check(ok1 && ok2, "due dita attive")
    check(tracker.currentFingers == [
        NormalizedFinger(id: 0, x: 32768, y: 32768, pressure: 120),
        NormalizedFinger(id: 1, x: 0, y: 65535, pressure: 200),
    ], "stati iniziali: \(tracker.currentFingers)")

    // Trascinamento fuori dal video -> resta ancorato al bordo senza cadere
    check(tracker.move(touchID: 10, x: 0, y: 100, pressure: 150, in: rect), "move valido restituisce true")
    check(tracker.currentFingers[0] == NormalizedFinger(id: 0, x: 0, y: 16384, pressure: 150), "move clampato")

    // Rilascio dito 0 e nuovo tocco -> riutilizza slot 0 mantenendo slot 1 sul secondo dito
    check(tracker.end(touchID: 10), "end valido restituisce true")
    check(!tracker.end(touchID: 10), "secondo end sullo stesso dito restituisce false")
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

// 5. TouchDispatchState (rilascio ultimo dito senza rimanere incastrato)
do {
    var dispatch = TouchDispatchState()
    check(!dispatch.shouldSendOnTick, "inizialmente inattivo")

    let v1 = dispatch.update([NormalizedFinger(id: 0, x: 1000, y: 2000, pressure: 128)])
    check(dispatch.shouldSendOnTick, "con dita appoggiate manda sempre")
    dispatch.markSent(version: v1)
    check(!dispatch.dirty && dispatch.shouldSendOnTick, "continua a mandare mentre il dito è giù")

    // Rilascio rapido bloccato dal rate-limit: deve restare in attesa finché `markSent(v2)` non passa
    let v2 = dispatch.update([])
    check(dispatch.shouldSendOnTick && dispatch.dirty, "rilascio ultimo dito resta pronto per onTick")
    dispatch.markSent(version: v1) // versione vecchia ignorata
    check(dispatch.dirty && dispatch.shouldSendOnTick, "versione vecchia non cancella dirty")

    // Primo invio di [] + 1 ripetizione di sicurezza, poi idle
    dispatch.markSent(version: v2)
    check(!dispatch.dirty && dispatch.shouldSendOnTick, "una ripetizione di sicurezza dopo il rilascio")
    dispatch.markSent(version: v2)
    check(!dispatch.shouldSendOnTick, "dopo la ripetizione torna inattivo")
    print("ok · TouchDispatchState")
}

// 6. AnnexBScanner (zero-copy NAL ranges & parameter sets)
do {
    check(AnnexBScanner.nalRanges(in: Data()).isEmpty, "vuoto")
    check(AnnexBScanner.nalRanges(in: Data([0, 0, 1])).isEmpty, "solo start code")

    let h264 = Data([0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88])
    let r264 = AnnexBScanner.nalRanges(in: h264)
    check(r264 == [4..<6, 9..<11, 15..<17], "intervalli H.264: \(r264)")
    check(AnnexBScanner.parameterSetKind(firstByte: h264[r264[0].lowerBound], isHEVC: false) == .sps, "H.264 SPS")
    check(AnnexBScanner.parameterSetKind(firstByte: h264[r264[1].lowerBound], isHEVC: false) == .pps, "H.264 PPS")
    check(!AnnexBScanner.isParameterSet(firstByte: h264[r264[2].lowerBound], isHEVC: false), "H.264 IDR")

    // H.265 NAL header byte = (nal_unit_type << 1): VPS=32 (0x40), SPS=33 (0x42), PPS=34 (0x44), IDR=19 (0x26)
    let h265 = Data([0, 0, 1, 0x40, 0x01, 0, 0, 1, 0x42, 0x01, 0, 0, 1, 0x44, 0x01, 0, 0, 0, 1, 0x26, 0xAA])
    let r265 = AnnexBScanner.nalRanges(in: h265)
    check(r265.count == 4, "4 NAL H.265")
    check(AnnexBScanner.parameterSetKind(firstByte: h265[r265[0].lowerBound], isHEVC: true) == .vps, "HEVC VPS")
    check(AnnexBScanner.parameterSetKind(firstByte: h265[r265[1].lowerBound], isHEVC: true) == .sps, "HEVC SPS")
    check(AnnexBScanner.parameterSetKind(firstByte: h265[r265[2].lowerBound], isHEVC: true) == .pps, "HEVC PPS")
    check(!AnnexBScanner.isParameterSet(firstByte: h265[r265[3].lowerBound], isHEVC: true), "HEVC slice")
    print("ok · AnnexBScanner")
}

print("Tutti i test superati.")
