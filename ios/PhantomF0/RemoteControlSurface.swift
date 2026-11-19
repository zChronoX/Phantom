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

    func enqueueOnBackground(_ sb: CMSampleBuffer, flushBeforeEnqueue: Bool) {
        guard let renderer = videoRenderer else { return }
        renderQueue.async {
            if flushBeforeEnqueue || renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
                renderer.flush()
            }
            if renderer.isReadyForMoreMediaData {
                renderer.enqueue(sb)
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
            ring.fillColor = UIColor.systemTeal.withAlphaComponent(0.25).cgColor
            ring.strokeColor = UIColor.systemTeal.withAlphaComponent(0.85).cgColor
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

    @State private var hudExpanded = true
    @State private var hudHidden = false
    @State private var keyboardVisible = false
    @State private var toggleKeyboardToken = 0
    @State private var resetTouchesToken = 0
    @State private var started = false
    @State private var controllerBridge = GamepadBridge()

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

            if hudHidden {
                collapsedHandle
            } else {
                hudBar
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
                resetTouchesToken &+= 1
                end()
            } else if phase == .active && !started {
                begin()
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
        VStack(spacing: 10) {
            if session.running {
                ProgressView()
                    .tint(.white)
            } else {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.title3)
                    .foregroundStyle(.orange)
            }
            Text(session.statusText)
                .font(.footnote)
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
            if !session.running {
                Button("Riconnetti") { reconnect() }
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(16)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14))
        .padding(.horizontal, 24)
    }

    private var collapsedHandle: some View {
        HStack {
            Spacer()
            Button {
                withAnimation(.easeInOut(duration: 0.18)) { hudHidden = false }
            } label: {
                HStack(spacing: 6) {
                    Circle()
                        .fill(session.metrics.connected ? .green : .orange)
                        .frame(width: 8, height: 8)
                    Image(systemName: "slider.horizontal.3")
                        .font(.caption.weight(.semibold))
                }
                .foregroundStyle(.white)
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(.black.opacity(0.55), in: Capsule())
            }
            .padding(.top, 8)
            .padding(.trailing, 12)
        }
    }

    private var hudBar: some View {
        VStack(spacing: 8) {
            HStack(spacing: 10) {
                Button {
                    end()
                    dismiss()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.title3)
                }

                Circle()
                    .fill(session.metrics.connected ? .green : .orange)
                    .frame(width: 9, height: 9)

                Text(hudSummaryLine)
                    .font(.caption.monospacedDigit())
                    .lineLimit(1)

                Spacer()

                Button {
                    resetTouchesToken &+= 1
                    toggleKeyboardToken &+= 1
                } label: {
                    Image(systemName: keyboardVisible ? "keyboard.chevron.compact.down" : "keyboard")
                }

                Button {
                    withAnimation(.easeInOut(duration: 0.18)) { hudExpanded.toggle() }
                } label: {
                    Image(systemName: hudExpanded ? "chevron.up" : "chevron.down")
                }

                Button {
                    withAnimation(.easeInOut(duration: 0.18)) { hudHidden = true }
                } label: {
                    Image(systemName: "eye.slash")
                }
            }
            .foregroundStyle(.white)

            if hudExpanded {
                HStack(spacing: 12) {
                    quickButton("Indietro", systemImage: "chevron.backward", action: .back)
                    quickButton("Home", systemImage: "circle", action: .home)
                    quickButton("Recenti", systemImage: "square.on.square", action: .recents)
                    quickButton("Vol−", systemImage: "speaker.wave.1", action: .volumeDown)
                    quickButton("Vol+", systemImage: "speaker.wave.3", action: .volumeUp)
                }

                if case .localQuicLoopback = mode {
                    Text("Loopback QUIC · tocchi all'host \(session.metrics.hostTouchReceived) · tasti \(session.metrics.hostKeyReceived)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.white.opacity(0.8))
                } else if !session.metrics.agreementSummary.isEmpty {
                    Text(session.metrics.agreementSummary)
                        .font(.caption2)
                        .foregroundStyle(.white.opacity(0.8))
                        .lineLimit(1)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.black.opacity(0.65), in: RoundedRectangle(cornerRadius: 16))
        .padding(.horizontal, 10)
        .padding(.top, 6)
    }

    private var hudSummaryLine: String {
        let m = session.metrics
        guard m.connected else { return session.statusText }
        let rtt = m.rttMs.map { String(format: "%.1f ms", $0) } ?? "–"
        let prefix = m.endpointLabel.isEmpty ? "" : "\(m.endpointLabel) · "
        return String(format: "%@%.0f fps · %.1f Mbps · RTT %@", prefix, m.fps, m.bitrateMbps, rtt)
    }

    private func quickButton(_ title: String, systemImage: String, action: RemoteQuickAction) -> some View {
        Button {
            session.sendKeyEvents(action.hidSequence)
        } label: {
            VStack(spacing: 3) {
                Image(systemName: systemImage)
                    .font(.footnote.weight(.semibold))
                Text(title)
                    .font(.caption2)
            }
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 5)
            .background(.white.opacity(0.14), in: RoundedRectangle(cornerRadius: 8))
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
