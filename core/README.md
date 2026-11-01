# Phantom · core Rust

Logica condivisa tra host (Android) e client (iPhone). È scritta come **macchine a stati pure**: nessun thread, nessuna lettura dell'orologio, nessun sonno; il tempo è un argomento (`now_us`). Per questo le stesse prove girano in millisecondi e danno sempre lo stesso risultato su Linux, macOS e Windows.

| Crate | Cosa fa |
| --- | --- |
| `phantom-proto` | Formato su filo (datagrammi ≤ 1200 byte), parser senza panici, tocco a stato completo. [ADR-004](../docs/adr/ADR-004-protocollo-v0.md) |
| `phantom-netsim` | Simulatore di rete deterministico: ritardo, jitter FIFO, perdite casuali e a raffica a tempo, riordino, collo di bottiglia, cambi a metà prova. [ADR-005](../docs/adr/ADR-005-simulatore-di-rete.md) |
| `phantom-clock` | Sincronia degli orologi (offset e deriva, stile NTP con scelta dei campioni migliori) e stima del ritardo di coda. |
| `phantom-video` | Frammentazione, parità XOR, NACK con budget, riassemblaggio in ordine, richieste di keyframe ripetute. [ADR-006](../docs/adr/ADR-006-recupero-delle-perdite-video.md) |
| `phantom-core` | `Host` e `Client` che mettono insieme i pezzi, più il controllo del bitrate (`rate`). [ADR-007](../docs/adr/ADR-007-controllo-del-bitrate-v0.md) |

## Comandi

```
cd core
cargo test --workspace --release        # tutto: unitarie, proprietà, simulatore, UDP reale (circa 10 s)
cargo test -p phantom-core --release --test e2e_netsim -- --nocapture   # numeri delle prove sul simulatore
PHANTOM_E2E_TIMELINE=1 cargo test -p phantom-core --release --test e2e_netsim congested -- --nocapture   # secondo per secondo
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```

Le prove su UDP reale (`tests/udp_loopback.rs`) usano soglie larghe di proposito, perché su una macchina di CI sotto carico lo scheduler introduce pause di decine di millisecondi.

## Cosa manca (vedi la roadmap)

Binding UniFFI per Swift e Kotlin, trasporto QUIC (fase 2), cifratura e autenticazione (fase 5), parità Reed-Solomon, controllo di congestione completo (fase 5).
