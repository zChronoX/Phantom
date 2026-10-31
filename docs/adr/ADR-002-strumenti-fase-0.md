# ADR-002 · Strumenti della fase 0 in C statico e modulo root

- Stato: accettata
- Data: 2026-10-31

## Contesto

La fase 0 deve dare risposte sul telefono reale (POCO F4 GT, LineageOS non ufficiale, SukiSU) prima di investire nel sistema vero. Le domande sono: la rete di casa regge? il telefono resta raggiungibile in Doze? cattura ed encoder funzionano su questa ROM? Gli strumenti devono funzionare senza Android Studio, senza Termux e senza PC collegato.

## Decisione

- Una sonda **C11 senza dipendenze**, compilata statica per `aarch64-linux-musl` con `zig cc`. Non usa la libc di Android, quindi gira su qualsiasi versione, ed esegue i programmi di sistema (`screenrecord`, `getprop`) con percorsi assoluti, senza shell.
- Distribuita come **modulo SukiSU/KernelSU** (compatibile Magisk) con avvio al boot, pulsante Azione e WebUI: si installa e si usa solo dal telefono.
- Il video di base usa **`screenrecord`** di sistema: dimostra che la cattura con encoder hardware funziona e fissa una linea di base da battere. Non è la pipeline finale.
- Le misure lato iPhone usano una **pagina web servita dalla sonda** (WebSocket in Safari) finché non esiste l'app.

## Conseguenze

Il codice della sonda è usa e getta: il protocollo vero (QUIC, Rust) arriva in fase 1. La misura WebSocket è su TCP e quindi più pessimista del protocollo UDP finale; il client PC misura invece su UDP. Il server Kotlin con le API nascoste si compila in CI (GitHub Actions ha l'SDK Android preinstallato).
