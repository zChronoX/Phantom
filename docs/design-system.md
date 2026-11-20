# Phantom · Design System & Identità Visiva

Documento di riferimento per l'interfaccia utente (iOS e Android) e l'identità visiva di **Phantom**.
Decisione architetturale collegata: [ADR-011](adr/ADR-011-ui-ux-liquid-glass.md).

---

## 1. Filosofia di design (Zero «AI Slop»)

Phantom deve trasmettere la stessa cura di un'applicazione di sistema di prima parte (**Apple** su iOS, **Google / Pixel** su Android) unita al rigore industriale monocromatico di **Leica / Nothing / Apple Pro**.

### Principi guida
1. **Sottrazione prima di tutto**: una sola azione primaria per schermata; tutto ciò che è diagnostico o usato una sola volta (pairing manuale, certificato hex, log, test Fase 0) sta in fogli dedicati o sotto *Impostazioni → Diagnostica*.
2. **Nativo fino al midollo**: componenti, gesti, safe area, Dynamic Type, animazioni a molla e comportamento Light/Dark delle rispettive piattaforme.
3. **Vetro come materia funzionale, non decorazione**: l'effetto **Liquid Glass** serve a far respirare i controlli sopra il contenuto (soprattutto sull'HUD in sessione video), mai a creare contrasti illeggibili.
4. **Chiarezza tipografica**: numeri di telemetria (`fps`, `ms`, `Mbps`, `°C`) sempre in cifre tabulari (`monospacedDigit`) così non oscillano durante l'aggiornamento live.

### Anti-pattern esplicitamente vietati
- Gradienti viola/ciano/blu «neon» o bagliori esterni (`glow` / `box-shadow` colorati).
- Sfondi scuri hardcoded (`#111317`, `#1B1F27`) che ignorano la modalità chiara di sistema.
- Muri di testo descrittivo sotto ogni card, abuso di emoji o troppi badge colorati sulla stessa riga.
- Esposizione fissa del codice QR o dei log grezzi nella schermata principale.

---

## 2. Identità & Logo

- **Nome app**: **Phantom** su iPhone (`CFBundleDisplayName = Phantom`, niente più *Phantom F0*); **Phantom** (o *Phantom Host* nel titolo interno/notifica) su Android.
- **Direzione scelta — Opzione A · «Ghost Pure» (Minimalismo geometrico)**:
  - **Sfondo**: nero OLED puro (`#000000`) per massima integrazione su iOS (incluse icone Dark/Tinted) e Android (Adaptive Icon + monochrome Themed Icon).
  - **Simbolo centrale**: fantasmino bianco puro (`#FFFFFF`) costruito su griglia geometrica pulita (cupola semicircolare perfetta + 2–3 onde morbide simmetriche alla base, proporzioni generose con margine di sicurezza ~22% per squircle iOS e maschere circolari/squircle Android).
  - **Varianti di rifinitura da confrontare**:
    - *A1 · Astratto (senza occhi)*: silhouette pura, massima eleganza minimalista tipo Apple/Nothing.
    - *A2 · Con due tagli/occhi minimali*: due piccoli ovali/tagli neri puliti per renderlo immediatamente riconoscibile anche a 29×29 pt.
- **Varianti alternative archiviate per riferimento futuro**:
  - **Opzione B · «Liquid Ghost»**: doppia silhouette sovrapposta del fantasmino (una bianco ghiaccio piena e una seconda sfalsata semi-traslucida effetto vetro) per evocare la proiezione dello schermo e l'estetica Liquid Glass.
  - **Opzione C · «Phantom Frame»**: profilo esterno arrotondato a cornice di smartphone il cui bordo inferiore si dissolve in 3 onde morbide da fantasma, in bianco su nero.

---

## 3. Palette Monocromatica (Light & Dark Mode)

Look **Leica / Nothing / Apple Pro**: scala acromatica pura + solo 3 colori semantici riservati allo stato reale del sistema.

