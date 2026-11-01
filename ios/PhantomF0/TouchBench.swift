import SwiftUI
import UIKit

/// Misure del tocco e dello schermo dell'iPhone, senza il POCO.
///
/// Servono a decidere come mandare il tocco ad Android per i giochi: quanti campioni al secondo dà l'iPhone, con quale
/// regolarità, quanto ritardo c'è tra il momento fisico del tocco e la consegna all'app, e a che frequenza si aggiorna
/// davvero lo schermo (ProMotion a 120 Hz richiede una voce nell'Info.plist dell'app).
struct TouchStats: Equatable {
    var seconds = 0.0
    var callbacks = 0
    var samples = 0
    var coalescedMax = 0
    var coalesced = BenchSummary()
    var interval = BenchSummary()     // ms tra campioni consecutivi dello stesso dito
    var delivery = BenchSummary()     // ms tra il tocco fisico e la consegna all'app
    var frame = BenchSummary()        // ms tra due aggiornamenti dello schermo (CADisplayLink)
    var maxFps = 0

    var sampleRate: Double { seconds > 0 ? Double(samples) / seconds : 0 }
    var callbackRate: Double { seconds > 0 ? Double(callbacks) / seconds : 0 }
    var displayRate: Double { frame.p50 > 0 ? 1000 / frame.p50 : 0 }

    var text: String {
        var t = "Phantom F0 · banco di prova di tocco e schermo (iPhone)\n"
        t += String(format: "durata %.1f s · schermo fino a %d Hz\n", seconds, maxFps)
        t += String(format: "aggiornamento reale dello schermo: %.0f Hz · intervallo %@\n", displayRate, frame.text)
        t += String(format: "campioni di tocco: %d (%.0f al secondo) in %d consegne (%.0f al secondo)\n", samples, sampleRate, callbacks, callbackRate)
        t += "campioni per consegna (coalesced): p50 \(String(format: "%.0f", coalesced.p50)) · max \(coalescedMax)\n"
        t += "intervallo tra campioni dello stesso dito: \(interval.text)\n"
        t += "ritardo di consegna dal tocco fisico: \(delivery.text)\n"
        return t
    }
}

final class TouchSurface: UIView {
    var onUpdate: ((TouchStats) -> Void)?

