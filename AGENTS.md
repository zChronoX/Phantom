# Istruzioni del progetto Phantom

Valgono anche le regole globali in `~/.config/opencode/AGENTS.md`. Questo file
aggiunge solo le regole specifiche del repository.

## Prima di qualsiasi lavoro: preflight obbligatorio

Eseguire `tools/repo-preflight.ps1` (oppure i comandi git equivalenti) e leggere
l'output. Non modificare file prima di aver capito lo stato del repository.

1. `git fetch --prune origin`
2. `git status --short` — esaminare ogni riga; non eliminare modifiche non proprie.
3. `git log -1 --oneline` e `git branch -vv`
4. `git rev-list --left-right --count HEAD...origin/main`

Interpretazione:

- `0 0`: allineato a `origin/main`, si può iniziare.
- `0 N`: indietro di N commit. Aggiornare con `git merge --ff-only origin/main`
  solo se il working tree è pulito; altrimenti chiedere all'utente.
- `N 0`: N commit locali non pushati. Non pushare senza richiesta; esaminarli.
- `N M`: divergente. **STOP**: niente rebase, reset o force push. Confrontare
  commit e file, conservare il lavoro locale su un branch e chiedere all'utente.

Poi leggere, nell'ordine:

1. `HANDOFF.md` e `PROGRESS.md` per lo stato e il prossimo passo.
2. `README.md` e i documenti/ADR della parte da toccare.
3. I test della parte da toccare, ed eseguirli prima e dopo le modifiche.

## Regole di continuità

- `origin/main` è l'unica fonte di verità. Un `PLAN.md`, un handoff o un piano
  locale che non corrisponde al remoto è superato: non seguirlo.
- Non scartare lavoro locale. Se è duplicato o superato, conservarlo su un branch
  `archive/...` e chiedere prima di abbandonarlo.
- `archive/f0-avsamplebuffer` è un branch locale di archivio: non pushare e non
  cancellare.
- A fine sessione aggiornare `HANDOFF.md` (stato, prossimo passo, verifica) e
  `PROGRESS.md` (stato e decisioni).
- Non committare né pushare senza richiesta esplicita.
- Su Windows il checkout converte gli script in CRLF: gli script bash del repo
  vanno eseguiti in Docker normalizzando i fine riga (`tr -d '\r'`).
