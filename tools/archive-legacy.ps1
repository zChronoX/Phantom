# Archivia la parte legacy della Fase 0 su un branch e la toglie da main.
#
# Cosa archivia (resta intatta sul branch `archive/legacy-f0`, recuperabile in ogni momento):
#   - android/module   (modulo Magisk/KernelSU della Fase 0)
#   - android/probe    (sonda di cattura e telemetria)
#   - tools/fetch_scrcpy.sh, tools/embed_html.py, tools/f0_client.py
#   - .github/workflows/android-f0.yml, inspect-scrcpy.yml, inspect-scrcpy-tags.yml
#   - docs/f0-guida.md, docs/f0/
#
# Uso, dalla cartella del progetto, DOPO aver committato e pushato il resto:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/archive-legacy.ps1
$ErrorActionPreference = "Stop"

$dirty = git status --porcelain
if ($dirty) {
    Write-Host "Ci sono modifiche non committate: committa o metti da parte prima di archiviare." -ForegroundColor Yellow
    git status --short
    exit 1
}

$branch = "archive/legacy-f0"
git show-ref --verify --quiet "refs/heads/$branch"
if ($LASTEXITCODE -eq 0) {
    Write-Host "Il branch $branch esiste già: non lo tocco." -ForegroundColor Yellow
} else {
    git branch $branch
    Write-Host "Creato $branch su $(git rev-parse --short HEAD)" -ForegroundColor Green
}
git push origin $branch
if ($LASTEXITCODE -ne 0) { throw "Push del branch di archivio non riuscito: non rimuovo niente da main." }

$paths = @(
    "android/module",
    "android/probe",
    "tools/fetch_scrcpy.sh",
    "tools/embed_html.py",
    "tools/f0_client.py",
    ".github/workflows/android-f0.yml",
    ".github/workflows/inspect-scrcpy.yml",
    ".github/workflows/inspect-scrcpy-tags.yml",
    "docs/f0-guida.md",
    "docs/f0"
)
$existing = $paths | Where-Object { Test-Path $_ }
if (-not $existing) {
    Write-Host "Niente da rimuovere: già archiviato." -ForegroundColor Green
    exit 0
}
git rm -r --quiet -- $existing
git commit -m "chore: archivia la Fase 0 legacy (modulo Magisk, probe, strumenti scrcpy) sul branch $branch"
Write-Host ""
Write-Host "Fatto. Controlla con 'git show --stat HEAD' e poi pubblica con: git push origin main" -ForegroundColor Green