| Ruolo | Light Mode | Dark Mode | Uso |
| --- | --- | --- | --- |
| **Canvas / Sfondo** | `#F5F5F7` (iOS `systemGroupedBackground`) | `#000000` (OLED puro) | Sfondo principale |
| **Superficie Card** | `#FFFFFF` (vetro chiaro) | `#111113` / `.ultraThinMaterial` | Card dispositivi, fogli, pannelli |
| **Superficie Elevata** | `#EBEBF0` | `#1C1C1E` | Pillole secondarie, controlli inattivi |
| **Bordo Speculare (Hairline)** | `rgba(0,0,0,0.08)` | `rgba(255,255,255,0.14)` (top `0.26` → bottom `0.05`) | Riflesso Liquid Glass `0.5 pt` / `1 dp` |
| **Testo Primario** | `#0A0A0C` | `#F5F5F7` | Titoli, nomi dispositivi, azioni |
| **Testo Secondario** | `#6E6E73` | `#8E8E93` | Indirizzi, sottotitoli, etichette |
| **Azione Primaria (CTA)** | Fondo `#0A0A0C`, testo `#FFFFFF` | Fondo `#FFFFFF`, testo `#0A0A0C` | Pulsanti principali (*Connetti*, *Avvia*, *Scansiona QR*) |
| **Stato Connesso / LAN / Protetto** | `#16A34A` / `Color.green` | `#30D158` | Solo pallino stato live e scudo attivo |
| **Stato Avviso / Riconnessione / Caldo** | `#D97706` / `Color.orange` | `#FF9F0A` | Riconnessione, temperatura alta |
| **Stato Stop / Errore / Critico** | `#DC2626` / `Color.red` | `#FF453A` | *Ferma Host*, errore critico, elimina |

---

## 4. Sistema Liquid Glass & Tipografia

### iOS (`Phantom`)
- **Tipografia**: 100% **SF Pro** nativo (`Font.system(...)`), pesi `.semibold`/`.bold` per gerarchia, `.monospacedDigit()` su metriche e timer. Iconografia esclusivamente **SF Symbols** con peso `.medium` / `.semibold`.
- **Materiali Liquid Glass**:
  - Su **iOS 26+**: `#available(iOS 26.0, *)` con `.glassEffect(.regular.interactive(), in: ...)` e `GlassEffectContainer` per gruppi di pulsanti contigui (es. HUD flottante e pillole controllo).
  - Su **iOS 17–18** (fallback coerente): materiale di sistema `.ultraThinMaterial` / `.regularMaterial` + bordo speculare `0.5 pt` con gradiente direzionale dall'alto verso il basso (`white 0.30 → 0.06` in dark, `white 0.85 → black 0.08` in light) + ombra neutra morbida (`radius: 16`, `y: 6`, `opacity: 0.14`).
- **Raggi d'angolo**: `22 pt` per card principali, `Capsule` (`999 pt`) per pillole, pulsanti primari e HUD collassato, `14 pt` per tasti rapidi interni.

### Android (`Phantom Host`)
- **Leggerezza preservata ([ADR-010](adr/ADR-010-host-android-apk.md))**: zero Jetpack Compose / Material Components pesanti; solo framework Android (`Activity`, `Drawable`, `StateListDrawable`, `TileService`) con un piccolo file `HostTheme.kt` che legge `Configuration.UI_MODE_NIGHT_MASK` per Light & Dark mode autentici.
- **Tipografia**: font di sistema (`sans-serif` / `sans-serif-medium` / `sans-serif-bold`), `tnum` (`fontFeatureSettings = "tnum"`) su FPS/Mbps/temperatura.
- **Superfici «Glass-inspired» leggere**: card arrotondate `24 dp` con fondo semi-traslucido sul canvas neutro e bordo sottile `1 dp` ad alto contrasto delicato (senza appesantire CPU/RAM durante lo streaming 120 fps).

---

## 5. Architettura delle schermate

