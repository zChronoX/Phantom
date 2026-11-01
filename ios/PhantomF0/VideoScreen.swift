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
    init(host: String, mode: VideoMode) {
        self.host = host
        self.mode = mode
        _model = StateObject(wrappedValue: VideoModel(mode: mode))
    }
    @Environment(\.dismiss) private var dismiss
    @State private var showOverlay = true

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
        .statusBarHidden()
        .onTapGesture { withAnimation { showOverlay.toggle() } }
        .onAppear { model.start(host: host) }
        .onDisappear { model.stop() }
    }

    private var overlay: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button { dismiss() } label: {
                    Label("Chiudi", systemImage: "chevron.left")
                }
                Spacer()
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
            Text("Tocca lo schermo per mostrare o nascondere i dati.")
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
