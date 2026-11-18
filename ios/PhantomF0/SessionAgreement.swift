import Foundation

/// Esito dell'apertura della sessione del core Rust, riassunto in parole per il rapporto della prova.
///
/// I bit delle funzioni si leggono con `capsContains` dei binding UniFFI; qui arrivano già come booleani, quindi questa
/// parte è pura (solo Foundation) e si prova con `swiftc` in `ios/Tests`, senza telefono e senza la libreria Rust.
struct SessionAgreement: Equatable {
    /// Versione del protocollo concordata dai due lati.
    var proto: UInt8
    /// Codec video annunciati da entrambi.
    var hevc: Bool
    var avc: Bool
    /// Recupero delle perdite concordato.
    var parity: Bool
    var retransmit: Bool
    var keyframeRequest: Bool
    /// Il client riporta all'host il ritardo di coda che misura.
    var queueDelay: Bool
    /// Valore casuale annunciato dall'altro lato (16 byte).
    var peerNonce: Data

    /// Codec che l'host deve usare: HEVC se concordato, altrimenti H.264. `nil` se non c'è nulla in comune.
    var videoCodec: String? {
        if hevc { return "HEVC" }
        if avc { return "H.264" }
        return nil
    }

    /// Come si recuperano le perdite, in breve.
    var lossRecovery: [String] {
        var out: [String] = []
        if parity { out.append("parità") }
        if retransmit { out.append("ritrasmissioni") }
        if keyframeRequest { out.append("keyframe su richiesta") }
        return out
    }

    /// Riga per il rapporto: cosa hanno concordato i due lati.
    var summary: String {
        let codec = videoCodec ?? "nessun codec video in comune"
        let recovery = lossRecovery.isEmpty ? "nessun recupero delle perdite" : lossRecovery.joined(separator: ", ")
        let delay = queueDelay ? " · ritardo di coda riportato all'host" : ""
        return "sessione: protocollo \(proto) · \(codec) · \(recovery)\(delay) · nonce dell'altro lato \(peerNonce.count) byte"
    }

    /// Un motivo di chiusura in parole; `nil` = sessione ancora aperta.
    static func closeText(_ reason: UInt8?) -> String {
        guard let reason = reason else { return "ancora aperta" }
        switch reason {
        case 0: return "normale (motivo 0)"
        case 1: return "versione del protocollo non supportata (motivo 1)"
        default: return "motivo \(reason)"
        }
    }
}
