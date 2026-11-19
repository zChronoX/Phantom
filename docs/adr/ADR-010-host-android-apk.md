# ADR-010 · Host Android come APK normale con root on-demand (stile FKM)

- Stato: accettata
- Data: 2026-11-19

## Contesto

Nella fase 0 la sonda C e il relay `scrcpy` giravano come modulo SukiSU/Magisk (`android/module/`) avviato al boot. Questo approccio esponeva a due rischi concreti:
- avvio automatico in background con possibili proprietà persistenti (`system.prop`, modifiche a refresh rate o Doze);
- nessuna visibilità immediata nella barra di sistema o controllo diretto da parte dell'utente.

Dopo il guasto fisico del display del POCO F4 GT, lo sviluppo prosegue su **Redmi Note 11 Pro 5G** (`arm64-v8a`, Snapdragon 695, schermo 1080×2400 120 Hz). Servono controllo trasparente, leggerezza su CPU/RAM di fascia media, nessuna modifica persistente al sistema e protezione termica attiva.

## Decisione

- **APK normale (`android/app/`, `app.phantom.host`)** al posto del modulo Magisk/SukiSU per il server di produzione. Nessun script al boot, nessuna modifica a `/system`, `system.prop`, `min_refresh_rate`, `peak_refresh_rate` o stati Doze/batteria.
- **Zero dipendenze UI pesanti**: niente Jetpack Compose, AppCompat o Material. L'interfaccia e il servizio usano direttamente le API del framework Android (`Activity`, `Service`, `Notification`, `MediaCodec`, `MediaProjection`, `PowerManager`) più `net.java.dev.jna:jna` per i binding UniFFI (`uniffi.phantom_ffi`).
- **Doppia modalità di cattura on-demand**:
  - **Root (`su` → `app_process`)**: quando l'utente preme *Avvia Host*, `HostForegroundService` lancia `app_process` dallo stesso APK (`CLASSPATH = sourceDir`, `java.library.path` e `jna.boot.library.path` su `nativeLibraryDir` con `extractNativeLibs="true"`). Esegue `RootHostMain` finché resta attivo il servizio e si ferma pulito sullo *Stop* (anche dalla notifica persistente). Offre cattura schermo senza popup e iniezione reale multi-touch/tastiera.
  - **MediaProjection (senza root)**: fallback sicuro tramite consenso di sistema per testare video + QUIC anche senza privilegi `su` (solo video, nessun input iniettato).
- **Profilo predefinito e sicurezza `MediaCodec`**: default **1080p · 120 fps** (con scelte rapide 1080p/60 e 720p/60), parametri `MediaCodec` neutri senza flag real-time aggressivi e fallback automatico 120 → 60 fps e HEVC → H.264 se l'encoder hardware li rifiuta.
- **Protezione termica attiva (`ThermalGuard`)**: controllo continuo di temperatura batteria e `PowerManager.currentThermalStatus`, con riduzione graduale del bitrate sopra i 41 °C / 44 °C e arresto automatico di sicurezza a ≥ 47 °C o stato termico `CRITICAL`.

## Conseguenze

- `android/module/` e `android/probe/` restano invariati come archivio storico della fase 0.
- Il binding Kotlin e `libphantom_ffi.so` vengono generati automaticamente in CI nel workflow `.github/workflows/android-app.yml`, che esegue gli unit test JVM e pubblica `PhantomHost-debug.apk`.
- L'APK disinstallato o chiuso non lascia alcun processo né modifica di sistema sul telefono.
