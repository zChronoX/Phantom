import SwiftUI

/// Punto d'ingresso principale di Phantom: apre direttamente sulla schermata pulita dei dispositivi salvati.
@main
struct PhantomF0App: App {
    @Environment(\.scenePhase) private var scenePhase

    init() {
        CrashLog.start()
    }

    var body: some Scene {
        WindowGroup {
            PhantomHomeView()
                .onChange(of: scenePhase) { _, phase in
                    CrashLog.markMem("FASE \(phase)")
                    if phase != .active {
                        TelemetryRecorder.shared.flush()
                    }
                }
        }
    }
}
