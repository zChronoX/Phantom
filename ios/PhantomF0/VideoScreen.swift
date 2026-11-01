import SwiftUI
import AVFoundation
import CoreMedia

/// UIView il cui layer è un AVSampleBufferDisplayLayer: decodifica e mostra i CMSampleBuffer.
final class SampleLayerView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }
    var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }

    override init(frame: CGRect) {
        super.init(frame: frame)
        displayLayer.videoGravity = .resizeAspect
        backgroundColor = .black
    }
    required init?(coder: NSCoder) { fatalError("non supportato") }

    func enqueue(_ sb: CMSampleBuffer) {
        if displayLayer.status == .failed { displayLayer.flush() }
        if displayLayer.isReadyForMoreMediaData { displayLayer.enqueue(sb) }
    }
}

struct VideoLayer: UIViewRepresentable {
    let client: VideoSource
    func makeUIView(context: Context) -> SampleLayerView {
        let view = SampleLayerView()
        client.onSample = { [weak view] sb in
            DispatchQueue.main.async { view?.enqueue(sb) }
        }
        return view
    }
    func updateUIView(_ uiView: SampleLayerView, context: Context) {}
}

/// Tiene insieme il client video e lo stato osservabile dall'interfaccia.
enum VideoMode: String {
    case scrcpy, basic
}

final class VideoModel: ObservableObject {
    @Published var stats = VideoStats()
    @Published var status = "Pronto"
    let client: VideoSource

    init(mode: VideoMode) {
        client = mode == .scrcpy ? ScrcpyClient() : VideoClient()
        client.onStats = { [weak self] s in self?.stats = s }
        client.onStatus = { [weak self] t in self?.status = t }
    }

    func start(host: String) { client.start(host: host) }
    func stop() { client.stop() }
}

struct VideoScreen: View {
    let host: String
    let mode: VideoMode
    @StateObject private var model: VideoModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var blackbox = TelemetryRecorder.shared
    @State private var showOverlay = true
    /// Impedisce una seconda connessione se SwiftUI chiama onAppear più di una volta.
    @State private var running = false

    init(host: String, mode: VideoMode) {
        self.host = host
        self.mode = mode
        _model = StateObject(wrappedValue: VideoModel(mode: mode))
    }

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            VideoLayer(client: model.client).ignoresSafeArea()

            if showOverlay {
                overlay
                    .padding()
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.black.opacity(0.35))
                    .transition(.opacity)
            }
        }
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .statusBarHidden()
        .onTapGesture { withAnimation { showOverlay.toggle() } }
        .onAppear { begin() }
        .onDisappear { end() }
        // Quando l'app va in background iOS la sospende e smette di leggere: il flusso va chiuso subito,
        // altrimenti i buffer del POCO si riempiono e il server può bloccare lo schermo del telefono.
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { end() } else if phase == .active { begin() }
        }
    }

    private func begin() {
        guard !running else { return }
        running = true
        UIApplication.shared.isIdleTimerDisabled = true
        // La scatola nera si accende con il video e NON si ferma quando si chiude la schermata: se il POCO si blocca,
        // continua a registrare il silenzio e il ritorno.
        blackbox.startIfNeeded(host: host)
        model.start(host: host)
    }

    private func end() {
        guard running else { return }
        running = false
        UIApplication.shared.isIdleTimerDisabled = false
        model.stop()
    }

    private func reconnect() {
        end()
        begin()
    }

    private var overlay: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button { dismiss() } label: {
                    Label("Chiudi", systemImage: "chevron.left")
                }
                Spacer()
                if !model.stats.connected {
                    Button { reconnect() } label: {
                        Label("Riconnetti", systemImage: "arrow.clockwise")
                    }
                }
                Circle()
                    .fill(model.stats.connected ? .green : .orange)
                    .frame(width: 10, height: 10)
            }
            .foregroundStyle(.white)

            if model.stats.frames == 0 {
                Text(model.status).foregroundStyle(.white.opacity(0.9)).font(.footnote)
            } else {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 4) {
                    GridRow {
                        stat("FPS", String(format: "%.0f", model.stats.fps))
                        stat("Risoluzione", "\(model.stats.width)×\(model.stats.height)")
                        stat("Banda", String(format: "%.1f Mbps", model.stats.kbps / 1000))
                    }
                    GridRow {
                        stat("Frame", "\(model.stats.frames)")
                        stat("Intervallo medio", String(format: "%.1f ms", model.stats.meanGapMs))
                        stat("Pausa max", String(format: "%.0f ms", model.stats.maxGapMs))
                    }
                    if mode == .scrcpy {
                        GridRow {
                            stat("Codec", model.stats.codec)
                            stat("Ritardo extra p50", String(format: "%.1f ms", model.stats.excessP50Ms))
                            stat("Ritardo extra p95", String(format: "%.1f ms", model.stats.excessP95Ms))
                        }
                    }
                }
            }
            Text(blackbox.badge)
                .font(.caption2.monospacedDigit().weight(.semibold))
                .foregroundStyle(blackbox.silentSeconds > 2 ? Color.red : Color.white.opacity(0.75))
            Text("Tocca lo schermo per mostrare o nascondere i dati. Se il POCO non risponde, chiudi questa schermata.")
                .font(.caption2).foregroundStyle(.white.opacity(0.6))
        }
    }

    private func stat(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).font(.caption2).foregroundStyle(.white.opacity(0.6))
            Text(value).font(.callout.monospacedDigit().weight(.semibold)).foregroundStyle(.white)
        }
    }
}
