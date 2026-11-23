import SwiftUI
import UIKit
import AVFoundation
import CoreMedia
import GameController

/// Superficie full-screen che mostra i frame decodificati e cattura multi-touch a bassa latenza + tastiera.
final class RemoteDisplayTouchView: UIView, UIKeyInput {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }

    var onTouchesChanged: (([NormalizedFinger]) -> Void)?
    var onKeyEvents: (([HidKeyEvent]) -> Void)?
    var onKeyboardStateChanged: ((Bool) -> Void)?

    var videoSize = VideoDimensions.zero
    var showTouchIndicators = true {
        didSet {
            if !showTouchIndicators { clearTouchLayers() }
        }
    }

    private let tracker = TouchTracker(maxFingers: Int(maxFingers()))
    private var touchLayers: [UInt: CAShapeLayer] = [:]
    private let renderQueue = DispatchQueue(label: "app.phantom.remote.render", qos: .userInteractive)
    private var videoRenderer: AVSampleBufferVideoRenderer?

    // MARK: - UITextInputTraits (tastiera software senza correzioni iOS)
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
        videoRenderer = displayLayer.sampleBufferRenderer
    }

    required init?(coder: NSCoder) { fatalError("non supportato") }

    /// Chiamato (sulla coda di rendering) quando un frame non arriva al decoder o il decoder fallisce: serve un
    /// keyframe, altrimenti i frame successivi restano corrotti fino al prossimo I-frame periodico.
    var onFrameDropped: (() -> Void)?

    func enqueueOnBackground(_ sb: CMSampleBuffer, flushBeforeEnqueue: Bool) {
        guard let renderer = videoRenderer else { return }
        renderQueue.async { [weak self] in
            // `flushBeforeEnqueue` arriva con un keyframe (formato nuovo): ripartire da lì è sicuro. Un flush per
            // errore del decoder, invece, lascia il decoder senza riferimento: il prossimo frame utile è un keyframe.
            let decoderFailed = renderer.status == .failed || renderer.requiresFlushToResumeDecoding
            if flushBeforeEnqueue || decoderFailed {
                renderer.flush()
            }
            if renderer.isReadyForMoreMediaData {
                renderer.enqueue(sb)
                if decoderFailed && !flushBeforeEnqueue {
                    self?.onFrameDropped?()
                }
            } else {
                // Prima il frame veniva perso in silenzio e l'immagine restava rovinata anche per secondi.
                self?.onFrameDropped?()
            }
        }
    }

    // MARK: - Tastiera software (UIKeyInput) e fisica (presses)

    override var canBecomeFirstResponder: Bool { true }

    @discardableResult
    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        onKeyboardStateChanged?(ok)
        return ok
    }

    @discardableResult
    override func resignFirstResponder() -> Bool {
        let ok = super.resignFirstResponder()
        onKeyboardStateChanged?(!ok)
        return ok
    }

    var hasText: Bool { true }

    func insertText(_ text: String) {
        let events = HidCharacterMapper.events(for: text)
        if !events.isEmpty { onKeyEvents?(events) }
    }

    func deleteBackward() {
        onKeyEvents?([
            HidKeyEvent(code: HidKeyCode.backspace, down: true),
            HidKeyEvent(code: HidKeyCode.backspace, down: false),
        ])
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var events: [HidKeyEvent] = []
        var unhandled: Set<UIPress> = []
        for press in presses {
            if let key = press.key {
                events.append(HidKeyEvent(code: UInt16(key.keyCode.rawValue), down: true))
            } else {
                unhandled.insert(press)
            }
        }
        if !events.isEmpty { onKeyEvents?(events) }
        if !unhandled.isEmpty { super.pressesBegan(unhandled, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var events: [HidKeyEvent] = []
        var unhandled: Set<UIPress> = []
        for press in presses {
            if let key = press.key {
                events.append(HidKeyEvent(code: UInt16(key.keyCode.rawValue), down: false))
            } else {
                unhandled.insert(press)
            }
        }
        if !events.isEmpty { onKeyEvents?(events) }
        if !unhandled.isEmpty { super.pressesEnded(unhandled, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        pressesEnded(presses, with: event)
    }

    // MARK: - Multi-touch a bassa latenza

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        processTouches(touches, event: event, phase: .began)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        processTouches(touches, event: event, phase: .moved)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        processTouches(touches, event: event, phase: .ended)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        processTouches(touches, event: event, phase: .ended)
    }

    func resetTouches() {
        tracker.reset()
        clearTouchLayers()
        onTouchesChanged?([])
    }

    private enum Phase { case began, moved, ended }

    private func processTouches(_ touches: Set<UITouch>, event: UIEvent?, phase: Phase) {
        let rect = VideoRect.aspectFit(
            viewWidth: Double(bounds.width),
            viewHeight: Double(bounds.height),
            videoWidth: videoSize.width,
            videoHeight: videoSize.height
        )
        var changed = false

        for touch in touches {
            let touchID = UInt(bitPattern: ObjectIdentifier(touch))
            switch phase {
            case .began:
                let p = touch.location(in: self)
                let pressure = normalizedPressure(touch)
                let accepted = tracker.begin(
                    touchID: touchID,
                    x: Double(p.x),
                    y: Double(p.y),
                    pressure: pressure,
                    in: rect,
                    requireInsideRect: true
                )
                if accepted {
                    updateTouchLayer(id: touchID, at: p, visible: true)
                    changed = true
                }

            case .moved:
                let last = event?.coalescedTouches(for: touch)?.last ?? touch
                let p = last.location(in: self)
                let pressure = normalizedPressure(last)
                if tracker.move(touchID: touchID, x: Double(p.x), y: Double(p.y), pressure: pressure, in: rect) {
                    updateTouchLayer(id: touchID, at: p, visible: true)
                    changed = true
                }

            case .ended:
                if tracker.end(touchID: touchID) {
                    updateTouchLayer(id: touchID, at: .zero, visible: false)
                    changed = true
                }
            }
        }

        if changed {
            onTouchesChanged?(tracker.currentFingers)
        }
    }

    private func normalizedPressure(_ touch: UITouch) -> UInt8 {
        guard touch.maximumPossibleForce > 0 else { return 128 }
        let scaled = (touch.force / touch.maximumPossibleForce) * 255.0
        return UInt8(min(255.0, max(1.0, scaled.rounded())))
    }

    private func updateTouchLayer(id: UInt, at point: CGPoint, visible: Bool) {
        guard visible && showTouchIndicators else {
            touchLayers[id]?.removeFromSuperlayer()
            touchLayers[id] = nil
            return
        }
        let ring: CAShapeLayer
        if let existing = touchLayers[id] {
            ring = existing
        } else {
            ring = CAShapeLayer()
            ring.path = UIBezierPath(ovalIn: CGRect(x: -20, y: -20, width: 40, height: 40)).cgPath
            ring.fillColor = UIColor.white.withAlphaComponent(0.22).cgColor
            ring.strokeColor = UIColor.white.withAlphaComponent(0.82).cgColor
            ring.lineWidth = 2
            layer.addSublayer(ring)
            touchLayers[id] = ring
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        ring.position = point
        CATransaction.commit()
    }

    private func clearTouchLayers() {
        for layer in touchLayers.values { layer.removeFromSuperlayer() }
        touchLayers.removeAll()
    }
}

private final class RemoteDisplayViewController: UIViewController {
    let displayView = RemoteDisplayTouchView()

    override func loadView() {
        view = displayView
    }

    override var preferredScreenEdgesDeferringSystemGestures: UIRectEdge { .all }
    override var prefersHomeIndicatorAutoHidden: Bool { true }
    override var prefersStatusBarHidden: Bool { true }
}

private struct RemoteDisplayControllerRepresentable: UIViewControllerRepresentable {
    let session: RemoteSession
    let showTouchIndicators: Bool
    @Binding var keyboardVisible: Bool
    @Binding var toggleKeyboardToken: Int
    @Binding var resetTouchesToken: Int

    final class Coordinator {
        var lastToggleToken = 0
        var lastResetTouchesToken = 0
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIViewController(context: Context) -> RemoteDisplayViewController {
        let controller = RemoteDisplayViewController()
        let view = controller.displayView
        view.showTouchIndicators = showTouchIndicators
        view.onTouchesChanged = { [weak session] fingers in
            session?.updateTouches(fingers)
        }
        view.onKeyEvents = { [weak session] events in
            session?.sendKeyEvents(events)
        }
        view.onKeyboardStateChanged = { visible in
            DispatchQueue.main.async { keyboardVisible = visible }
        }
        session.onSample = { [weak view] sb, flush in
            view?.enqueueOnBackground(sb, flushBeforeEnqueue: flush)
        }
        view.onFrameDropped = { [weak session] in
            session?.requestKeyframeAfterDecoderDrop()
        }
        return controller
    }

    func updateUIViewController(_ uiViewController: RemoteDisplayViewController, context: Context) {
        let uiView = uiViewController.displayView
        let prevSize = uiView.videoSize
        let nextSize = session.videoSize
        if prevSize.isValid && nextSize.isValid && prevSize != nextSize {
            uiView.resetTouches()
        }
        uiView.videoSize = nextSize
        uiView.showTouchIndicators = showTouchIndicators
        if context.coordinator.lastResetTouchesToken != resetTouchesToken {
            context.coordinator.lastResetTouchesToken = resetTouchesToken
            uiView.resetTouches()
        }
        if context.coordinator.lastToggleToken != toggleKeyboardToken {
            context.coordinator.lastToggleToken = toggleKeyboardToken
            if uiView.isFirstResponder {
                uiView.resignFirstResponder()
            } else {
                uiView.becomeFirstResponder()
            }
        }
    }
}

/// Schermata full-screen stile emulatore per il controllo remoto di Android su QUIC.
struct RemoteSessionView: View {
    let mode: RemoteSessionMode
    let showTouchIndicators: Bool

    @StateObject private var session = RemoteSession()
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    /// Pannello laterale aperto (controlli e telemetria); da chiuso resta solo la linguetta sul bordo.
    @State private var panelOpen = false
    /// Lato della HUD: «trailing» (destra, predefinito) o «leading» (sinistra). Ricordato tra le sessioni.
    @AppStorage("hudEdge") private var hudEdgeRaw = "trailing"
    @GestureState private var panelDrag: CGFloat = 0
    @State private var keyboardVisible = false
    @State private var toggleKeyboardToken = 0
    @State private var resetTouchesToken = 0
    @State private var started = false
    @State private var controllerBridge = GamepadBridge()
    private let pillHaptic = UIImpactFeedbackGenerator(style: .light)

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()

            RemoteDisplayControllerRepresentable(
                session: session,
                showTouchIndicators: showTouchIndicators,
                keyboardVisible: $keyboardVisible,
                toggleKeyboardToken: $toggleKeyboardToken,
                resetTouchesToken: $resetTouchesToken
            )
            .ignoresSafeArea()

            if !session.metrics.connected {
                VStack {
                    Spacer()
                    connectionBanner
                    Spacer()
                }
            }

            sideHud

            if let toast = session.toastText {
                VStack {
                    Spacer()
                    Text(toast)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 9)
                        .phantomGlassCapsule(overVideo: true)
                        .padding(.bottom, 28)
                }
                .allowsHitTesting(false)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .onAppear { begin() }
        .onDisappear { end() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .inactive {
                resetTouchesToken &+= 1
            } else if phase == .background {
                // Prima qui si chiudeva la sessione e al ritorno si ricollegava da capo. Ora resta aperta: video in
                // pausa, audio e connessione attivi.
                resetTouchesToken &+= 1
                if started { session.enterBackground() }
            } else if phase == .active {
                if started {
                    session.enterForeground()
                } else {
                    begin()
                }
            }
        }
    }

    private func begin() {
        guard !started else { return }
        started = true
        session.start(mode: mode)
        controllerBridge.start { [weak session] code, down in
            session?.sendKey(code: code, down: down)
        }
    }

    private func end() {
        guard started else { return }
        started = false
        resetTouchesToken &+= 1
        controllerBridge.stop()
        session.stop()
    }

    private func reconnect() {
        end()
        begin()
    }

    private var connectionBanner: some View {
        VStack(spacing: 12) {
            if session.running {
                ProgressView()
                    .tint(.white)
            } else {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.title3)
                    .foregroundStyle(PhantomPalette.warnAmber)
            }
            Text(session.statusText)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
            if !session.running {
                Button("Riconnetti") { reconnect() }
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.black)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 9)
                    .background(.white, in: Capsule())
            }
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 18)
        .phantomGlassCard(cornerRadius: 22, overVideo: true)
        .padding(.horizontal, 28)
    }

    private var hudOnLeft: Bool { hudEdgeRaw == "leading" }

    private var hudEdge: Edge { hudOnLeft ? .leading : .trailing }

    private var hudAnimation: Animation { .spring(response: 0.3, dampingFraction: 0.86) }

    private func setPanel(open: Bool) {
        guard panelOpen != open else { return }
        pillHaptic.impactOccurred(intensity: 0.5)
        withAnimation(hudAnimation) { panelOpen = open }
    }

    /// Spostamento orizzontale «verso il centro dello schermo» (positivo) di un trascinamento.
    private func towardCenter(_ dx: CGFloat) -> CGFloat { hudOnLeft ? dx : -dx }

    /// HUD laterale: una linguetta fissa a metà altezza sul bordo (stato, FPS e Indietro/Home/Recenti sempre a portata)
    /// e un pannello che scorre dal lato con tutti i controlli e la telemetria. Il pannello si chiude toccando fuori,
    /// trascinandolo verso il bordo o toccando la freccia. Prima la HUD stava in alto, era difficile da spostare e il
    /// tasto «–» era piccolo e scomodo da raggiungere.
    private var sideHud: some View {
        GeometryReader { geo in
            ZStack(alignment: hudOnLeft ? Alignment.leading : Alignment.trailing) {
                if panelOpen {
                    // Velo trasparente: un tocco fuori chiude il pannello (e non arriva ad Android)
                    Color.black.opacity(0.18)
                        .ignoresSafeArea()
                        .contentShape(Rectangle())
                        .onTapGesture { setPanel(open: false) }
                        .transition(.opacity)
                }

                if panelOpen {
                    sidePanel(maxHeight: max(240, geo.size.height - 24), width: min(340, geo.size.width - 56))
                        .offset(x: hudOnLeft ? min(0, panelDrag) : max(0, panelDrag))
                        .gesture(
                            DragGesture(minimumDistance: 8)
                                .updating($panelDrag) { value, state, _ in state = value.translation.width }
                                .onEnded { value in
                                    if towardCenter(value.translation.width) < -50
                                        || towardCenter(value.predictedEndTranslation.width) < -120 {
                                        setPanel(open: false)
                                    }
                                }
                        )
                        .transition(.move(edge: hudEdge).combined(with: .opacity))
                } else {
                    edgeTab
                        .transition(.move(edge: hudEdge).combined(with: .opacity))
                }
            }
            .frame(width: geo.size.width, height: geo.size.height, alignment: hudOnLeft ? Alignment.leading : Alignment.trailing)
        }
    }

    /// Linguetta sul bordo: si apre toccando lo stato o trascinandola verso il centro.
    private var edgeTab: some View {
        VStack(spacing: 2) {
            Button {
                setPanel(open: true)
            } label: {
                VStack(spacing: 3) {
                    Image(systemName: hudOnLeft ? "chevron.compact.right" : "chevron.compact.left")
                        .font(.body.weight(.semibold))
                    Circle()
                        .fill(session.metrics.connected ? PhantomPalette.liveGreen : PhantomPalette.warnAmber)
                        .frame(width: 7, height: 7)
                    if session.metrics.connected {
                        Text(String(format: "%.0f", session.metrics.fps))
                            .font(.caption2.monospacedDigit().weight(.semibold))
                            .lineLimit(1)
                            .minimumScaleFactor(0.7)
                    }
                }
                .frame(width: 40, height: 58)
                .contentShape(Rectangle())
            }
            .accessibilityLabel("Apri i controlli")

            Rectangle()
                .fill(.white.opacity(0.22))
                .frame(width: 18, height: 1)
                .padding(.vertical, 3)

            tabNavButton(.back, systemImage: "chevron.backward", label: "Indietro")
            tabNavButton(.home, systemImage: "circle", label: "Home")
            tabNavButton(.recents, systemImage: "square", label: "Recenti")
        }
        .foregroundStyle(.white)
        .padding(.vertical, 6)
        .phantomGlassCard(cornerRadius: 22, overVideo: true)
        .padding(hudOnLeft ? Edge.Set.leading : Edge.Set.trailing, 4)
        .simultaneousGesture(
            DragGesture(minimumDistance: 12)
                .onEnded { value in
                    if towardCenter(value.translation.width) > 24 {
                        setPanel(open: true)
                    }
                }
        )
    }

    private func tabNavButton(_ action: RemoteQuickAction, systemImage: String, label: String) -> some View {
        Button {
            pillHaptic.impactOccurred(intensity: 0.6)
            session.sendQuickAction(action)
        } label: {
            Image(systemName: systemImage)
                .font(.footnote.weight(.bold))
                .frame(width: 40, height: 40)
                .contentShape(Rectangle())
        }
        .disabled(!session.metrics.connected)
        .opacity(session.metrics.connected ? 1 : 0.45)
        .accessibilityLabel(label)
    }

    /// Pannello laterale: riepilogo, tasti in griglia, telemetria, lato della HUD e chiusura della sessione.
    private func sidePanel(maxHeight: CGFloat, width: CGFloat) -> some View {
        ViewThatFits(in: .vertical) {
            panelContent
            ScrollView(showsIndicators: false) { panelContent }
        }
        .frame(width: width)
        // In verticale il contenuto ci sta e il pannello è alto quanto serve; in orizzontale diventa scorrevole.
        .frame(maxHeight: maxHeight)
        .phantomGlassCard(cornerRadius: 26, overVideo: true)
        .padding(hudOnLeft ? Edge.Set.leading : Edge.Set.trailing, 8)
    }

    private var panelContent: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 9) {
                Circle()
                    .fill(session.metrics.connected ? PhantomPalette.liveGreen : PhantomPalette.warnAmber)
                    .frame(width: 8, height: 8)
                Text(hudSummaryLine)
                    .font(.caption.monospacedDigit().weight(.medium))
                    .lineLimit(1)
                    .minimumScaleFactor(0.75)
                Spacer(minLength: 4)
                Button {
                    setPanel(open: false)
                } label: {
                    Image(systemName: hudOnLeft ? "chevron.left" : "chevron.right")
                        .font(.footnote.weight(.bold))
                        .frame(width: 34, height: 34)
                        .background(.white.opacity(0.14), in: Circle())
                }
                .accessibilityLabel("Chiudi i controlli")
            }

            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 3), spacing: 8) {
                quickButton("Indietro", systemImage: "chevron.backward", action: .back)
                quickButton("Home", systemImage: "circle", action: .home)
                quickButton("Recenti", systemImage: "square.on.square", action: .recents)
                quickButton("Vol−", systemImage: "speaker.wave.1", action: .volumeDown)
                quickButton("Vol+", systemImage: "speaker.wave.3", action: .volumeUp)
                controlButton(
                    session.audioMuted ? "Audio off" : "Audio on",
                    systemImage: session.audioMuted ? "speaker.slash.fill" : "speaker.wave.2.fill"
                ) {
                    session.toggleAudioMute()
                }
                if session.systemKeysSupported {
                    quickButton("Notifiche", systemImage: "bell", action: .notifications)
                    quickButton("Screenshot", systemImage: "camera.viewfinder", action: .screenshot)
                    quickButton("Blocca", systemImage: "lock", action: .power)
                }
                controlButton("Tastiera", systemImage: keyboardVisible ? "keyboard.chevron.compact.down" : "keyboard") {
                    resetTouchesToken &+= 1
                    toggleKeyboardToken &+= 1
                    setPanel(open: false)
                }
                controlButton("Appunti", systemImage: "doc.on.clipboard") {
                    session.sendClipboardFromPasteboard()
                }
                controlButton(
                    session.hostScreenOff ? "Schermo On" : "Schermo Off",
                    systemImage: session.hostScreenOff ? "sun.max.fill" : "moon.fill"
                ) {
                    session.sendControl(session.hostScreenOff ? .screenOn : .screenOff)
                }
                controlButton("Risveglia", systemImage: "power") {
                    session.sendControl(.wakeScreen)
                }
            }

            if case .localQuicLoopback = mode {
                Text("Loopback · tocchi \(session.metrics.hostTouchReceived) · tasti \(session.metrics.hostKeyReceived)")
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.white.opacity(0.7))
            } else if session.metrics.connected {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(Array(hudTelemetryLines.enumerated()), id: \.offset) { _, line in
                        Text(line)
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(.white.opacity(0.72))
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            HStack(spacing: 8) {
                controlButton(hudOnLeft ? "HUD a destra" : "HUD a sinistra",
                              systemImage: hudOnLeft ? "arrow.right.to.line" : "arrow.left.to.line") {
                    withAnimation(hudAnimation) { hudEdgeRaw = hudOnLeft ? "trailing" : "leading" }
                }
                Button(role: .destructive) {
                    end()
                    dismiss()
                } label: {
                    VStack(spacing: 4) {
                        Image(systemName: "xmark")
                            .font(.footnote.weight(.semibold))
                        Text("Termina")
                            .font(.caption2.weight(.medium))
                    }
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 7)
                    .background(Color.red.opacity(0.42), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                }
                .accessibilityLabel("Termina la sessione")
            }
        }
        .foregroundStyle(.white)
        .padding(14)
    }

    private var hudSummaryLine: String {
        let m = session.metrics
        guard m.connected else { return session.statusText }
        let rtt = m.rttMs.map { String(format: "%.0f ms", $0) } ?? "–"
        let prefix = m.endpointLabel.isEmpty ? "" : "\(m.endpointLabel) · "
        return String(format: "%@%.0f fps · %@", prefix, m.fps, rtt)
    }

    /// Telemetria dell'HUD espanso, una riga per argomento: video, latenza, rete, audio e host.
    private var hudTelemetryLines: [String] {
        let m = session.metrics
        var lines: [String] = []

        // Video: risoluzione e codec, fps dell'encoder (dal rapporto dell'host) e fps ricevuti
        var video: [String] = []
        if m.videoWidth > 0 && m.videoHeight > 0 {
            video.append("\(m.videoWidth)×\(m.videoHeight)")
        }
        if !m.codecName.isEmpty {
            video.append(m.codecName)
        }
        if let h = m.host {
            video.append(String(format: "encoder %.0f fps", h.encoderFps))
        }
        video.append(String(format: "ricevuti %.0f fps", m.fps))
        lines.append(video.joined(separator: " · "))

        // Latenza: cattura → decoder (mediana e p95), tempo di codifica, RTT, coda
        var latency: [String] = []
        if let p50 = m.latencyP50Ms, let p95 = m.latencyP95Ms {
            latency.append(String(format: "latenza %.0f ms (p95 %.0f)", p50, p95))
        } else {
            latency.append("latenza –")
        }
        if let h = m.host, h.encodeLatencyUs > 0 {
            latency.append(String(format: "codifica %.0f ms", Double(h.encodeLatencyUs) / 1000.0))
        }
        if let rtt = m.rttMs {
            latency.append(String(format: "RTT %.0f ms", rtt))
        }
        if let q = m.queueDelayMs {
            latency.append(String(format: "coda %.0f ms", q))
        }
        lines.append(latency.joined(separator: " · "))

        // Rete: bitrate ricevuto e obiettivo, coda di invio dell'host, perdite e keyframe
        var net: [String] = []
        if let h = m.host, h.targetKbps > 0 {
            net.append(String(format: "%.1f/%.0f Mbps", m.bitrateMbps, Double(h.targetKbps) / 1000.0))
        } else {
            net.append(String(format: "%.1f Mbps", m.bitrateMbps))
        }
        net.append("persi \(m.framesLost)")
        if let h = m.host {
            net.append("coda host \(h.sendQueueBytes / 1024) KB")
            if h.queueOverflows > 0 || h.sendErrors > 0 {
                net.append("scartati \(h.queueOverflows + h.sendErrors)")
            }
            if h.framesSkipped > 0 {
                // frame non spediti per svuotare la coda: la rete non reggeva il bitrate
                net.append("saltati \(h.framesSkipped)")
            }
            net.append("keyframe \(h.keyframes)")
        } else {
            net.append("richieste keyframe \(m.keyframeRequests)")
        }
        if m.decoderDrops > 0 {
            net.append("decoder \(m.decoderDrops)")
        }
        lines.append(net.joined(separator: " · "))

        // Audio e stato termico dell'host
        var tail: [String] = []
        if let a = m.audio {
            tail.append("audio \(a.delayMs) ms · buchi \(a.skipped) · ritardi \(a.late) · resync \(a.resyncs)")
        } else {
            tail.append("audio –")
        }
        if let h = m.host {
            let thermal = ["normale", "tiepido", "caldo", "critico"]
            let level = Int(h.thermalLevel)
            tail.append("termico " + (level < thermal.count ? thermal[level] : "?"))
        }
        lines.append(tail.joined(separator: " · "))
        return lines
    }

    private func quickButton(_ title: String, systemImage: String, action: RemoteQuickAction) -> some View {
        controlButton(title, systemImage: systemImage) {
            session.sendQuickAction(action)
        }
    }

    private func controlButton(_ title: String, systemImage: String, onTap: @escaping () -> Void) -> some View {
        Button(action: onTap) {
            VStack(spacing: 4) {
                Image(systemName: systemImage)
                    .font(.footnote.weight(.semibold))
                Text(title)
                    .font(.caption2.weight(.medium))
            }
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 7)
            .background(.white.opacity(0.12), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(.white.opacity(0.14), lineWidth: 0.5)
            }
        }
    }
}

