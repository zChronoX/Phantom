import Foundation

/// Formato del pacchetto della sonda (vedi android/probe/phantom-probe.c), little endian, 32 byte:
///   0  magic "PHP1"
///   4  seq                UInt32
///   8  client_ns          UInt64  orologio del client, rimandato indietro così com'è
///  16  server_mono_ns     UInt64  scritto dalla sonda
///  24  server_suspend_ms  UInt64  millisecondi di sonno del telefono dall'avvio
enum ProbePacket {
    static let headerSize = 32
    static let magic: [UInt8] = Array("PHP1".utf8)

    static func make(seq: UInt32, clientNs: UInt64, size: Int) -> Data {
        var bytes = magic
        bytes += littleEndianBytes(seq)
        bytes += littleEndianBytes(clientNs)
        bytes += littleEndianBytes(UInt64(0))
        bytes += littleEndianBytes(UInt64(0))
        if size > bytes.count {
            bytes += [UInt8](repeating: 0, count: size - bytes.count)
        }
        return Data(bytes)
    }

    struct Reply {
        let seq: UInt32
        let clientNs: UInt64
        let serverSuspendMs: UInt64
    }

    static func parse(_ data: Data) -> Reply? {
        guard data.count >= headerSize else { return nil }
        let start = data.startIndex
        for i in 0..<4 where data[start + i] != magic[i] {
            return nil
        }
        return Reply(
            seq: readLittleEndian(data, offset: 4, as: UInt32.self),
            clientNs: readLittleEndian(data, offset: 8, as: UInt64.self),
            serverSuspendMs: readLittleEndian(data, offset: 24, as: UInt64.self)
        )
    }

    private static func littleEndianBytes<T: FixedWidthInteger>(_ value: T) -> [UInt8] {
        var le = value.littleEndian
        return withUnsafeBytes(of: &le) { Array($0) }
    }

    private static func readLittleEndian<T: FixedWidthInteger>(_ data: Data, offset: Int, as type: T.Type) -> T {
        var value: T = 0
        let start = data.startIndex + offset
        for i in 0..<MemoryLayout<T>.size {
            value |= T(data[start + i]) << (8 * i)
        }
        return value
    }
}
