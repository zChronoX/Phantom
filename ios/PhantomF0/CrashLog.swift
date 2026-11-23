import Foundation
import UIKit

/// Diagnostica dei crash, pensata per un iPhone senza Mac.
///
/// Quando l'app si chiude da sola, iOS non lascia nulla di leggibile dentro l'app. Qui si tiene un piccolo diario su disco
/// («briciole di pane»: cosa stava facendo l'app, una riga alla volta, ciascuna scritta con una sola chiamata di sistema
/// così sopravvive a un crash) e si installano i gestori dei segnali e delle eccezioni, che nel diario scrivono il tipo di
/// errore e i punti del programma. Al lancio successivo l'app mostra il diario dell'ultima volta, da copiare in chat.
///
/// Il diario non contiene nulla di personale: solo nomi di passaggi, numeri e tempi.
enum CrashLog {
    /// Rapporto dell'ultima esecuzione, se è finita male; nil se tutto è andato normalmente.
    private(set) static var previousReport: String?

    /// Da chiamare una sola volta, appena parte l'app.
    static func start() {
        guard gCrashFd < 0 else { return }
        gCrashStartNs = DispatchTime.now().uptimeNanoseconds
        let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let current = dir.appendingPathComponent("crash-current.log")
        let previous = dir.appendingPathComponent("crash-previous.log")
        if let data = try? Data(contentsOf: current), !data.isEmpty {
            let text = String(decoding: data, as: UTF8.self)
            try? data.write(to: previous)
            previousReport = makeReport(from: text)
        }
        gCrashFd = current.path.withCString { open($0, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0o644) }
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
        mark("AVVIO app \(version) (\(build)) · iOS \(UIDevice.current.systemVersion) · \(DecoderBench.machineId()) · memoria \(Int(residentMB())) MB")

        NSSetUncaughtExceptionHandler { exception in
            writeCrashLine("ECCEZIONE \(exception.name.rawValue): \(exception.reason ?? "-")")
            for s in exception.callStackSymbols.prefix(40) { writeCrashLine("  " + s) }
            writeCrashLine("FINE-DEL-RAPPORTO")
        }
        for sig in [SIGABRT, SIGILL, SIGSEGV, SIGBUS, SIGFPE, SIGTRAP] {
            _ = signal(sig, crashSignalHandler)
        }
    }

    /// Una briciola: cosa sta per fare l'app. Si può chiamare da qualsiasi thread.
    static func mark(_ text: String) {
        let secs = Double(DispatchTime.now().uptimeNanoseconds &- gCrashStartNs) / 1e9
        writeCrashLine("+" + String(format: "%.3f", secs) + " " + text)
    }

    /// Come `mark`, con la memoria occupata dall'app (per capire se iOS la chiude per memoria).
    static func markMem(_ text: String) {
        mark(text + " · memoria \(Int(residentMB())) MB")
    }

    /// Chiamata quando l'app esce in modo normale o quando il rapporto è stato letto.
    static func clearPrevious() {
        previousReport = nil
        let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try? FileManager.default.removeItem(at: dir.appendingPathComponent("crash-previous.log"))
    }

    /// Memoria residente dell'app in MB (-1 se non disponibile).
    static func residentMB() -> Double {
        var info = mach_task_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size) / 4
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: 1) {
                task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
            }
        }
        return kr == KERN_SUCCESS ? Double(info.resident_size) / 1_048_576 : -1
    }

    /// Trasforma il diario dell'esecuzione precedente in un rapporto, oppure nil se l'app era finita bene.
    /// «Bene» = nessun errore registrato e ultima fase «background» o «inactive» (ad esempio l'utente l'ha chiusa dal
    /// selettore delle app).
    private static func makeReport(from text: String) -> String? {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: true).map(String.init)
        let crashed = lines.contains { $0.hasPrefix("SEGNALE") || $0.hasPrefix("ECCEZIONE") }
        let lastPhase = lines.last(where: { $0.contains(" FASE ") })
        // La riga è «+12.345 FASE background · memoria 80 MB»: prima si controllava che *finisse* con «background»,
        // e il pezzo sulla memoria faceva sembrare un crash ogni chiusura dalle app recenti.
        // Anche «inactive» conta come uscita normale: chiudendo l'app dal selettore può non fare in tempo a passare a
        // «background». I crash veri restano riconosciuti dalle righe SEGNALE/ECCEZIONE.
        let wentBackground = lastPhase.map { $0.contains("FASE background") || $0.contains("FASE inactive") } ?? false
        if !crashed && wentBackground { return nil }
        let title = crashed
            ? "L'app è stata interrotta da un errore (cerca SEGNALE o ECCEZIONE qui sotto)."
            : "L'app si è chiusa all'improvviso mentre era in primo piano (memoria esaurita, blocco o chiusura forzata)."
        return title + "\n\n" + lines.suffix(80).joined(separator: "\n")
    }
}

// MARK: gestori (devono essere funzioni semplici: un gestore di segnali non può catturare nulla)

private var gCrashFd: Int32 = -1
private var gCrashStartNs: UInt64 = 0

private func writeCrashLine(_ s: String) {
    let fd = gCrashFd
    guard fd >= 0 else { return }
    let line = s + "\n"
    line.withCString { p in _ = write(fd, p, Int(strlen(p))) }
}

private func crashSignalName(_ sig: Int32) -> String {
    switch sig {
    case SIGABRT: return "SIGABRT (abort: asserzione o errore fatale)"
    case SIGILL: return "SIGILL (istruzione non valida)"
    case SIGSEGV: return "SIGSEGV (accesso a memoria non valido)"
    case SIGBUS: return "SIGBUS (accesso a memoria non allineato)"
    case SIGFPE: return "SIGFPE (errore aritmetico)"
    case SIGTRAP: return "SIGTRAP (errore fatale di Swift: indice fuori limiti, valore nullo, overflow, gruppo non bilanciato)"
    default: return "segnale \(sig)"
    }
}

private func crashSignalHandler(_ sig: Int32) {
    writeCrashLine("SEGNALE \(sig) \(crashSignalName(sig)) · thread \(Thread.isMainThread ? "principale" : "secondario") \(Thread.current.name ?? "")")
    for s in Thread.callStackSymbols.prefix(40) { writeCrashLine("  " + s) }
    writeCrashLine("FINE-DEL-RAPPORTO")
    _ = signal(sig, SIG_DFL)
    _ = raise(sig)
}
