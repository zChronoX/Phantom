import SwiftUI
import UIKit

/// Schermata principale di Phantom: design monocromatico Liquid Glass (Light & Dark Mode), dispositivi 1-tap,
/// scanner QR immediato e impostazioni/diagnostica in un foglio dedicato.
struct PhantomHomeView: View {
    @Environment(\.colorScheme) private var colorScheme

    @StateObject private var deviceStore = PairedDeviceStore()
    @StateObject private var bonjour = BonjourDiscovery()
    @StateObject private var networkPath = NetworkPathObserver()

    @AppStorage("remoteAddress") private var manualAddress = "192.168.1.128:47800"
    @AppStorage("remoteCert") private var manualCertInput = ""
    @AppStorage("remoteCodec") private var codecRaw = "h265"
    @AppStorage("remoteShowTouch") private var showTouchIndicators = true
    @AppStorage("loopbackFps") private var loopbackFps = 120

    @State private var activeMode: ActiveRoute?
    @State private var showScanner = false
    @State private var showSettings = false
    @State private var editingDevice: PairedDevice?
    @State private var bannerMessage: (text: String, isError: Bool)?
    @State private var crashReport = CrashLog.previousReport

    private enum ActiveRoute: Identifiable {
        case remote(id: String, endpoints: [RemoteEndpointCandidate], cert: Data, authToken: Data?, codec: VideoCodec)
        case loopback(fps: Int)

        var id: String {
            switch self {
            case let .remote(id, _, _, _, _): return "remote-\(id)"
            case let .loopback(fps): return "loopback-\(fps)"
            }
        }

        var sessionMode: RemoteSessionMode {
            switch self {
            case let .remote(_, endpoints, cert, authToken, codec):
                return .remote(
                    endpoints: endpoints,
                    certificateDer: cert,
                    authToken: authToken,
                    codec: codec,
                    autoReconnect: true
                )
            case let .loopback(fps):
                return .localQuicLoopback(fps: fps, width: 1080, height: 2400)
            }
        }
    }

