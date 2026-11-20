import SwiftUI
import UIKit
import AVFoundation

/// Scanner QR nativo basato su `AVCaptureSession` per l'accoppiamento 1-tap con Phantom Host su Android.
struct QRScannerSheet: View {
    let onScanned: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var cameraError = ""

    var body: some View {
        NavigationStack {
            ZStack {
                Color.black.ignoresSafeArea()

                if cameraError.isEmpty {
                    QRScannerCameraView(
                        onCode: { code in
                            onScanned(code)
                            dismiss()
                        },
                        onError: { err in
                            cameraError = err
                        }
                    )
                    .ignoresSafeArea()

                    RoundedRectangle(cornerRadius: 20)
                        .strokeBorder(.white.opacity(0.85), lineWidth: 3)
                        .frame(width: 240, height: 240)
                        .allowsHitTesting(false)

                    VStack {
                        Spacer()
                        Text("Inquadra il codice QR mostrato in Phantom su Android")
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(.white)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 10)
                            .phantomGlassCapsule(overVideo: true)
                            .padding(.bottom, 32)
                    }
                } else {
                    VStack(spacing: 12) {
                        Image(systemName: "camera.fill")
                            .font(.largeTitle)
                            .foregroundStyle(.orange)
                        Text(cameraError)
                            .font(.subheadline)
                            .foregroundStyle(.white)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 24)
                    }
                }
            }
            .navigationTitle("Scansiona QR")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Chiudi") { dismiss() }
                }
            }
        }
    }
}

private struct QRScannerCameraView: UIViewControllerRepresentable {
    let onCode: (String) -> Void
    let onError: (String) -> Void

    func makeUIViewController(context: Context) -> QRScannerController {
        let ctrl = QRScannerController()
        ctrl.onCode = onCode
        ctrl.onError = onError
        return ctrl
    }

    func updateUIViewController(_ uiViewController: QRScannerController, context: Context) {}
}

private final class QRScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    var onError: ((String) -> Void)?

    private let session = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private let sessionQueue = DispatchQueue(label: "app.phantom.qr.camera")
    private var didEmit = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black

        let status = AVCaptureDevice.authorizationStatus(for: .video)
        switch status {
        case .authorized:
            configureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    if granted {
                        self?.configureSession()
                    } else {
                        self?.onError?("Permesso fotocamera negato. Abilitalo in Impostazioni → Phantom.")
                    }
                }
            }
        default:
            onError?("Permesso fotocamera non disponibile. Abilitalo in Impostazioni → Phantom.")
        }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        sessionQueue.async { [session] in
            if session.isRunning {
                session.stopRunning()
            }
        }
    }

    private func configureSession() {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input) else {
            onError?("Fotocamera non disponibile su questo dispositivo.")
            return
        }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            onError?("Impossibile avviare la lettura QR.")
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: DispatchQueue.main)
        output.metadataObjectTypes = [.qr]

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.addSublayer(preview)
        previewLayer = preview

        sessionQueue.async { [session] in
            session.startRunning()
        }
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !didEmit else { return }
        for obj in metadataObjects {
            guard let readable = obj as? AVMetadataMachineReadableCodeObject,
                  let text = readable.stringValue,
                  !text.isEmpty else { continue }
            didEmit = true
            sessionQueue.async { [session] in
                if session.isRunning { session.stopRunning() }
            }
            onCode?(text)
            break
        }
    }
}