/// Collega controller fisici MFi/Bluetooth (D-pad e tasti principali) a eventi HID di Phantom.
private final class GamepadBridge {
    private var connectObserver: NSObjectProtocol?
    private var attached: [GCController] = []
    private var handler: ((UInt16, Bool) -> Void)?

    func start(onKey: @escaping (UInt16, Bool) -> Void) {
        handler = onKey
        for controller in GCController.controllers() { attach(controller) }
        connectObserver = NotificationCenter.default.addObserver(
            forName: .GCControllerDidConnect,
            object: nil,
            queue: .main
        ) { [weak self] note in
            if let controller = note.object as? GCController {
                self?.attach(controller)
            }
        }
    }

    func stop() {
        if let obs = connectObserver {
            NotificationCenter.default.removeObserver(obs)
            connectObserver = nil
        }
        for controller in attached {
            detach(controller)
        }
        attached.removeAll()
        handler = nil
    }

    private func attach(_ controller: GCController) {
        guard !attached.contains(where: { $0 === controller }), let pad = controller.extendedGamepad else { return }
        attached.append(controller)
        bind(pad.dpad.up, code: HidKeyCode.upArrow)
        bind(pad.dpad.down, code: HidKeyCode.downArrow)
        bind(pad.dpad.left, code: HidKeyCode.leftArrow)
        bind(pad.dpad.right, code: HidKeyCode.rightArrow)
        bind(pad.buttonA, code: HidKeyCode.enter)
        bind(pad.buttonB, code: HidKeyCode.escape)
        bind(pad.buttonX, code: HidKeyCode.space)
        bind(pad.buttonY, code: HidKeyCode.tab)
        bind(pad.buttonMenu, code: HidKeyCode.home)
    }

    private func detach(_ controller: GCController) {
        guard let pad = controller.extendedGamepad else { return }
        pad.dpad.up.pressedChangedHandler = nil
        pad.dpad.down.pressedChangedHandler = nil
        pad.dpad.left.pressedChangedHandler = nil
        pad.dpad.right.pressedChangedHandler = nil
        pad.buttonA.pressedChangedHandler = nil
        pad.buttonB.pressedChangedHandler = nil
        pad.buttonX.pressedChangedHandler = nil
        pad.buttonY.pressedChangedHandler = nil
        pad.buttonMenu.pressedChangedHandler = nil
    }

    private func bind(_ button: GCControllerButtonInput, code: UInt16) {
        button.pressedChangedHandler = { [weak self] _, _, pressed in
            self?.handler?(code, pressed)
        }
    }
}
