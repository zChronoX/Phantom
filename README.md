# Phantom

Controllo remoto nativo di un telefono Android da iPhone: bassa latenza in rete locale (QUIC) e accesso 5G/4G ovunque senza VPN (`iroh` P2P), fino a 120 fps, con design monocromatico Liquid Glass Light/Dark.

## Download rapido

Dalla pagina **Releases** di GitHub (note di ogni versione in [CHANGELOG.md](CHANGELOG.md)):
- **iPhone**: scarica `Phantom.ipa` e firmalo con Sideloadly (o AltStore/SideStore).
- **Android**: scarica `Phantom.apk` (`arm64-v8a` + `x86_64`), scegli la modalità e tocca **Collega iPhone · QR**.

## Modalità dell'host Android

| Modalità | Serve | Video e audio | Controllo dall'iPhone |
| --- | --- | --- | --- |
| **Root** | `su` (Magisk/KernelSU) | sì (audio Android 12+) | completo: multi-touch, tastiera, tasti di sistema, schermo spento |
| **Shizuku** | app [Shizuku](https://shizuku.rikka.app/) avviata con il Debug wireless (Android 11+), nessun root | sì (audio Android 12+) | completo, come Root |
| **Standard** | niente (permesso di cattura schermo) | sì (audio Android 10+) | con l'**Accessibilità** di Phantom attiva: tap, swipe, pinch, tasti di sistema e testo (i gesti partono quando si alza il dito); senza, solo visione |

## Release

1. Aggiornare la versione in `android/app/app/build.gradle.kts` (`versionName`), `ios/project.yml`
   (`MARKETING_VERSION`) e aggiungere la sezione in `CHANGELOG.md`.
2. Commit e push su `main`; quando la CI è verde: `git tag -a vX.Y.Z -m "..."` e `git push origin vX.Y.Z`.
3. La CI controlla che tag e versioni coincidano, pubblica `Phantom.apk` e `Phantom.ipa` e usa la sezione del
   changelog come note. Per firmare l'APK sempre con la stessa chiave: `tools/release/create-keystore.ps1` (una volta).

## Funzionalità principali

- **Core Rust ([core/](core/README.md))**: protocollo v0 (`Hello`/`HelloAck` con autenticazione tramite token condiviso 128-bit), video H.264/HEVC con parità XOR + NACK + bitrate adattivo, audio AAC-LC in tempo reale, tocco multi-touch 240 Hz, tastiera HID, appunti bidirezionali e controllo display, trasporto QUIC diretto LAN (`phantom-quic`), trasporto P2P NAT traversal + relay cifrato (`phantom-p2p` su `iroh`) e binding UniFFI Swift/Kotlin (`phantom-ffi`).
- **Host Android ([android/app/](android/app/))**: APK nativo leggero senza Compose/Material (vedi [ADR-010](docs/adr/ADR-010-host-android-apk.md)) con root on-demand (`su` → `app_process64`) + fallback `MediaProjection`, identità persistente + QR 1-tap in modale, doppio listener LAN + P2P `iroh` con preemption istantanea al cambio rete, cattura audio AAC-LC (Android 12+ in modalità Root via `AttributionSource`; Android 10+ in modalità Standard via `AudioPlaybackCapture`; su Android 8/9 come LDPlayer 9 streaming solo video), appunti condivisi, spegnimento display fisico (`ScreenOff`/`ScreenOn`), esenzione Doze, protezione termica (`ThermalGuard`) e **Quick Settings Tile**.
- **Client iPhone ([ios/](ios/))**: app SwiftUI/VideoToolbox full-screen 120 fps con Home dispositivi 1-tap in stile Liquid Glass ([ADR-011](docs/adr/ADR-011-ui-ux-liquid-glass.md)), scanner QR nativo, scoperta Bonjour LAN, switch automatico Wi-Fi ↔ 5G/4G (`NWPathMonitor`), player audio AAC-LC hardware (`AVSampleBufferAudioRenderer`), multi-touch 10 dita, tastiera/gamepad, appunti condivisi e HUD flottante minimizzabile/trascinabile.

## Struttura

```
android/
  app/        app host Android Kotlin leggera (Root, Shizuku, Standard + Accessibilità)
core/         core Rust condiviso: protocollo, video, audio, QUIC LAN, P2P iroh, binding UniFFI
ios/          app client iPhone Swift/SwiftUI (VideoToolbox + AVSampleBufferAudioRenderer)
tools/        preflight, archivio legacy e script di release (tools/release/)
docs/adr/     decisioni architetturali (ADR-001 … ADR-011)
docs/design-system.md  identità visiva, logo e design system Liquid Glass (iOS + Android)
dist/         pacchetti generati (non versionati)
```

La parte legacy della Fase 0 (modulo Magisk, sonda C, strumenti scrcpy) è archiviata sul branch
`archive/legacy-f0` (`tools/archive-legacy.ps1`).

## Dispositivi di riferimento

| Ruolo | Dispositivo |
| --- | --- |
| Host | Redmi Note 11 Pro 5G (`arm64-v8a`, target reale) · LDPlayer 9 (`x86_64`, test Root su PC) |
| Client | iPhone 17 Pro Max |
| Sviluppo | PC Windows, build iOS/Android/Rust in cloud (GitHub Actions) |
