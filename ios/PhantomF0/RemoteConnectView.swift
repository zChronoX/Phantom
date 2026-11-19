import SwiftUI
import UIKit

/// Schermata di configurazione e avvio del controllo remoto QUIC (verso Android o in loopback locale).
struct RemoteConnectView: View {
    @AppStorage("remoteAddress") private var address = "192.168.1.118:47800"
    @AppStorage("remoteCert") private var certInput = ""
    @AppStorage("remoteCodec") private var codecRaw = "h265"
    @AppStorage("remoteShowTouch") private var showTouchIndicators = true
    @AppStorage("loopbackFps") private var loopbackFps = 120

    @State private var activeMode: ActiveRoute?
    @State private var errorMessage = ""

    private enum ActiveRoute: Identifiable {
        case remote(address: String, cert: Data, codec: VideoCodec)
        case loopback(fps: Int)

        var id: String {
            switch self {
            case let .remote(address, _, _): return "remote-\(address)"
            case let .loopback(fps): return "loopback-\(fps)"
            }
        }

        var sessionMode: RemoteSessionMode {
            switch self {
            case let .remote(address, cert, codec):
                return .remote(address: address, certificateDer: cert, codec: codec)
            case let .loopback(fps):
                return .localQuicLoopback(fps: fps, width: 1080, height: 2400)
            }
        }
    }

    var body: some View {
        Form {
            Section("Telefono Android (QUIC)") {
                TextField("IP:porta (es. 192.168.1.118:47800)", text: $address)
                    .keyboardType(.numbersAndPunctuation)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()

                TextField("Certificato DER dell'host (Hex o Base64)", text: $certInput, axis: .vertical)
                    .lineLimit(2...5)
                    .font(.system(.caption, design: .monospaced))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()

                HStack {
                    Button {
                        if let text = UIPasteboard.general.string {
                            certInput = text.trimmingCharacters(in: .whitespacesAndNewlines)
                        }
                    } label: {
                        Label("Incolla dagli appunti", systemImage: "doc.on.clipboard")
                    }
                    .font(.footnote)
                    Spacer()
                    if let status = certificateStatusText {
                        Text(status.text)
                            .font(.caption2)
                            .foregroundStyle(status.valid ? .green : .orange)
                    }
                }

                Picker("Codec preferito", selection: $codecRaw) {
                    Text("HEVC (H.265)").tag("h265")
                    Text("H.264 (AVC)").tag("h264")
                }

                Toggle("Mostra cerchi sul tocco", isOn: $showTouchIndicators)

                Button {
                    launchRemote()
                } label: {
                    Label("Avvia controllo remoto", systemImage: "play.display")
                        .frame(maxWidth: .infinity)
                        .fontWeight(.semibold)
                }

                if !errorMessage.isEmpty {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }

            Section("Prova sull'iPhone (senza il POCO)") {
                Picker("Frequenza loopback", selection: $loopbackFps) {
                    Text("60 fps").tag(60)
                    Text("120 fps").tag(120)
                }

                Button {
                    errorMessage = ""
                    activeMode = .loopback(fps: loopbackFps)
                } label: {
                    Label("Avvia in loopback QUIC locale", systemImage: "arrow.triangle.2.circlepath")
                        .frame(maxWidth: .infinity)
                }

                Text("Apre una vera connessione QUIC (TLS 1.3 con certificato fissato) sullo stesso iPhone, codifica in HEVC hardware, riceve il video e invia davvero tocchi multi-finger, tastiera e azioni Android all'host.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Controllo remoto (QUIC)")
        .navigationBarTitleDisplayMode(.inline)
        .fullScreenCover(item: $activeMode) { route in
            RemoteSessionView(mode: route.sessionMode, showTouchIndicators: showTouchIndicators)
        }
    }

    private var certificateStatusText: (text: String, valid: Bool)? {
        let trimmed = certInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let cert = CertificateCodec.decode(trimmed), !cert.isEmpty {
            return ("Certificato valido · \(cert.count) byte", true)
        }
        return ("Formato non valido (atteso Hex o Base64)", false)
    }

    private func launchRemote() {
        guard let normalized = EndpointParser.normalizeAddress(address) else {
            errorMessage = "Inserisci un indirizzo valido (es. 192.168.1.118 o 192.168.1.118:47800)."
            return
        }
        guard let cert = CertificateCodec.decode(certInput), !cert.isEmpty else {
            errorMessage = "Inserisci il certificato DER dell'host in formato esadecimale o Base64."
            return
        }
        errorMessage = ""
        let codec: VideoCodec = codecRaw == "h264" ? .h264 : .h265
        activeMode = .remote(address: normalized, cert: cert, codec: codec)
    }
}
