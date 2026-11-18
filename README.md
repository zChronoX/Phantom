# Phantom

Controllo remoto nativo di un telefono Android con root da iPhone: latenza obiettivo sotto i 40 ms in rete locale, a 120 fps, con il telefono raggiungibile anche bloccato e in Doze.

Il piano completo (obiettivi, architettura, roadmap, test) è nel documento di progetto. Questo repository segue quella struttura.

## Stato

**Fase 0 · Fattibilità** — in corso. Strumenti per verificare sul telefono reale i tre rischi principali: cattura ed encoder, rete di casa, raggiungibilità in Doze. Guida: [docs/f0-guida.md](docs/f0-guida.md).

**Fase 1 · Fondamenta** — iniziata. Il core Rust ([core/](core/README.md)) ha protocollo v0, apertura obbligatoria (`Hello`/`HelloAck`: senza accordo non passano video né input) e chiusura ordinata della sessione, simulatore di rete, sincronia degli orologi, video con parità e NACK, controllo del bitrate, prove sul simulatore e su UDP reale, CI su tre sistemi operativi. Il crate `phantom-quic` porta gli stessi datagrammi su QUIC (fase 2, prima parte: trasporto provato in loopback, non ancora collegato all'app). I binding UniFFI (`phantom-ffi`) espongono host e client a Swift e Kotlin: la CI li genera, ne compila ed esegue una prova per entrambe le lingue e pubblica le librerie per iPhone e Android.

## Struttura

```
android/
  probe/      phantom-probe: sonda C statica (rete, Doze, video di base)
  module/     modulo SukiSU/KernelSU/Magisk "Phantom F0" con WebUI
core/         core Rust condiviso: protocollo, simulatore di rete, orologi, video, bitrate (fase 1)
ios/          app Swift (dalla fase 0, seconda metà)
tools/        client di test per PC e utilità di build
docs/adr/     decisioni architetturali
dist/         pacchetti generati (non versionati)
```

## Build della fase 0

Serve solo `zig` come cross-compilatore (`pip install ziglang`) e `zip`.

```
android/module/build.sh      # → dist/phantom-f0.zip
```

## Dispositivi di riferimento

| Ruolo | Dispositivo |
| --- | --- |
| Host | POCO F4 GT (ingres), Snapdragon 8 Gen 1, LineageOS non ufficiale, root SukiSU |
| Client | iPhone 17 Pro Max |
| Sviluppo | PC Windows, build iOS in cloud |
