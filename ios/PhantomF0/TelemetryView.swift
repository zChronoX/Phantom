import SwiftUI
import UIKit

/// Sezione della schermata principale per la scatola nera del POCO.
struct TelemetrySection: View {
    @ObservedObject private var recorder = TelemetryRecorder.shared
    let host: String
    @State private var copied = false

    var body: some View {
        Section("Scatola nera (stato del POCO in tempo reale)") {
            Button(recorder.running ? "Ferma la registrazione" : "Avvia la registrazione") {
                copied = false
                if recorder.running { recorder.stop() } else { recorder.start(host: host) }
            }
            .frame(maxWidth: .infinity)
            .fontWeight(.semibold)

            HStack {
                Circle().fill(color).frame(width: 10, height: 10)
                Text(statusLine).font(.footnote).foregroundStyle(.secondary)
            }

            if let s = recorder.summary {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(s.rows.enumerated()), id: \.offset) { _, row in
                        VStack(alignment: .leading, spacing: 0) {
                            Text(row.0).font(.caption2).foregroundStyle(.secondary)
                            Text(row.1)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundStyle(row.0 == "INCASTRATO" ? Color.red : Color.primary)
                        }
                    }
                }
            }

            if !recorder.recent.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Ultimi eventi (stalli, task bloccati, kernel)").font(.caption2).foregroundStyle(.secondary)
                    ForEach(Array(recorder.recent.enumerated()), id: \.offset) { _, line in
                        Text(line.prefix(220))
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(line.hasPrefix("S") ? Color.orange : Color.primary)
                    }
                }
            }

            if let url = recorder.fileURL {
                ShareLink(item: url) {
                    Label("Condividi il registro (file)", systemImage: "square.and.arrow.up")
                }
            }
            Button(copied ? "Copiato" : "Copia le ultime 200 righe") {
                UIPasteboard.general.string = recorder.recentText(200)
                copied = true
            }
            .disabled(recorder.lineCount == 0)

            Text("Accendila PRIMA di ogni prova di cattura video e lasciala accesa con lo schermo dell'iPhone acceso. Se il POCO si blocca, qui restano le ultime righe e il momento esatto in cui ha smesso di parlare: poi condividi il registro e mandamelo.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }

    private var color: Color {
        if !recorder.running { return .gray }
        if !recorder.connected { return .orange }
        if recorder.silentSeconds > 2 { return .red }
        return .green
    }

    private var statusLine: String {
        guard recorder.running else { return recorder.status }
        if !recorder.connected { return recorder.status }
        var t = "Collegata · \(recorder.lineCount) righe"
        if recorder.silentSeconds > 2 { t = String(format: "MUTA da %.0f s — il POCO potrebbe essersi bloccato", recorder.silentSeconds) }
        if recorder.stallCount > 0 { t += " · stalli rilevati: \(recorder.stallCount)" }
        return t
    }
}
