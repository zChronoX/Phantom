# Fase 0 · Guida ai test

Obiettivo: verificare sul POCO F4 GT reale cattura ed encoder, rete di casa e raggiungibilità in Doze, prima di scrivere il sistema vero. Tempo richiesto: circa un'ora attiva più un test Doze di 2 ore in cui il telefono resta fermo.

## Cosa serve

- POCO F4 GT con SukiSU; iPhone sulla stessa rete Wi-Fi.
- `dist/phantom-f0.zip` sul telefono Android.
- Facoltativi sul PC Windows: Python 3 (per `tools/f0_client.py`) e FFmpeg (per il video di base).

## 1 · Installazione e report

1. Manager SukiSU → Moduli → Installa → `phantom-f0.zip`.
2. Tocca **Azione** sul modulo: la sonda parte e mostra l'indirizzo IP del telefono; il report finisce in `Download/phantom-f0-report.txt`.
3. Dal riavvio successivo la sonda parte da sola. La **WebUI** del modulo ha tutti i comandi.

## 2 · Rete dall'iPhone

1. In Safari apri `http://<IP del POCO>:47801`.
2. **Test da 2 min** con l'iPhone vicino all'access point del POCO, poi da un'altra stanza o collegato a un altro access point (percorso via powerline).
3. **Copia risultati** dopo ogni test.

## 3 · Rete UDP dal PC (facoltativo)

```
python f0_client.py ping <IP> --seconds 60
python f0_client.py ping <IP> --seconds 60 --size 1200
```

## 4 · Doze

- **Rapido**: WebUI → *Forza Doze*. Dal PC `python f0_client.py doze <IP> --every 60 --hours 0.25`, oppure riapri la pagina in Safari. Poi *Ripristina Doze*.
- **Reale**: telefono scollegato dal caricatore, schermo spento, fermo. Dal PC (che deve restare acceso):
  `python f0_client.py doze <IP> --every 300 --hours 2 --csv doze.csv`.
  Senza PC: dopo almeno un'ora di inattività apri la pagina in Safari e conta quanto ci mette (3 prove).
- Il log della sonda (WebUI → *Log*) scrive per ogni risveglio quanto il telefono aveva dormito.

## 5 · Video di base e misura al rallentatore

1. WebUI → *Accendi video*.
2. Sul POCO apri in Chrome `http://127.0.0.1:47801/clock` (orologio al millisecondo).
3. Sul PC: `python f0_client.py video <IP>` (serve FFmpeg).
4. Filma con l'iPhone al rallentatore (240 fps) lo schermo del POCO e quello del PC insieme. La differenza tra i due orologi è la latenza della linea di base.
5. WebUI → *Spegni video*.

## Cosa mandare

Il file del report, i risultati copiati da Safari, l'output del PC, `doze.csv` e il numero letto dal video al rallentatore.

## Criteri di uscita della prima metà

| Test | Criterio |
| --- | --- |
| Cattura con screenrecord | parte e produce video |
| Encoder | intra-refresh o bassa latenza disponibili |
| Rete, stesso access point | p95 ≤ 15 ms, picchi > 50 ms sotto l'1% |
| Rete via powerline | p95 ≤ 25 ms |
| UDP 1200 byte | perdita < 0,5% |
| Doze forzato e reale | 100% raggiungibile, risposta entro 3 s |
| Video di base | solo misura, è il riferimento da battere |

La seconda metà della fase 0 porta il server di cattura Kotlin a bassa latenza, l'app iOS minima e la catena di build in cloud, con il criterio di ≤ 60 ms in LAN.
