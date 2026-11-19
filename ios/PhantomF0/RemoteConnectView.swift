import SwiftUI
import UIKit

/// Schermata di accoppiamento QR, dispositivi salvati (1-tap LAN + 5G/4G) e controllo remoto QUIC.
struct RemoteConnectView: View {
    @StateObject private var deviceStore = PairedDeviceStore()
    @StateObject private var bonjour = BonjourDiscovery()
    @StateObject private var networkPath = NetworkPathObserver()

    @AppStorage("remoteAddress") private var address = "192.168.1.118:47800"
    @AppStorage("remoteCert") private var certInput = ""
    @AppStorage("remoteCodec") private var codecRaw = "h265"
    @AppStorage("remoteShowTouch") private var showTouchIndicators = true
    @AppStorage("loopbackFps") private var loopbackFps = 120

    @State private var activeMode: ActiveRoute?
    @State private var showScanner = false
    @State private var editingDevice: PairedDevice?
    @State private var errorMessage = ""
    @State private var infoMessage = ""

    private enum ActiveRoute: Identifiable {
        case remote(id: String, endpoints: [RemoteEndpointCandidate], cert: Data, codec: VideoCodec)
        case loopback(fps: Int)

        var id: String {
            switch self {
            case let .remote(id, _, _, _): return "remote-\(id)"
            case let .loopback(fps): return "loopback-\(fps)"
            }
        }

        var sessionMode: RemoteSessionMode {
            switch self {
            case let .remote(_, endpoints, cert, codec):
                return .remote(endpoints: endpoints, certificateDer: cert, codec: codec, autoReconnect: true)
            case let .loopback(fps):
                return .localQuicLoopback(fps: fps, width: 1080, height: 2400)
            }
        }
    }

    var body: some View {
        Form {
            Section {
                Button {
                    errorMessage = ""
                    infoMessage = ""
                    showScanner = true
                } label: {
                    Label("Scansiona QR da Android", systemImage: "qrcode.viewfinder")
                        .frame(maxWidth: .infinity)
                        .fontWeight(.semibold)
                }

                if deviceStore.devices.isEmpty {
                    Text("Inquadra una sola volta il QR mostrato in Phantom Host su Android. Da quel momento potrai connetterti con 1 tap sia a casa (Wi-Fi) sia fuori in 5G/4G.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(deviceStore.devices) { device in
                        savedDeviceRow(device)
                    }
                    .onDelete { offsets in
                        deviceStore.remove(at: offsets)
                    }
                }

                if !infoMessage.isEmpty {
                    Text(infoMessage)
                        .font(.footnote)
                        .foregroundStyle(.green)
                }
            } header: {
                Text("Dispositivi salvati (1-tap · Wi-Fi + 5G/4G)")
            } footer: {
                if !bonjour.hosts.isEmpty {
                    Text("Rete iPhone: \(networkPath.summaryLabel) · In LAN: \(bonjour.hosts.map(\.displayName).joined(separator: ", "))")
                } else {
                    Text("Rete iPhone: \(networkPath.summaryLabel) · In 5G/4G usa automaticamente il P2P integrato (senza VPN).")
                }
            }

            Section("Opzioni sessione") {
                Picker("Codec preferito", selection: $codecRaw) {
                    Text("HEVC (H.265)").tag("h265")
                    Text("H.264 (AVC)").tag("h264")
                }
                Toggle("Mostra cerchi sul tocco", isOn: $showTouchIndicators)
            }

            Section("Connessione manuale (IP, Tailscale o DDNS)") {
                TextField("IP o hostname:porta (es. casa.duckdns.org:47800)", text: $address)
                    .keyboardType(.URL)
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
                            let cleaned = text.trimmingCharacters(in: .whitespacesAndNewlines)
                            if let parsed = PairingURI.parse(cleaned) {
                                handleScannedPairing(parsed)
                            } else {
                                certInput = cleaned
                            }
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

                Button {
                    launchManualRemote()
                } label: {
                    Label("Connetti manualmente", systemImage: "play.display")
                        .frame(maxWidth: .infinity)
                        .fontWeight(.semibold)
                }

                if !errorMessage.isEmpty {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }

            Section("Prova locale su iPhone") {
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
            }
        }
        .navigationTitle("Controllo remoto (QUIC)")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            bonjour.start()
            networkPath.start()
        }
        .onDisappear {
            bonjour.stop()
            networkPath.stop()
        }
        .sheet(isPresented: $showScanner) {
            QRScannerSheet { scannedText in
                if let parsed = PairingURI.parse(scannedText) {
                    handleScannedPairing(parsed)
                } else {
                    errorMessage = "Codice QR non riconosciuto (atteso QR di Phantom Host)."
                }
            }
        }
        .sheet(item: $editingDevice) { device in
            EditPairedDeviceSheet(device: device) { name, lan, remote in
                deviceStore.update(id: device.id, name: name, lanAddress: .some(lan), remoteAddress: .some(remote))
            }
        }
        .fullScreenCover(item: $activeMode) { route in
            RemoteSessionView(mode: route.sessionMode, showTouchIndicators: showTouchIndicators)
        }
    }