    var body: some View {
        NavigationStack {
            ZStack(alignment: .bottom) {
                PhantomPalette.canvas(colorScheme)
                    .ignoresSafeArea()

                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        networkHeaderRow

                        if let report = crashReport {
                            crashNoticeCard(report)
                        }

                        if deviceStore.devices.isEmpty {
                            emptyStateView
                        } else {
                            VStack(spacing: 14) {
                                ForEach(deviceStore.devices) { device in
                                    deviceCard(device)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 8)
                    .padding(.bottom, 40)
                }

                if let banner = bannerMessage {
                    HStack(spacing: 8) {
                        Image(systemName: banner.isError ? "exclamationmark.circle.fill" : "checkmark.circle.fill")
                            .foregroundStyle(banner.isError ? PhantomPalette.dangerRed : PhantomPalette.liveGreen)
                        Text(banner.text)
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(.primary)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .phantomGlassCapsule()
                    .padding(.bottom, 18)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
            .navigationTitle("Phantom")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showScanner = true
                    } label: {
                        Image(systemName: "plus")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(.primary)
                    }
                    .accessibilityLabel("Aggiungi dispositivo tramite QR")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "slider.horizontal.3")
                            .font(.body.weight(.medium))
                            .foregroundStyle(.primary)
                    }
                    .accessibilityLabel("Impostazioni e diagnostica")
                }
            }
        }
        .tint(.primary)
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
                    showBanner("Codice QR non riconosciuto", isError: true)
                }
            }
        }
        .sheet(isPresented: $showSettings) {
            PhantomSettingsSheet(
                codecRaw: $codecRaw,
                showTouchIndicators: $showTouchIndicators,
                manualAddress: $manualAddress,
                manualCertInput: $manualCertInput,
                loopbackFps: $loopbackFps,
                onPastePairing: { parsed in
                    handleScannedPairing(parsed)
                },
                onManualConnect: { address, cert in
                    showSettings = false
                    launchManualRemote(address: address, cert: cert)
                },
                onLoopbackStart: { fps in
                    showSettings = false
                    activeMode = .loopback(fps: fps)
                }
            )
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

    private var networkHeaderRow: some View {
        HStack(spacing: 8) {
            HStack(spacing: 6) {
                Circle()
                    .fill(networkPath.isSatisfied ? PhantomPalette.liveGreen : PhantomPalette.warnAmber)
                    .frame(width: 7, height: 7)
                Text(networkPath.summaryLabel)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 6)
            .phantomGlassCapsule()

            if !bonjour.hosts.isEmpty {
                HStack(spacing: 5) {
                    Image(systemName: "dot.radiowaves.left.and.right")
                        .font(.caption2)
                    Text("\(bonjour.hosts.count) in rete")
                        .font(.caption.weight(.medium))
                }
                .foregroundStyle(.secondary)
                .padding(.horizontal, 11)
                .padding(.vertical, 6)
                .phantomGlassCapsule()
            }

            Spacer()
        }
    }

    private var emptyStateView: some View {
        VStack(spacing: 22) {
            ZStack {
                Circle()
                    .fill(colorScheme == .dark ? Color.white.opacity(0.06) : Color.black)
                    .frame(width: 84, height: 84)
                PhantomGhostMark()
                    .fill(.white, style: FillStyle(eoFill: true))
                    .frame(width: 52, height: 52)
            }
            .overlay {
                Circle()
                    .strokeBorder(PhantomPalette.specularRim(colorScheme), lineWidth: 0.7)
            }
            .padding(.top, 24)

            VStack(spacing: 8) {
                Text("Nessun dispositivo")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(.primary)
                Text("Apri Phantom su Android e inquadra il codice QR per collegarti in Wi-Fi o 5G con un tocco.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 12)
            }

            Button {
                showScanner = true
            } label: {
                Label("Scansiona QR", systemImage: "qrcode.viewfinder")
            }
            .buttonStyle(PhantomPrimaryButtonStyle())
            .padding(.bottom, 16)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 28)
        .padding(.horizontal, 20)
        .phantomGlassCard(cornerRadius: 26)
        .padding(.top, 16)
    }

    private func deviceCard(_ device: PairedDevice) -> some View {
        let liveLan = bonjour.resolvedAddress(forFingerprint: device.id)
        let routeSubtitle: String = {
            if let lan = liveLan { return "LAN · \(lan)" }
            if networkPath.isOnCellularOnly, device.p2pEndpointId != nil { return "5G · P2P integrato" }
            if let lan = device.lanAddress { return "Casa · \(lan)" }
            if device.p2pEndpointId != nil { return "P2P ovunque" }
            return device.remoteAddress ?? "Solo certificato"
        }()

        return VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 14) {
                ZStack {
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .fill(colorScheme == .dark ? Color.white.opacity(0.09) : Color.black.opacity(0.06))
                        .frame(width: 46, height: 46)
                    Image(systemName: "iphone.gen3")
                        .font(.title3.weight(.medium))
                        .foregroundStyle(.primary)
                }

                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(device.name)
                            .font(.headline.weight(.semibold))
                            .foregroundStyle(.primary)
                            .lineLimit(1)

                        if liveLan != nil {
                            Circle()
                                .fill(PhantomPalette.liveGreen)
                                .frame(width: 7, height: 7)
                        }
                    }

                    HStack(spacing: 6) {
                        Text(routeSubtitle)
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .lineLimit(1)

                        if device.authToken != nil {
                            Image(systemName: "lock.fill")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Spacer(minLength: 8)

                Menu {
                    Button {
                        editingDevice = device
                    } label: {
                        Label("Modifica indirizzi", systemImage: "pencil")
                    }
                    Button {
                        showScanner = true
                    } label: {
                        Label("Aggiorna QR", systemImage: "qrcode.viewfinder")
                    }
                    Divider()
                    Button(role: .destructive) {
                        deviceStore.remove(id: device.id)
                    } label: {
                        Label("Rimuovi", systemImage: "trash")
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .frame(width: 34, height: 34)
                        .contentShape(Rectangle())
                }
            }

            if device.p2pEndpointId != nil && device.authToken == nil {
                HStack(spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.caption2)
                        .foregroundStyle(PhantomPalette.warnAmber)
                    Text("Riscansiona il QR per aggiornare il token di sicurezza")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }

            HStack(spacing: 10) {
                if liveLan != nil {
                    badgeChip("In LAN", dot: PhantomPalette.liveGreen)
                }
                if device.p2pEndpointId != nil {
                    badgeChip(device.authToken != nil ? "5G P2P" : "P2P", icon: device.authToken != nil ? "lock.shield" : "globe")
                }
                Spacer()
                HStack(spacing: 5) {
                    Text("Connetti")
                        .font(.subheadline.weight(.semibold))
                    Image(systemName: "arrow.right")
                        .font(.caption.weight(.bold))
                }
                .foregroundStyle(PhantomPalette.ctaForeground(colorScheme))
                .padding(.horizontal, 16)
                .padding(.vertical, 9)
                .background(PhantomPalette.ctaBackground(colorScheme), in: Capsule())
            }
        }
        .padding(18)
        .phantomGlassCard(cornerRadius: 24)
        .contentShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .onTapGesture {
            launchSavedDevice(device, liveLan: liveLan)
        }
    }

    private func badgeChip(_ text: String, dot: Color? = nil, icon: String? = nil) -> some View {
        HStack(spacing: 5) {
            if let dot = dot {
                Circle().fill(dot).frame(width: 6, height: 6)
            }
            if let icon = icon {
                Image(systemName: icon).font(.caption2)
            }
            Text(text).font(.caption2.weight(.medium))
        }
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(PhantomPalette.elevatedFill(colorScheme), in: Capsule())
    }

    private func crashNoticeCard(_ report: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Chiusura imprevista precedente", systemImage: "exclamationmark.triangle.fill")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(PhantomPalette.warnAmber)
                Spacer()
                Button {
                    CrashLog.clearPrevious()
                    crashReport = nil
                } label: {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                }
            }
            Text(report)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(5)
            Button("Copia rapporto") {
                UIPasteboard.general.string = report
                showBanner("Rapporto copiato", isError: false)
            }
            .font(.caption.weight(.semibold))
        }
        .padding(16)
        .phantomGlassCard(cornerRadius: 20)
    }

    private func handleScannedPairing(_ parsed: ParsedPairing) {
        let saved = deviceStore.upsert(from: parsed)
        showBanner("Dispositivo «\(saved.name)» salvato", isError: false)
    }

    private func launchSavedDevice(_ device: PairedDevice, liveLan: String?) {
        // Il telefono host può cambiare IP (DHCP, MAC casuale dopo un cambio ROM): se Bonjour lo ha appena trovato,
        // aggiorna l'indirizzo LAN salvato così il prossimo avvio non scivola sul P2P (più lento) per un IP vecchio.
        if let live = liveLan, let norm = EndpointParser.normalizeAddress(live), norm != device.lanAddress {
            deviceStore.update(id: device.id, lanAddress: .some(norm))
        }
        let candidates = EndpointCandidateSelector.candidates(
            for: device,
            discoveredLan: liveLan,
            cellularOnly: networkPath.isOnCellularOnly
        )
        guard !candidates.isEmpty else {
            showBanner("Nessun indirizzo salvato per \(device.name)", isError: true)
            return
        }
        let codec: VideoCodec = codecRaw == "h264" ? .h264 : .h265
        activeMode = .remote(
            id: device.id,
            endpoints: candidates,
            cert: device.certificateDer,
            authToken: device.authToken,
            codec: codec
        )
    }

    private func launchManualRemote(address: String, cert: Data) {
        guard let normalized = EndpointParser.normalizeAddress(address) else {
            showBanner("Indirizzo non valido", isError: true)
            return
        }
        let codec: VideoCodec = codecRaw == "h264" ? .h264 : .h265
        let candidate = RemoteEndpointCandidate(
            target: .directQuic(address: normalized, isLan: false),
            label: "Manuale",
            timeoutMs: 5_000
        )
        activeMode = .remote(id: normalized, endpoints: [candidate], cert: cert, authToken: nil, codec: codec)
    }

    private func showBanner(_ text: String, isError: Bool) {
        withAnimation(.spring(response: 0.28, dampingFraction: 0.85)) {
            bannerMessage = (text, isError)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.6) {
            withAnimation(.easeOut(duration: 0.2)) {
                if bannerMessage?.text == text {
                    bannerMessage = nil
                }
            }
        }
    }
}

