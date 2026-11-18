// Prova dei binding Swift: host e client del core Rust collegati in memoria, guidati da codice Swift attraverso UniFFI.
// Si compila e si esegue in CI su macOS (vedi .github/workflows/core.yml); non serve un progetto Xcode.
//
// Controlla ciò che conta per l'app: tipi e nomi generati, passaggio di Data e array, sincronia degli orologi,
// video in ordine, tocco a stato completo, tastiera, apertura e chiusura della sessione, datagrammi malformati
// ignorati, uso da più thread.

import Foundation

let hostOffsetUs: UInt64 = 5_000_000

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("FALLITO: \(message)\n".utf8))
    exit(1)
}

func check(_ condition: Bool, _ message: @autoclosure () -> String) {
    if !condition { fail(message()) }
}

enum Dest { case host, client }

let settings = defaultSessionSettings()
check(maxDatagramSize() == 1200, "dimensione massima del datagramma: \(maxDatagramSize())")
check(maxFingers() == 10, "dita al massimo: \(maxFingers())")
check(settings.codec == .h265, "codec predefinito: \(settings.codec)")
check(settings.fecGroup == 10, "gruppo di parità predefinito: \(settings.fecGroup)")
check(settings.nonce.count == 16, "nonce predefinito di \(settings.nonce.count) byte")

let host = PhantomHost(settings: settings)
let client = PhantomClient(settings: settings)

// Datagrammi malformati o vuoti: ignorati, senza errori né crash.
for junk in [Data(), Data([1, 2, 3]), Data(repeating: 0xFF, count: 1200), Data([0x50, 0, 3])] {
    check(host.onDatagram(nowUs: 0, data: junk).isEmpty, "il host risponde a un datagramma malformato")
    check(client.onDatagram(nowUs: 0, data: junk).isEmpty, "il client risponde a un datagramma malformato")
}

// Scambia datagrammi finché non c'è più nulla da spedire.
func pump(_ t: UInt64, _ initial: [(Dest, Data)]) {
    var queue = initial
    while let (to, datagram) = queue.popLast() {
        switch to {
        case .host:
            for reply in host.onDatagram(nowUs: t + hostOffsetUs, data: datagram) { queue.append((.client, reply)) }
        case .client:
            for reply in client.onDatagram(nowUs: t, data: datagram) { queue.append((.host, reply)) }
        }
    }
}

var frames: [VideoFrame] = []
var touchStates = 0
var keyEvents = 0
let fingers = [TouchPoint(id: 3, x: 1000, y: 2000, pressure: 90)]
var firstFrameWasKey: Bool?

// 3 secondi a passi di 1 ms; un frame ogni 8 ms, tocco al ritmo configurato, un tasto ogni 100 ms.
for step in 0..<3000 {
    let t = UInt64(step) * 1000
    var out: [(Dest, Data)] = client.tick(nowUs: t).map { (.host, $0) }
    if let datagram = client.touchTick(nowUs: t, fingers: fingers) { out.append((.host, datagram)) }
    if step % 100 == 50, let datagram = client.sendKey(nowUs: t, code: 4, down: step % 200 == 50) {
        out.append((.host, datagram))
    }
    pump(t, out)

    if step % 8 == 0 && step >= 100 {
        let hostNow = t + hostOffsetUs
        let key = host.wantsKeyframe(nowUs: hostNow)
        if firstFrameWasKey == nil { firstFrameWasKey = key }
        let payload = Data(repeating: 0xAB, count: key ? 40_000 : 9_000)
        let datagrams = host.sendFrame(key: key, captureTsUs: hostNow, data: payload)
        check(datagrams.allSatisfy { $0.count <= Int(maxDatagramSize()) }, "datagramma oltre il massimo")
        pump(t, datagrams.map { (.client, $0) })
    }

    frames.append(contentsOf: client.takeFrames())
    if let state = host.takeTouch() {
        touchStates += 1
        check(state.fingers == fingers, "dita ricevute: \(state.fingers)")
    }
    while let key = host.takeKey() {
        keyEvents += 1
        check(key.code == 4, "codice del tasto ricevuto: \(key.code)")
    }
}

