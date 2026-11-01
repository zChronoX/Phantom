import SwiftUI
import UIKit

/// Schermata della prova «tutto sul telefono» del core Rust.
struct CoreLoopbackScreen: View {
    @StateObject private var engine = CoreLoopback()
    @State private var fps = 120
    @State private var loss = 2.0
    @State private var seconds = 20
    @State private var copied = false

    var body: some View {
        Form {
            Section("Impostazioni") {
                Picker("Fotogrammi al secondo", selection: $fps) {
                    Text("60").tag(60)
                    Text("120").tag(120)
                }
                Picker("Perdita simulata", selection: $loss) {
                    Text("Nessuna").tag(0.0)
                    Text("2%").tag(2.0)
                    Text("5%").tag(5.0)
                    Text("10%").tag(10.0)
                }
                Picker("Durata", selection: $seconds) {
                    Text("10 s").tag(10)
                    Text("20 s").tag(20)
                    Text("30 s").tag(30)
                }
            }
            .disabled(engine.running)

            Section {
                Button(engine.running ? "Ferma" : "Avvia la prova") {
                    if engine.running {
                        engine.stop()
                    } else {
                        copied = false
                        engine.start(LoopbackSettings(fps: fps, lossPercent: loss, seconds: seconds))
                    }
                }
                .frame(maxWidth: .infinity)
                .fontWeight(.semibold)
                if !engine.progress.isEmpty {
                    Text(engine.progress)
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                LoopbackPreview(engine: engine)
                    .frame(height: 360)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }

            if !engine.report.isEmpty {
                Section("Risultati") {
                    Text(engine.report)
                        .font(.system(size: 11, design: .monospaced))
                        .textSelection(.enabled)
                    Button(copied ? "Copiato" : "Copia risultati") {
                        UIPasteboard.general.string = engine.report
                        copied = true
                    }
                }
            }

            Section {
                Text("L'iPhone fa da solo tutto il percorso del video: crea frame sintetici, li codifica in HEVC con l'encoder hardware, li passa al core Rust di Phantom (frammenti, parità, ritrasmissioni, controllo del bitrate), perde una parte dei pacchetti a caso, li riceve con il client del core, li decodifica e li mostra. Intanto un dito finto manda lo stato del tocco a 240 Hz. Non usa la rete né il POCO. Tieni l'app aperta e lo schermo acceso fino alla fine.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Core Rust sull'iPhone")
        .navigationBarTitleDisplayMode(.inline)
        .onDisappear { engine.stop() }
    }
}

/// Mostra i frame che escono dal client del core.
private struct LoopbackPreview: UIViewRepresentable {
    let engine: CoreLoopback

    func makeUIView(context: Context) -> SampleLayerView {
        let view = SampleLayerView()
        engine.onSample = { [weak view] sb in
            DispatchQueue.main.async { view?.enqueue(sb) }
        }
        return view
    }

    func updateUIView(_ uiView: SampleLayerView, context: Context) {}
}
