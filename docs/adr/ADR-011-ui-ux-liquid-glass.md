# ADR-011 · Design System Monocromatico Liquid Glass, Identità «Phantom» e Architettura UI

- Stato: accettata
- Data: 2026-11-20

## Contesto

Completate le fondamenta funzionali delle fasi 0–5 (core Rust, QUIC LAN, P2P `iroh` 5G/4G con preemption, autenticazione con token 128-bit, audio AAC-LC, appunti condivisi e controllo display), entrambe le app mostrano ancora un'interfaccia di sviluppo:
- su iOS l'app si chiama `Phantom F0` e apre direttamente sui vecchi banchi diagnostici della fase 0 (`ContentView.swift`);
- su Android `MainActivity.kt` usa colori scuri hardcoded (`#111317`) senza Light Mode, espone il QR fisso a metà schermata insieme ai log tecnici e non offre un Quick Settings Tile.

Servono un'identità coerente e una UI/UX pulita, sobria e professionale ispirata alle app ufficiali Apple e Google, priva degli stereotipi visivi «vibecoded / AI slop».

## Decisione

1. **Identità e Nome**:
   - L'app esce dalla fase alpha/F0 e adotta il nome definitivo **Phantom** (su Android la notifica/titolo interno può indicare *Phantom Host* per chiarezza di ruolo).
   - **Logo (Opzione A · «Ghost Pure»)**: simbolo geometrico minimale di un fantasmino bianco puro su sfondo nero OLED `#000000`, compatibile con le icone Dark/Tinted iOS e le Adaptive/Themed Icons Android. Le alternative **B · Liquid Ghost** (doppia silhouette traslucida) e **C · Phantom Frame** (cornice smartphone con onde) restano documentate in [docs/design-system.md](../design-system.md).
2. **Estetica Monocromatica + Liquid Glass (Light & Dark Mode)**:
   - Palette acromatica ad alto contrasto in stile **Leica / Nothing / Apple Pro** (bianco, nero, grigi di sistema), con supporto pieno a **Light Mode e Dark Mode** su entrambe le piattaforme.
   - Gli unici accenti cromatici sono **semantici di stato**: verde (*Connesso / In LAN / Protetto*), ambra (*Riconnessione / Avviso termico*), rosso (*Ferma / Errore*).
   - Su iOS: 100% **SF Pro** + **SF Symbols** e **Liquid Glass** (`.glassEffect(...)` su iOS 26+ con fallback `.ultraThinMaterial` + bordo speculare `0.5 pt` su iOS 17–18), con HUD in sessione a pillola traslucida collassabile/trascinabile.
   - Su Android: restano validi i vincoli di leggerezza di [ADR-010](ADR-010-host-android-apk.md) (zero Jetpack Compose/Material pesanti), implementando Light & Dark mode nativi, superfici pulite arrotondate con bordi hairline e font di sistema con cifre tabulari.
3. **Separazione tra uso quotidiano e diagnostica**:
   - **iOS**: la Home mostra solo i dispositivi salvati (connessione 1-tap) e il tasto `+` per il QR; *Connessione manuale*, *Loopback* e tutti i banchi Fase 0 vengono spostati in *Impostazioni → Diagnostica*.
   - **Android**: la Home mostra una Hero Card (*Avvia / Ferma* + stato), il QR si apre in un foglio modale dedicato (*Collega iPhone*), i log/certificato stanno in una sezione *Diagnostica* chiusa di default, e viene aggiunto un **Quick Settings Tile** nella tendina Android.

## Conseguenze

- Tutti i dettagli di palette, raggi d'angolo, tipografia, schermate e checklist sono mantenuti in [docs/design-system.md](../design-system.md).
- Nessuna funzionalità diagnostica o test esistente viene eliminata: restano accessibili sotto *Diagnostica* senza ingombrare l'esperienza quotidiana.
