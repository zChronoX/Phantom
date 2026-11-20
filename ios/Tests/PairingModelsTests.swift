import Foundation

private func check(_ cond: Bool, _ msg: String) {
    if !cond {
        fputs("FALLITO: \(msg)\n", stderr)
        exit(1)
    }
    print("ok · \(msg)")
}

// 1. SHA-256 corrisponde al vettore standard "abc" e all'implementazione Android
let abcFp = PhantomSHA256.fingerprintHex(Data("abc".utf8), length: 16)
check(abcFp == "ba7816bf8f01cfea", "SHA-256 fingerprint 'abc' coerente con Android")

// 2. Roundtrip PairingURI con LAN + P2P integrato + WAN opzionale + token + nome con spazi
let sampleCert = Data([0x30, 0x82, 0x01, 0x0A, 0x05, 0xFF, 0x7E, 0x11])
let sampleP2p = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
let sampleTok = Data((1...16).map { UInt8($0) })
let uri = PairingURI.build(
    name: "Redmi Note 11 Pro",
    lan: "192.168.1.42:47800",
    p2pId: sampleP2p,
    wan: "100.88.12.5:47800",
    authToken: sampleTok,
    certDer: sampleCert
)
let parsed = PairingURI.parse(uri)
check(parsed != nil, "PairingURI parsato correttamente")
check(parsed?.name == "Redmi Note 11 Pro", "nome dispositivo preservato")
check(parsed?.lanAddress == "192.168.1.42:47800", "indirizzo LAN preservato")
check(parsed?.p2pEndpointId == sampleP2p, "EndpointId P2P preservato")
check(parsed?.remoteAddress == "100.88.12.5:47800", "indirizzo remoto opzionale preservato")
check(parsed?.authToken == sampleTok, "token di accoppiamento 16 byte preservato")
check(parsed?.certificateDer == sampleCert, "certificato DER intatto dopo Base64URL")
check(PairingURI.parse("https://example.com") == nil, "URI non Phantom rifiutato")
check(PairingURI.parse("phantom://pair?name=solo") == nil, "URI senza certificato rifiutato")

// 3. Supporto hostname DNS in EndpointParser
check(
    EndpointParser.normalizeAddress("casa.duckdns.org") == "casa.duckdns.org:47800",
    "hostname DDNS senza porta ottiene porta predefinita 47800"
)
check(
    EndpointParser.normalizeAddress("redmi.tailnet.ts.net:47800") == "redmi.tailnet.ts.net:47800",
    "hostname con porta preservato"
)

// 4. Selezione candidati in Wi-Fi: Bonjour LAN -> LAN salvata -> P2P integrato -> Remoto manuale
let dev = PairedDevice(
    name: "Redmi",
    lanAddress: "192.168.1.42:47800",
    p2pEndpointId: sampleP2p,
    remoteAddress: "casa.duckdns.org:47800",
    certificateDer: sampleCert
)
let onWifi = EndpointCandidateSelector.candidates(for: dev, discoveredLan: "192.168.1.99:47800", cellularOnly: false)
check(onWifi.count == 4, "in Wi-Fi: 4 candidati (Bonjour + LAN + P2P + Remoto)")
check(onWifi[0].target == .directQuic(address: "192.168.1.99:47800", isLan: true), "priorità 1: LAN live Bonjour")
check(onWifi[1].target == .directQuic(address: "192.168.1.42:47800", isLan: true), "priorità 2: LAN salvata")
check(onWifi[2].target == .p2pIroh(endpointId: sampleP2p), "priorità 3: P2P integrato iroh")
check(onWifi[3].target == .directQuic(address: "casa.duckdns.org:47800", isLan: false), "priorità 4: Remoto manuale")

// 5. In 5G/4G (cellularOnly): salta la LAN e va subito su P2P integrato
let on5g = EndpointCandidateSelector.candidates(for: dev, discoveredLan: "192.168.1.99:47800", cellularOnly: true)
check(on5g.count == 2, "in 5G salta le LAN e prova solo P2P + remoto")
check(on5g[0].target == .p2pIroh(endpointId: sampleP2p), "in 5G primo tentativo immediato su P2P integrato")

// 6. Deduplicazione quando Bonjour coincide con LAN salvata
let deduped = EndpointCandidateSelector.candidates(for: dev, discoveredLan: "192.168.1.42:47800", cellularOnly: false)
check(deduped.count == 3, "deduplicazione quando Bonjour coincide con LAN salvata")

// 7. Backoff riconnessione automatica
check(ReconnectBackoff.delaySeconds(attempt: 1) < 1.0, "primo tentativo rapido (< 1s)")
check(ReconnectBackoff.delaySeconds(attempt: 12) == 8.0, "backoff limitato a 8s")

print("Tutti i test superati.")
