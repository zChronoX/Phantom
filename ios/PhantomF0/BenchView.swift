import SwiftUI
import UIKit

/// Schermata del banco di prova del decoder hardware.
struct DecoderBenchScreen: View {
    @StateObject private var bench = DecoderBench()
    @State private var copied = false

    var body: some View {
        Form {
            Section {
                Button(bench.running ? "In corso…" : "Avvia la prova (circa 30 secondi)") {
                    copied = false
                    bench.start()
                }
                .disabled(bench.running)
                .frame(maxWidth: .infinity)
                .fontWeight(.semibold)
                if !bench.progress.isEmpty {
                    Text(bench.progress).font(.footnote).foregroundStyle(.secondary)
                }
            }

            if !bench.report.isEmpty {
                Section("Risultati") {
                    Text(bench.report)
                        .font(.system(size: 11, design: .monospaced))
                        .textSelection(.enabled)
                    Button(copied ? "Copiato" : "Copia risultati") {
                        UIPasteboard.general.string = bench.report
                        copied = true
                    }
                }
            }

            Section {
                Text("L'iPhone crea frame sintetici a 1080×2400, li codifica con il suo encoder hardware a bassa latenza e li decodifica con lo stesso tipo di decoder che usa l'app per il video del POCO. Misura quanto costa la decodifica e quanti frame al secondo regge al massimo. Tieni l'app aperta e lo schermo acceso fino alla fine.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Decoder hardware")
        .navigationBarTitleDisplayMode(.inline)
    }
}
