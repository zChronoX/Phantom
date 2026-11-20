# Phantom

Controllo remoto nativo di un telefono Android da iPhone: bassa latenza in rete locale (QUIC) e accesso 5G/4G ovunque senza VPN (`iroh` P2P), fino a 120 fps, con design monocromatico Liquid Glass Light/Dark.

## Download rapido (v1.0.0)

Dalla pagina **Releases** di GitHub:
- **iPhone**: scarica `Phantom.ipa` e firmalo con Sideloadly (o AltStore/SideStore).
- **Android**: scarica `Phantom.apk` (`arm64-v8a` + `x86_64`), avvia l'host in modalità **Root** (consigliata) o **Standard** (`MediaProjection`) e tocca **Collega iPhone · QR**.

## Funzionalità principali

- **Core Rust ([core/](core/README.md))**: protocollo v0 (`Hello`/`HelloAck` con autenticazione tramite token condiviso 128-bit), video H.264/HEVC con parità XOR + NACK + bitrate adattivo, audio AAC-LC in tempo reale, tocco multi-touch 240 Hz, tastiera HID, appunti bidirezionali e controllo display, trasporto QUIC diretto LAN (`phantom-quic`), trasporto P2P NAT traversal + relay cifrato (`phantom-p2p` su `iroh`) e binding UniFFI Swift/Kotlin (`phantom-ffi`).
- **Host Android ([android/app/](android/app/))**: APK nativo leggero senza Compose/Material (vedi [ADR-010](docs/adr/ADR-010-host-android-apk.md)) con root on-demand (`su` → `app_process64`) + fallback `MediaProjection`, identità persistente + QR 1-tap in modale, doppio listener LAN + P2P `iroh` con preemption istantanea al cambio rete, cattura audio AAC-LC (Android 12+ in modalità Root via `AttributionSource`; Android 10+ in modalità Standard via `AudioPlaybackCapture`; su Android 8/9 come LDPlayer 9 streaming solo video), appunti condivisi, spegnimento display fisico (`ScreenOff`/`ScreenOn`), esenzione Doze, protezione termica (`ThermalGuard`) e **Quick Settings Tile**.
- **Client iPhone ([ios/](ios/))**: app SwiftUI/VideoToolbox full-screen 120 fps con Home dispositivi 1-tap in stile Liquid Glass ([ADR-011](docs/adr/ADR-011-ui-ux-liquid-glass.md)), scanner QR nativo, scoperta Bonjour LAN, switch automatico Wi-Fi ↔ 5G/4G (`NWPathMonitor`), player audio AAC-LC hardware (`AVSampleBufferAudioRenderer`), multi-touch 10 dita, tastiera/gamepad, appunti condivisi e HUD flottante minimizzabile/trascinabile.

## Struttura

```
android/
  app/        app host Android Kotlin leggera (Root on-demand + MediaProjection)
  probe/      archivio fase 0: sonda C statica
  module/     archivio fase 0: modulo SukiSU/KernelSU/Magisk con WebUI
core/         core Rust condiviso: protocollo, video, audio, QUIC LAN, P2P iroh, binding UniFFI
ios/          app client iPhone Swift/SwiftUI (VideoToolbox + AVSampleBufferAudioRenderer)
tools/        client di test per PC e utilità di build/preflight
docs/adr/     decisioni architetturali (ADR-001 … ADR-011)
docs/design-system.md  identità visiva, logo e design system Liquid Glass (iOS + Android)
dist/         pacchetti generati (non versionati)
```

## Dispositivi di riferimento

| Ruolo | Dispositivo |
| --- | --- |
| Host | Redmi Note 11 Pro 5G (`arm64-v8a`, target reale) · LDPlayer 9 (`x86_64`, test Root su PC) |
| Client | iPhone 17 Pro Max |
| Sviluppo | PC Windows, build iOS/Android/Rust in cloud (GitHub Actions) |