check(firstFrameWasKey == true, "all'inizio della sessione l'host chiede un keyframe")
check(frames.count > 330, "frame ricevuti: \(frames.count)")
check(frames[0].key && frames[0].config, "il primo frame deve essere un keyframe con i parametri del codec")
check(frames[0].codec == .h265, "codec del frame")
check(zip(frames, frames.dropFirst()).allSatisfy { $0.frameId < $1.frameId }, "i frame non sono in ordine")
check(frames.allSatisfy { $0.data.count == 40_000 || $0.data.count == 9_000 }, "dimensione dei frame alterata")
check(frames.allSatisfy { $0.data.allSatisfy { $0 == 0xAB } }, "contenuto dei frame alterato")
check(touchStates >= 590, "stati del tocco ricevuti dall'host: \(touchStates)")
check(keyEvents >= 29, "eventi di tastiera ricevuti dall'host: \(keyEvents)")

// Apertura della sessione: versione e funzioni concordate, visibili da entrambi i lati.
guard let negotiated = client.negotiated() else { fail("la sessione non è stata negoziata") }
check(negotiated.proto == 0, "versione negoziata: \(negotiated.proto)")
check(negotiated.peerNonce.count == 16, "nonce dell'host: \(negotiated.peerNonce.count) byte")
check(capsContains(caps: negotiated.caps, capability: .h265), "HEVC non concordato")
check(capsContains(caps: negotiated.caps, capability: .xorParity), "parità non concordata")
check(!capsContains(caps: negotiated.caps, capability: .h264), "due codec concordati insieme")
guard let hostSide = host.negotiated() else { fail("l'host non ha negoziato la sessione") }
check(hostSide.caps == negotiated.caps && hostSide.proto == negotiated.proto, "i due lati vedono funzioni diverse")

let counters = client.counters(nowUs: 3_000_000)
guard let offset = counters.clockOffsetUs else { fail("orologi non sincronizzati") }
check(abs(offset - Int64(hostOffsetUs)) < 50, "offset stimato \(offset) µs contro \(hostOffsetUs)")
check(counters.framesLost == 0, "frame persi: \(counters.framesLost)")
check(counters.framesOk == UInt64(frames.count), "framesOk \(counters.framesOk) contro \(frames.count)")
let latencies = frames.compactMap { $0.latencyUs }
check(!latencies.isEmpty && latencies.allSatisfy { abs($0) < 1000 }, "latenza in memoria: \(latencies.prefix(3))")

let hostCounters = host.counters()
check(hostCounters.framesSent > 330, "frame inviati dall'host: \(hostCounters.framesSent)")
check(hostCounters.targetBitrateBps > 6_000_000, "il bitrate deve salire: \(hostCounters.targetBitrateBps)")
check(hostCounters.keyEvents >= 29, "eventi di tastiera contati dall'host: \(hostCounters.keyEvents)")

// Più thread sugli stessi oggetti: le chiamate sono serializzate dal blocco interno.
let group = DispatchGroup()
let queue = DispatchQueue(label: "smoke", attributes: .concurrent)
for _ in 0..<8 {
    queue.async(group: group) {
        for _ in 0..<500 {
            _ = client.counters(nowUs: 3_000_000)
            _ = host.counters()
            _ = host.wantsKeyframe(nowUs: 8_000_000)
        }
    }
}
check(group.wait(timeout: .now() + 20) == .success, "le chiamate concorrenti non terminano")

// Chiusura ordinata: il client chiude, l'host lo registra e nessuno dei due manda più niente.
guard let closeDatagram = client.close(reason: 0) else { fail("la chiusura non è partita") }
check(client.isClosed(), "il client non risulta chiuso")
check(host.onDatagram(nowUs: 4_000_000 + hostOffsetUs, data: closeDatagram).isEmpty, "l'host risponde alla chiusura")
check(host.isClosed(), "l'host non risulta chiuso")
check(host.closeReason() == 0 && client.closeReason() == 0, "motivo della chiusura perso")
check(client.tick(nowUs: 4_000_000).isEmpty, "il client chiuso manda ancora datagrammi")
check(client.sendKey(nowUs: 4_000_000, code: 4, down: true) == nil, "tastiera dopo la chiusura")
check(client.close(reason: 0) == nil, "seconda chiusura")

print("OK: \(frames.count) frame, \(touchStates) stati del tocco, \(keyEvents) tasti, offset orologi \(offset) µs, bitrate \(hostCounters.targetBitrateBps / 1_000_000) Mbps")
