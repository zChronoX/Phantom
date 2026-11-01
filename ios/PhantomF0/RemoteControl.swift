import Foundation
import Network

/// Comandi di emergenza verso la sonda sul POCO (porta HTTP 47801).
enum RemoteControl {
    /// Chiede alla sonda di fermare la sessione video scrcpy in corso: libera lo schermo del telefono
    /// anche quando questa app non riesce più a chiudere il flusso da sola.
    static func stopSessions(host: String, completion: @escaping (String) -> Void) {
        let h = host.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty, let port = NWEndpoint.Port(rawValue: 47801) else {
            completion("Inserisci l'indirizzo IP del POCO")
            return
        }
        let queue = DispatchQueue(label: "app.phantom.f0.remote")
        let conn = NWConnection(host: NWEndpoint.Host(h), port: port, using: .tcp)
        var finished = false // toccata solo da `queue`

        func finish(_ text: String) {
            if finished { return }
            finished = true
            conn.cancel()
            DispatchQueue.main.async { completion(text) }
        }

        conn.stateUpdateHandler = { state in
            switch state {
            case .ready:
                let request = "GET /stop HTTP/1.1\r\nHost: \(h)\r\nConnection: close\r\n\r\n"
                conn.send(content: Data(request.utf8), completion: .contentProcessed { _ in })
                conn.receive(minimumIncompleteLength: 1, maximumLength: 4096) { data, _, _, _ in
                    let body = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
                    finish(body.contains("\"stopped\":true") ? "Sessione fermata sul POCO" : "Nessuna sessione attiva sul POCO")
                }
            case .failed(let error):
                finish("Errore: \(error.localizedDescription)")
            default:
                break
            }
        }
        conn.start(queue: queue)
        queue.asyncAfter(deadline: .now() + 5) { finish("Il POCO non risponde (timeout di 5 secondi)") }
    }
}
