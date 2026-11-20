import Foundation
import Network

/// Osserva in tempo reale i cambi di rete dell'iPhone (Wi-Fi ↔ 5G/4G) tramite `NWPathMonitor`
/// per migrare o ristabilire la connessione verso Android senza attendere timeout.
final class NetworkPathObserver: ObservableObject {
    @Published private(set) var isSatisfied = true
    @Published private(set) var isOnCellularOnly = false
    @Published private(set) var summaryLabel = "Rete attiva"

    var onNetworkChanged: ((_ isCellularOnly: Bool, _ isSatisfied: Bool) -> Void)?

    private var monitor: NWPathMonitor?
    private let queue = DispatchQueue(label: "app.phantom.netpath")
    private let lock = NSLock()
    private var started = false
    private var lastSignature = ""
    private var currentCellularOnly = false

    var cellularOnlySnapshot: Bool {
        lock.lock()
        defer { lock.unlock() }
        return currentCellularOnly
    }

    func start() {
        lock.lock()
        guard !started else {
            lock.unlock()
            return
        }
        started = true
        let newMonitor = NWPathMonitor()
        monitor = newMonitor
        lock.unlock()

        newMonitor.pathUpdateHandler = { [weak self] path in
            guard let self = self else { return }
            let satisfied = path.status == .satisfied
            let hasWifi = path.usesInterfaceType(.wifi) || path.usesInterfaceType(.wiredEthernet)
            let hasCell = path.usesInterfaceType(.cellular)
            let cellOnly = satisfied && hasCell && !hasWifi
            let ifaceNames = path.availableInterfaces.map(\.name).sorted().joined(separator: ",")
            let sig = "\(satisfied)|\(hasWifi)|\(hasCell)|\(ifaceNames)"

            let label: String
            if !satisfied {
                label = "Offline"
            } else if hasWifi {
                label = "Wi-Fi"
            } else if hasCell {
                label = "5G/4G"
            } else {
                label = "Rete attiva"
            }

            self.lock.lock()
            let changed = !self.lastSignature.isEmpty && self.lastSignature != sig
            self.lastSignature = sig
            self.currentCellularOnly = cellOnly
            let cb = self.onNetworkChanged
            self.lock.unlock()

            DispatchQueue.main.async {
                self.isSatisfied = satisfied
                self.isOnCellularOnly = cellOnly
                self.summaryLabel = label
            }

            if changed {
                cb?(cellOnly, satisfied)
            }
        }
        newMonitor.start(queue: queue)
    }

    func stop() {
        lock.lock()
        guard started else {
            lock.unlock()
            return
        }
        started = false
        onNetworkChanged = nil
        let active = monitor
        monitor = nil
        lock.unlock()
        active?.cancel()
    }
}
