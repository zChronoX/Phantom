import SwiftUI
import UIKit

struct ContentView: View {
    @StateObject private var tester = PingTester()
    @AppStorage("host") private var host = "192.168.1.118"
    @AppStorage("seconds") private var seconds = 120.0
    @AppStorage("size") private var size = 64
    @AppStorage("voice") private var voice = true
    @AppStorage("videoMode") private var videoMode = VideoMode.scrcpy.rawValue
    @State private var copied = false
    @State private var remoteMessage = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Telefono Android") {
                    TextField("Indirizzo IP del POCO", text: $host)
                        .keyboardType(.numbersAndPunctuation)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Picker("Durata", selection: $seconds) {
                        Text("30 s").tag(30.0)
                        Text("2 min").tag(120.0)
                    }
                    Picker("Pacchetto", selection: $size) {
                        Text("64 byte").tag(64)
                        Text("1.200 byte (come il video)").tag(1200)
                    }
                    Toggle("Priorità voce Wi-Fi (WMM)", isOn: $voice)
                }
                .disabled(tester.running)

                Section {
                    Button(tester.running ? "Ferma il test" : "Avvia il test") {
                        if tester.running {
                            tester.stop()
                        } else {
                            copied = false
                            tester.start(.init(host: host, seconds: seconds, intervalMs: 10, size: size, voice: voice))
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .fontWeight(.semibold)
                    Text(tester.status)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }

                Section("RTT (ms)") {
                    StatsGrid(stats: tester.stats)
                    RttChart(values: tester.recent)
                        .frame(height: 120)
                }

                if !tester.report.isEmpty {
                    Section("Risultati") {
                        Text(tester.report)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                        Button(copied ? "Copiato" : "Copia risultati") {
                            UIPasteboard.general.string = tester.report
                            copied = true
                        }
                    }
                }

                Section("Video dal vivo") {
                    Picker("Sorgente", selection: $videoMode) {
                        Text("scrcpy (bassa latenza)").tag(VideoMode.scrcpy.rawValue)
                        Text("Base (screenrecord)").tag(VideoMode.basic.rawValue)
                    }
                    NavigationLink {
                        VideoScreen(host: host, mode: VideoMode(rawValue: videoMode) ?? .scrcpy)
                    } label: {
                        Label("Guarda lo schermo del POCO", systemImage: "play.rectangle")
                    }
                    Text("Prima accendi il video nella WebUI del modulo sul POCO: un profilo della scala scrcpy per la bassa latenza, «Accendi video» per la sorgente base. Mostra lo schermo di Android dal vivo, con frame rate e banda.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button("Ferma la sessione video sul POCO") {
                        remoteMessage = "Richiesta inviata…"
                        RemoteControl.stopSessions(host: host) { remoteMessage = $0 }
                    }
                    if !remoteMessage.isEmpty {
                        Text(remoteMessage)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }

                Section {
                    Text("Ogni 10 ms l'iPhone manda un pacchetto UDP alla sonda sul POCO, che lo rimanda indietro. Prova con e senza priorità voce, vicino e lontano dall'access point. Per test più puliti disattiva AirDrop (Ricezione disattivata).")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Phantom F0")
        }
    }
}

private struct StatsGrid: View {
    let stats: PingStats

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 10) {
            GridRow {
                cell("Mediano", stats.p50)
                cell("p95", stats.p95)
                cell("p99", stats.p99)
            }
            GridRow {
                cell("Jitter", stats.jitter)
                VStack(alignment: .leading) {
                    Text("Picchi > 50").font(.caption).foregroundStyle(.secondary)
                    Text("\(stats.spikes)").font(.title3.monospacedDigit()).fontWeight(.semibold)
                }
                VStack(alignment: .leading) {
                    Text("Persi").font(.caption).foregroundStyle(.secondary)
                    Text(String(format: "%.1f%%", stats.lossPercent)).font(.title3.monospacedDigit()).fontWeight(.semibold)
                }
            }
        }
        .padding(.vertical, 4)
    }

    private func cell(_ title: String, _ value: Double) -> some View {
        VStack(alignment: .leading) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value.isNaN ? "–" : String(format: "%.1f", value))
                .font(.title3.monospacedDigit())
                .fontWeight(.semibold)
        }
    }
}

/// Ultimi 300 RTT (3 secondi a 10 ms), con linee guida a 10, 20 e 50 ms.
private struct RttChart: View {
    let values: [Double]

    var body: some View {
        Canvas { ctx, size in
            let top = max(20, values.max() ?? 20) * 1.1
            for guide in [10.0, 20.0, 50.0] where guide < top {
                let y = size.height - size.height * CGFloat(guide / top)
                var line = Path()
                line.move(to: CGPoint(x: 0, y: y))
                line.addLine(to: CGPoint(x: size.width, y: y))
                ctx.stroke(line, with: .color(.secondary.opacity(0.3)), lineWidth: 0.5)
                ctx.draw(Text("\(Int(guide))").font(.caption2).foregroundStyle(.secondary), at: CGPoint(x: 10, y: y - 7))
            }
            guard values.count > 1 else { return }
            var path = Path()
            for (i, v) in values.enumerated() {
                let x = size.width * CGFloat(i) / CGFloat(values.count - 1)
                let y = size.height - size.height * CGFloat(min(v, top) / top)
                if i == 0 { path.move(to: CGPoint(x: x, y: y)) } else { path.addLine(to: CGPoint(x: x, y: y)) }
            }
            ctx.stroke(path, with: .color(.accentColor), lineWidth: 1.5)
        }
    }
}