    @ViewBuilder
    private func savedDeviceRow(_ device: PairedDevice) -> some View {
        let liveLan = bonjour.resolvedAddress(forFingerprint: device.id)
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(device.name)
                            .font(.headline)
                        if liveLan != nil {
                            Text("In LAN")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.white)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(.green, in: Capsule())
                        }
                    }
                    Text("ID \(device.id)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    editingDevice = device
                } label: {
                    Image(systemName: "slider.horizontal.3")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
            }

            if let lan = liveLan ?? device.lanAddress {
                Label("Casa (LAN): \(lan)", systemImage: "wifi")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            if let p2p = device.p2pEndpointId {
                Label("5G/4G P2P integrato · \(p2p.prefix(12))…", systemImage: "globe")
                    .font(.caption.monospaced())
                    .foregroundStyle(.green)
            }
            if let remote = device.remoteAddress {
                Label("Remoto manuale: \(remote)", systemImage: "antenna.radiowaves.left.and.right")
                    .font(.caption.monospaced())
                    .foregroundStyle(.blue)
            }
            if device.p2pEndpointId == nil && device.remoteAddress == nil {
                Text("Riscansiona il QR dal nuovo Phantom Host per abilitare il 5G/4G P2P integrato (senza VPN)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }

            Button {
                launchSavedDevice(device, liveLan: liveLan)
            } label: {
                Label("Connetti", systemImage: "play.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .padding(.top, 2)
        }
        .padding(.vertical, 4)
    }

    private func handleScannedPairing(_ parsed: ParsedPairing) {
        let saved = deviceStore.upsert(from: parsed)
        errorMessage = ""
        infoMessage = "Dispositivo \"\(saved.name)\" salvato (ID \(saved.id))."
    }

    private func launchSavedDevice(_ device: PairedDevice, liveLan: String?) {
        let candidates = EndpointCandidateSelector.candidates(
            for: device,
            discoveredLan: liveLan,
            cellularOnly: networkPath.isOnCellularOnly
        )
        guard !candidates.isEmpty else {
            errorMessage = "Nessun indirizzo o ID P2P salvato per \(device.name)."
            return
        }
        errorMessage = ""
        let codec: VideoCodec = codecRaw == "h264" ? .h264 : .h265
        activeMode = .remote(id: device.id, endpoints: candidates, cert: device.certificateDer, codec: codec)
    }

    private var certificateStatusText: (text: String, valid: Bool)? {
        let trimmed = certInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let cert = CertificateCodec.decode(trimmed), !cert.isEmpty {
            return ("Certificato valido · \(cert.count) byte", true)
        }
        return ("Formato non valido (atteso Hex o Base64)", false)
    }

    private func launchManualRemote() {
        guard let normalized = EndpointParser.normalizeAddress(address) else {
            errorMessage = "Inserisci un indirizzo valido (es. 192.168.1.118:47800 o casa.duckdns.org:47800)."
            return
        }
        guard let cert = CertificateCodec.decode(certInput), !cert.isEmpty else {
            errorMessage = "Inserisci il certificato DER dell'host in formato esadecimale o Base64."
            return
        }
        errorMessage = ""
        let codec: VideoCodec = codecRaw == "h264" ? .h264 : .h265
        let candidate = RemoteEndpointCandidate(
            target: .directQuic(address: normalized, isLan: false),
            label: "Manuale",
            timeoutMs: 5_000
        )
        activeMode = .remote(id: normalized, endpoints: [candidate], cert: cert, codec: codec)
    }
}

private struct EditPairedDeviceSheet: View {
    let device: PairedDevice
    let onSave: (String, String?, String?) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var name: String
    @State private var lan: String
    @State private var remote: String

    init(device: PairedDevice, onSave: @escaping (String, String?, String?) -> Void) {
        self.device = device
        self.onSave = onSave
        _name = State(initialValue: device.name)
        _lan = State(initialValue: device.lanAddress ?? "")
        _remote = State(initialValue: device.remoteAddress ?? "")
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Dispositivo") {
                    TextField("Nome", text: $name)
                    Text("ID certificato: \(device.id)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }

                Section {
                    TextField("LAN casa (es. 192.168.1.42:47800)", text: $lan)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Indirizzo Wi-Fi di casa")
                }

                Section {
                    TextField("Remoto (es. 100.x.y.z:47800 o casa.duckdns.org:47800)", text: $remote)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Indirizzo remoto 5G/4G")
                } footer: {
                    Text("Quando sei fuori casa in 5G/4G Phantom prova la LAN per ~1,6 s e poi passa subito automaticamente all'indirizzo remoto (Tailscale, WireGuard o DDNS).")
                }
            }
            .navigationTitle("Modifica dispositivo")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Annulla") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Salva") {
                        let lanVal = lan.trimmingCharacters(in: .whitespacesAndNewlines)
                        let remVal = remote.trimmingCharacters(in: .whitespacesAndNewlines)
                        onSave(name, lanVal.isEmpty ? nil : lanVal, remVal.isEmpty ? nil : remVal)
                        dismiss()
                    }
                }
            }
        }
    }
}
