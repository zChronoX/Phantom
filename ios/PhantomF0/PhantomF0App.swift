import SwiftUI

/// Phantom F0 · app di misura per la fase 0.
/// Fa lo stesso test di rete della pagina Safari, ma in UDP nativo come farà Phantom:
/// così si separa l'effetto di Safari e del TCP da quello del Wi-Fi.
@main
struct PhantomF0App: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
