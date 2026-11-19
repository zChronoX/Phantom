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

// 2. Roundtrip PairingURI con LAN + WAN 5G/Tailscale + nome con spazi
let sampleCert = Data([0x30, 0x82, 0x01, 0x0A, 0x05, 0xFF, 0x7E, 0x11])
let uri = PairingURI.build(
    name: "Redmi Note 11 Pro",
    lan: "192.168.1.42:47800",
    wan: "100.88.12.5:47800",
    certDer: sampleCert
)
let parsed = PairingURI.parse(uri)
check(parsed != nil, "PairingURI parsato correttamente")
check(parsed?.name == "Redmi Note 11 Pro", "nome dispositivo preservato")
check(parsed?.lanAddress == "192.168.1.42:47800", "indirizzo LAN preservato")
check(parsed?.remoteAddress == "100.88.12.5:47800", "indirizzo remoto 5G/Tailscale preservato")
check(parsed?.certificateDer == sampleCert, "certificato DER intatto dopo Base64URL")
check(PairingURI.parse("https://example.com") == nil, "URI non Phantom rifiutato")
check(PairingURI.parse("phantom://pair?name=solo") == nil, "URI senza certificato rifiutato")

// 3. Supporto hostname DNS (Tailscale MagicDNS / DDNS) in EndpointParser
check(
    EndpointParser.normalizeAddress("casa.duckdns.org") == "casa.duckdns.org:47800",
    "hostname DDNS senza porta ottiene porta predefinita 47800"
)
check(
    EndpointParser.normalizeAddress("redmi.tailnet.ts.net:47800") == "redmi.tailnet.ts.net:47800",
    "hostname Tailscale MagicDNS con porta preservato"
)

// 4. Selezione candidati LAN live -> LAN salvata -> Remoto 5G con timeout rapido e deduplicazione
let dev = PairedDevice(
    name: "Redmi",
    lanAddress: "192.168.1.42:47800",
    remoteAddress: "100.88.12.5:47800",
    certificateDer: sampleCert
)
let withBonjour = EndpointCandidateSelector.candidates(for: dev, discoveredLan: "192.168.1.99:47800")
check(withBonjour.count == 3, "tre candidati distinti (Bonjour + LAN salvato + Remoto 5G)")
check(withBonjour[0].address == "192.168.1.99:47800", "priorità 1: LAN live Bonjour")
check(withBonjour[1].address == "192.168.1.42:47800" && withBonjour[1].timeoutMs == 1_600, "priorità 2: LAN salvato con timeout breve per failover 5G")
check(withBonjour[2].address == "100.88.12.5:47800" && withBonjour[2].timeoutMs == 5_000, "priorità 3: Remoto 5G/VPN")

let deduped = EndpointCandidateSelector.candidates(for: dev, discoveredLan: "192.168.1.42:47800")
check(deduped.count == 2, "deduplicazione quando Bonjour coincide con LAN salvato")

let lanOnly = PairedDevice(name: "SoloCasa", lanAddress: "192.168.1.42", remoteAddress: nil, certificateDer: sampleCert)
let lanOnlyCands = EndpointCandidateSelector.candidates(for: lanOnly, discoveredLan: nil)
check(lanOnlyCands.count == 1 && lanOnlyCands[0].timeoutMs == 5_000, "senza remoto la LAN usa timeout pieno 5s")

// 5. Backoff riconnessione automatica
check(ReconnectBackoff.delaySeconds(attempt: 1) == 1.0, "primo tentativo dopo 1s")
check(ReconnectBackoff.delaySeconds(attempt: 8) == 8.0, "backoff limitato a 8s")

print("Tutti i test superati.")
