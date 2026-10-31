# ADR-001 · Rust per il core, Kotlin su Android, Swift su iOS

- Stato: accettata
- Data: 2026-10-31

## Contesto

L'obiettivo è una latenza percepita nativa (sotto 40 ms in LAN). Il percorso critico tocca encoder hardware Android, rete, decoder hardware iOS e display a 120 Hz. Protocollo e trasporto devono essere identici sui due lati e sicuri, perché sono esposti alla rete.

## Decisione

- **Rust** per protocollo, trasporto QUIC, correzione errori, crittografia e metriche, compilato per Android e iOS.
- **Kotlin** per il server di cattura Android eseguito come root con `app_process`: le API di cattura senza consenso sono Java nascoste.
- **Swift** per l'app iOS: VideoToolbox, Metal, Audio Unit, GameController, Secure Enclave.

## Alternative scartate

WebRTC (buffer pensati per le videochiamate), web app in Safari (nessun controllo sul decoder), Flutter/React Native/KMP (strato in più tra tocco e rendering), C++ (sicurezza della memoria), Go (pause del GC).

## Conseguenze

Tre toolchain da mantenere; binding Rust con UniFFI e C ABI per il percorso caldo. L'app iOS richiede macOS per la build: senza Mac usiamo build in cloud (vedi ADR-003).