typealias RemoteConnectView = PhantomHomeView

// MARK: - Impostazioni, connessione manuale e diagnostica

private struct PhantomSettingsSheet: View {
    @Binding var codecRaw: String
    @Binding var showTouchIndicators: Bool
    @Binding var manualAddress: String
    @Binding var manualCertInput: String
    @Binding var loopbackFps: Int

    let onPastePairing: (ParsedPairing) -> Void
    let onManualConnect: (String, Data) -> Void
    let onLoopbackStart: (Int) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var manualError = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Sessione") {
                    Picker("Codec preferito", selection: $codecRaw) {
                        Text("HEVC (H.265)").tag("h265")
                        Text("H.264 (AVC)").tag("h264")
                    }
                    Toggle("Mostra indicatori tocco", isOn: $showTouchIndicators)
                }

                Section("Connessione manuale") {
                    TextField("IP o hostname:porta", text: $manualAddress)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()

                    TextField("Certificato DER (Hex o Base64)", text: $manualCertInput, axis: .vertical)
                        .lineLimit(2...4)
                        .font(.system(.caption, design: .monospaced))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()

                    HStack {
                        Button {
                            if let text = UIPasteboard.general.string {
                                let cleaned = text.trimmingCharacters(in: .whitespacesAndNewlines)
                                if let parsed = PairingURI.parse(cleaned) {
                                    onPastePairing(parsed)
                                    dismiss()
                                } else {
                                    manualCertInput = cleaned
                                }
                            }
                        } label: {
                            Label("Incolla", systemImage: "doc.on.clipboard")
                        }
                        Spacer()
                        if let status = certificateStatusText {
                            Text(status.text)
                                .font(.caption2)
                                .foregroundStyle(status.valid ? PhantomPalette.liveGreen : PhantomPalette.warnAmber)
                        }
                    }

                    Button("Connetti") {
                        guard EndpointParser.normalizeAddress(manualAddress) != nil else {
                            manualError = "Inserisci un indirizzo valido (es. 192.168.1.128:47800)."
                            return
                        }
                        guard let cert = CertificateCodec.decode(manualCertInput), !cert.isEmpty else {
                            manualError = "Certificato DER mancante o non valido."
                            return
                        }
                        manualError = ""
                        onManualConnect(manualAddress, cert)
                    }
                    .fontWeight(.semibold)

                    if !manualError.isEmpty {
                        Text(manualError)
                            .font(.caption)
                            .foregroundStyle(PhantomPalette.dangerRed)
                    }
                }

                Section("Diagnostica e test") {
                    Picker("Loopback locale", selection: $loopbackFps) {
                        Text("60 fps").tag(60)
                        Text("120 fps").tag(120)
                    }
                    Button {
                        onLoopbackStart(loopbackFps)
                    } label: {
                        Label("Avvia loopback QUIC su iPhone", systemImage: "arrow.triangle.2.circlepath")
                    }
                    NavigationLink {
                        DiagnosticsView()
                    } label: {
                        Label("Strumenti diagnostici avanzati", systemImage: "wrench.and.screwdriver")
                    }
                }
            }
            .navigationTitle("Impostazioni")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Fine") { dismiss() }
                        .fontWeight(.semibold)
                }
            }
        }
        .tint(.primary)
    }

    private var certificateStatusText: (text: String, valid: Bool)? {
        let trimmed = manualCertInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let cert = CertificateCodec.decode(trimmed), !cert.isEmpty {
            return ("Valido · \(cert.count) B", true)
        }
        return ("Formato non valido", false)
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
                    LabeledContent("ID") {
                        Text(device.id)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }

                Section("Indirizzo LAN") {
                    TextField("es. 192.168.1.42:47800", text: $lan)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }

                Section("Indirizzo remoto opzionale") {
                    TextField("es. casa.duckdns.org:47800", text: $remote)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
            }
            .navigationTitle("Modifica")
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
                    .fontWeight(.semibold)
                }
            }
        }
        .tint(.primary)
    }
}
