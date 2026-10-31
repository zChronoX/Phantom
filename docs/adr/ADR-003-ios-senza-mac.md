# ADR-003 · App iOS sviluppata senza Mac

- Stato: accettata, da rivedere alla fine della fase 2
- Data: 2026-10-31

## Contesto

Lo sviluppatore ha un PC Windows e nessun Mac. Xcode gira solo su macOS. Il budget è zero.

## Decisione

- Codice Swift scritto su Windows; progetto Xcode generato da `project.yml` con XcodeGen, così non si modifica mai un `.xcodeproj` a mano.
- Build su macOS in cloud: Codemagic (500 minuti gratuiti al mese su Mac M2 per account personali) o GitHub Actions. La build produce un `.ipa` non firmato.
- Firma e installazione da Windows con Sideloadly o AltStore e un Apple ID gratuito; validità 7 giorni.
- Debug con telemetria in app e log esportabili al posto di Instruments.

## Conseguenze

Ogni prova sul dispositivo costa 10–15 minuti. Massimo 3 app installate con Apple ID gratuito e niente funzioni che richiedono l'account a pagamento. La logica va spinta nel core Rust, testabile su Windows. Se l'ottimizzazione della latenza in fase 2 diventa troppo lenta, si valuta un Mac mini usato con chip Apple.