    private var callbacks = 0
    private var samples = 0
    private var coalescedMax = 0
    private var coalescedCounts: [Double] = []
    private var intervals: [Double] = []
    private var delays: [Double] = []
    private var frames: [Double] = []
    private var lastSample: [ObjectIdentifier: Double] = [:]
    private var firstEvent: Double?
    private var lastEvent: Double?
    private var link: CADisplayLink?
    private var lastFrame: Double?
    private var timer: Timer?
    private var dots: [ObjectIdentifier: CAShapeLayer] = [:]

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = UIColor(white: 0.08, alpha: 1)
    }

    required init?(coder: NSCoder) { fatalError("non supportato") }

    func reset() {
        callbacks = 0; samples = 0; coalescedMax = 0
        coalescedCounts.removeAll(); intervals.removeAll(); delays.removeAll(); frames.removeAll()
        lastSample.removeAll(); firstEvent = nil; lastEvent = nil; lastFrame = nil
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil {
            let l = CADisplayLink(target: self, selector: #selector(tick(_:)))
            l.preferredFrameRateRange = CAFrameRateRange(minimum: 60, maximum: 120, preferred: 120)
            l.add(to: .main, forMode: .common)
            link = l
            timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.publish() }
        } else {
            link?.invalidate(); link = nil
            timer?.invalidate(); timer = nil
        }
    }

    @objc private func tick(_ l: CADisplayLink) {
        let ts = l.timestamp
        if let last = lastFrame {
            let d = (ts - last) * 1000
            if d < 100 { frames.append(d) }
        }
        lastFrame = ts
    }

    private func process(_ touches: Set<UITouch>, _ event: UIEvent?, ended: Bool) {
        let now = ProcessInfo.processInfo.systemUptime
        callbacks += 1
        if firstEvent == nil { firstEvent = now }
        lastEvent = now
        for t in touches {
            let id = ObjectIdentifier(t)
            let list = event?.coalescedTouches(for: t) ?? [t]
            coalescedCounts.append(Double(list.count))
            coalescedMax = max(coalescedMax, list.count)
            if let first = list.first { delays.append((now - first.timestamp) * 1000) }
            for c in list {
                samples += 1
                if let prev = lastSample[id] {
                    let d = (c.timestamp - prev) * 1000
                    if d > 0 && d < 100 { intervals.append(d) }
                }
                lastSample[id] = c.timestamp
            }
            moveDot(id, to: t.location(in: self), visible: !ended)
            if ended { lastSample[id] = nil }
        }
    }

    private func moveDot(_ id: ObjectIdentifier, to p: CGPoint, visible: Bool) {
        if !visible {
            dots[id]?.removeFromSuperlayer()
            dots[id] = nil
            return
        }
        let layer: CAShapeLayer
        if let l = dots[id] {
            layer = l
        } else {
            layer = CAShapeLayer()
            layer.path = UIBezierPath(ovalIn: CGRect(x: -22, y: -22, width: 44, height: 44)).cgPath
            layer.fillColor = UIColor.systemTeal.withAlphaComponent(0.55).cgColor
            self.layer.addSublayer(layer)
            dots[id] = layer
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.position = p
        CATransaction.commit()
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { process(touches, event, ended: false) }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { process(touches, event, ended: false) }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { process(touches, event, ended: true) }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { process(touches, event, ended: true) }

    private func publish() {
        var s = TouchStats()
        if let a = firstEvent, let b = lastEvent { s.seconds = max(b - a, 0.001) }
        s.callbacks = callbacks
        s.samples = samples
        s.coalescedMax = coalescedMax
        s.coalesced = BenchSummary(coalescedCounts)
        s.interval = BenchSummary(intervals)
        s.delivery = BenchSummary(delays)
        s.frame = BenchSummary(frames)
        s.maxFps = window?.screen.maximumFramesPerSecond ?? UIScreen.main.maximumFramesPerSecond
        onUpdate?(s)
    }
}

struct TouchSurfaceView: UIViewRepresentable {
    @Binding var stats: TouchStats
    let resetToken: Int

    func makeUIView(context: Context) -> TouchSurface {
        let v = TouchSurface()
        v.onUpdate = { s in DispatchQueue.main.async { stats = s } }
        return v
    }

    func updateUIView(_ uiView: TouchSurface, context: Context) {
        if context.coordinator.lastToken != resetToken {
            context.coordinator.lastToken = resetToken
            uiView.reset()
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }
    final class Coordinator { var lastToken = 0 }
}

struct TouchBenchScreen: View {
    @State private var stats = TouchStats()
    @State private var resetToken = 0
    @State private var copied = false

    var body: some View {
        ZStack(alignment: .top) {
            TouchSurfaceView(stats: $stats, resetToken: resetToken).ignoresSafeArea()
            VStack(alignment: .leading, spacing: 6) {
                Text("Muovi 1–5 dita sullo schermo per 10 secondi: scorri, disegna cerchi, tocca veloce. Poi tocca «Copia risultati».")
                    .font(.footnote).foregroundStyle(.white.opacity(0.8))
                Text(stats.text)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.white)
                HStack {
                    Button("Azzera") { resetToken += 1; stats = TouchStats(); copied = false }
                    Spacer()
                    Button(copied ? "Copiato" : "Copia risultati") {
                        UIPasteboard.general.string = stats.text
                        copied = true
                    }
                }
                .buttonStyle(.borderedProminent)
            }
            .padding()
            .background(.black.opacity(0.55))
        }
        .navigationTitle("Tocco e schermo")
        .navigationBarTitleDisplayMode(.inline)
    }
}
