import Foundation

// Test della lettura dell'esito della sessione (ios/PhantomF0/SessionAgreement.swift): cosa finisce nel rapporto.
// Senza XCTest: si compila con swiftc ed esce con codice diverso da zero se qualcosa non torna (vedi run-tests.sh).

var failures = 0
func check(_ cond: Bool, _ msg: String) {
    print((cond ? "ok   " : "FAIL ") + msg)
    if !cond { failures += 1 }
}

func agreement(hevc: Bool = false, avc: Bool = false, parity: Bool = false, retransmit: Bool = false,
               keyframe: Bool = false, delay: Bool = false) -> SessionAgreement {
    SessionAgreement(proto: 0, hevc: hevc, avc: avc, parity: parity, retransmit: retransmit,
                     keyframeRequest: keyframe, queueDelay: delay, peerNonce: Data(repeating: 7, count: 16))
}

// 1. Tutto concordato: HEVC, parità, ritrasmissioni, keyframe su richiesta, ritardo di coda.
let full = agreement(hevc: true, parity: true, retransmit: true, keyframe: true, delay: true)
check(full.videoCodec == "HEVC", "con HEVC si usa HEVC")
check(full.lossRecovery == ["parità", "ritrasmissioni", "keyframe su richiesta"], "recupero delle perdite in ordine")
check(full.summary.contains("protocollo 0") && full.summary.contains("HEVC"), "il riassunto dice versione e codec")
check(full.summary.contains("parità, ritrasmissioni, keyframe su richiesta"), "il riassunto elenca il recupero")
check(full.summary.contains("ritardo di coda") && full.summary.contains("16 byte"), "ritardo di coda e nonce nel riassunto")

// 2. Solo H.264: si usa quello, e senza funzioni di recupero il rapporto lo dice.
let avcOnly = agreement(avc: true)
check(avcOnly.videoCodec == "H.264", "senza HEVC si usa H.264")
check(avcOnly.lossRecovery.isEmpty, "nessuna funzione di recupero")
check(avcOnly.summary.contains("nessun recupero delle perdite"), "il riassunto dice che non si recupera")

// 3. Nessun codec in comune: il rapporto non finge che il video possa partire.
let none = agreement()
check(none.videoCodec == nil, "nessun codec concordato")
check(none.summary.contains("nessun codec video in comune"), "il riassunto dice che non c'è un codec")

// 4. Motivi di chiusura.
check(SessionAgreement.closeText(nil) == "ancora aperta", "sessione aperta")
check(SessionAgreement.closeText(0) == "normale (motivo 0)", "motivo 0: chiusura normale")
check(SessionAgreement.closeText(1).contains("versione"), "motivo 1: versione non supportata")
check(SessionAgreement.closeText(2).contains("token"), "motivo 2: token di accoppiamento non valido")
check(SessionAgreement.closeText(9) == "motivo 9", "motivo sconosciuto riportato così com'è")

print(failures == 0 ? "\nTutti i test superati" : "\n\(failures) test falliti")
exit(failures == 0 ? 0 : 1)
