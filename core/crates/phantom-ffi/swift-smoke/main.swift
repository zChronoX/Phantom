// Prova dei binding Swift: host e client del core Rust collegati in memoria, guidati da codice Swift attraverso UniFFI.
// Si compila e si esegue in CI su macOS (vedi .github/workflows/core.yml); non serve un progetto Xcode.
//
// Controlla ciò che conta per l'app: tipi e nomi generati, passaggio di Data e array, sincronia degli orologi,
// video in ordine, tocco a stato completo, datagrammi malformati ignorati, uso da più thread.

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
let fingers = [TouchPoint(id: 3, x: 1000, y: 2000, pressure: 90)]
var firstFrameWasKey: Bool?

// 3 secondi a passi di 1 ms; un frame ogni 8 ms, tocco al ritmo configurato.
for step in 0..<3000 {
    let t = UInt64(step) * 1000
    var out: [(Dest, Data)] = client.tick(nowUs: t).map { (.host, $0) }
    if let datagram = client.touchTick(nowUs: t, fingers: fingers) { out.append((.host, datagram)) }
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
}

check(firstFrameWasKey == true, "all'inizio della sessione l'host chiede un keyframe")
check(frames.count > 330, "frame ricevuti: \(frames.count)")
check(frames[0].key && frames[0].config, "il primo frame deve essere un keyframe con i parametri del codec")
check(frames[0].codec == .h265, "codec del frame")
check(zip(frames, frames.dropFirst()).allSatisfy { $0.frameId < $1.frameId }, "i frame non sono in ordine")
check(frames.allSatisfy { $0.data.count == 40_000 || $0.data.count == 9_000 }, "dimensione dei frame alterata")
check(frames.allSatisfy { $0.data.allSatisfy { $0 == 0xAB } }, "contenuto dei frame alterato")
check(touchStates >= 590, "stati del tocco ricevuti dall'host: \(touchStates)")

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

print("OK: \(frames.count) frame, \(touchStates) stati del tocco, offset orologi \(offset) µs, bitrate \(hostCounters.targetBitrateBps / 1_000_000) Mbps")