### 5.1 App iPhone (`Phantom`)
1. **Home · Dispositivi** (entry point principale dell'app, sostituisce la vecchia lista Fase 0):
   - Navigation bar grande nativa **Phantom** + stato rete discreto nel sottotitolo (`Wi-Fi` / `5G`).
   - Toolbar destra: pulsante `+` (apre subito lo **Scanner QR**) e menu `⋯` (*Connessione manuale*, *Impostazioni*, *Diagnostica*).
   - **Empty state** (se nessun dispositivo): fantasmino bianco/nero pulito al centro + titolo essenziale + un solo bottone pillola monocromatico **Scansiona QR da Android**.
   - **Card dispositivo salvato**:
     - Nome dispositivo + pallino verde `In LAN` (se trovato via Bonjour) oppure etichetta discreta `5G / P2P` + icona scudo se protetto da token.
     - Un tap sulla card apre direttamente la sessione full-screen; pressione lunga o tasto `⋯` apre *Modifica* / *Elimina*.
2. **Sessione Remota Full-Screen (HUD Liquid Glass)**:
   - Di default o quando minimizzata: **pillola flottante minuscola Liquid Glass** (stato pallino + `120 fps · 4 ms` + icona menu), **trascinabile** lungo il bordo superiore/laterale così non copre mai UI o tasti di gioco.
   - Espansa (animazione `.spring(response: 0.28, dampingFraction: 0.85)`):
     - Riga 1 (barra Liquid Glass): `Chiudi`, stato live (`LAN · 120 fps · 14.2 Mbps · 4 ms`), `Audio Muto`, `Tastiera`, `Collassa`, `Nascondi`.
     - Riga 2 (tasti Android compatti): `Indietro`, `Home`, `Recenti`, `Vol−`, `Vol+`.
     - Riga 3 (controlli rapidi): `Appunti`, `Schermo Off/On`, `Risveglia`.
   - Toast Liquid Glass in basso centrato (`Appunti ricevuti`, `Schermo spento`, `Audio disattivato`).
3. **Impostazioni & Diagnostica** (sheet / push pulito):
   - Preferenze sessione: *Codec preferito* (HEVC / H.264), *Cerchi tocco*.
   - *Connessione manuale* (IP/DDNS + certificato).
   - *Diagnostica* (chiusa in un sottomenu dedicato): *Loopback QUIC locale*, *CrashLog*, e i vecchi banchi della Fase 0 (*Test UDP*, *Decoder bench*, *Touch bench*, *scrcpy*, *Scatola nera*).

### 5.2 App Android (`Phantom Host`)
1. **Schermata principale essenziale** (Light & Dark mode nativi):
   - Header pulito: **Phantom** + pillola termica/stato (`34.2 °C · Normale`).
   - **Hero Card di stato**:
     - Stato corrente (`Pronto`, `In ascolto · LAN + P2P`, `Connesso · 120 fps · 14 Mbps`).
     - Un grande pulsante pillola primario monocromatico **Avvia Host** (o rosso **Ferma Host** quando attivo).
     - Pulsante secondario pulito **Collega iPhone (QR)** che apre il foglio modale con il QR grande su sfondo bianco puro, fingerprint e campo remoto opzionale.
   - **Card Impostazioni essenziali**:
     - Selettore compatto *Modalità* (`Root` / `Standard`), *Profilo* (`1080p 120` / `1080p 60` / `720p 60`), switch *HEVC* e riga *Esenzione Doze* (solo se non già concessa).
   - **Sezione *Diagnostica* (collassata di default)**:
     - Espandibile con 1 tap: mostra indirizzi LAN/P2P, certificato, pulsante *Copia*, *Nuova chiave* e i log live.
2. **Quick Settings Tile (`PhantomTileService`)**:
   - Tile nativo nella tendina Android (`android.service.quicksettings.TileService`) per avviare/fermare Phantom Host in 1 tap usando le preferenze salvate, mostrando stato *Attivo / Connesso / Fermo*.

---

## 6. Piano di implementazione incrementale

1. **Asset Logo & Icone**: finalizzare la scelta tra *A1 (senza occhi)* e *A2 (occhi minimali)*, generare `AppIcon` iOS (`Assets.xcassets/AppIcon.appiconset` in `ios/project.yml`, inclusa variante Dark/Tinted) e Adaptive Icon + Monochrome Icon Android (`mipmap-anydpi-v26` + vettoriale `ic_launcher_foreground.xml`).
2. **Restyling iOS (`Phantom`)**:
   - Rinominare `CFBundleDisplayName` → `Phantom` in `ios/project.yml`.
   - Creare `LiquidGlassStyle.swift` (modificatori riutilizzabili iOS 26 `.glassEffect` + fallback iOS 17–18, palette monocromatica adattiva).
   - Spostare i vecchi strumenti Fase 0 in `DiagnosticsView.swift` e promuovere la nuova Home dispositivi a root di `PhantomF0App`.
   - Aggiornare `RemoteControlSurface.swift` con HUD Liquid Glass flottante e trascinabile.
3. **Restyling Android (`Phantom Host`)**:
   - Creare `HostTheme.kt` (palette monocromatica Light/Dark adattiva + helper drawable arrotondati/hairline).
   - Rifare `MainActivity.kt` con Hero Card, foglio modale *Collega iPhone (QR)* e pannello *Diagnostica* collassabile.
   - Aggiungere `PhantomTileService.kt` (Quick Settings Tile nella tendina Android) registrato in `AndroidManifest.xml`.
