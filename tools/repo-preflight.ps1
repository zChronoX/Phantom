# Preflight del repository Phantom, da eseguire prima di qualsiasi modifica.
# PowerShell 5.1 compatibile.
# Exit code: 0 allineato a origin/main; 1 commit locali/remoti da gestire; 2 errore git.

$ErrorActionPreference = "Stop"

Write-Host "== Phantom - preflight del repository =="

$fetchOutput = @(& git fetch --prune origin 2>&1)
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERRORE: git fetch --prune origin non riuscito."
    $fetchOutput | ForEach-Object { Write-Host "  $_" }
    Write-Host "Controlla rete e credenziali prima di lavorare."
    exit 2
}

$null = & git rev-parse --verify --quiet origin/main
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERRORE: origin/main non esiste. Controlla il remoto."
    exit 2
}

$branch = (& git rev-parse --abbrev-ref HEAD).Trim()
$head = (& git log -1 --format="%h %s").Trim()
Write-Host "Branch: $branch"
Write-Host "HEAD:   $head"

$statusLines = @(& git status --short)
if ($statusLines.Count -eq 0) {
    Write-Host "Working tree: pulito"
} else {
    Write-Host "Working tree: $($statusLines.Count) righe da esaminare (non eliminare file non tuoi):"
    $statusLines | ForEach-Object { Write-Host "  $_" }
}

$countLine = (& git rev-list --left-right --count "HEAD...origin/main").Trim()
$parts = @($countLine -split "\s+" | Where-Object { $_ -ne "" })
$ahead = [int]$parts[0]
$behind = [int]$parts[1]
Write-Host "Verso origin/main: avanti $ahead, indietro $behind"

$unmerged = @(& git branch --no-merged origin/main 2>$null)
if ($unmerged.Count -gt 0) {
    Write-Host "Branch con commit non presenti in origin/main:"
    $unmerged | ForEach-Object { Write-Host "  $($_.Trim())" }
}

if ($ahead -eq 0 -and $behind -eq 0) {
    Write-Host "VERDETTO: allineato a origin/main."
    exit 0
}

if ($ahead -eq 0) {
    Write-Host "VERDETTO: indietro di $behind commit."
    Write-Host "Aggiorna solo con working tree pulito: git merge --ff-only origin/main"
    exit 1
}

if ($behind -eq 0) {
    Write-Host "VERDETTO: avanti di $ahead commit locali."
    Write-Host "Non pushare senza richiesta; esamina i commit prima di continuare."
    exit 1
}

Write-Host "VERDETTO: DIVERGENTE (avanti $ahead, indietro $behind)."
Write-Host "STOP: niente rebase, reset o force push."
Write-Host "Confronta i file, conserva il lavoro locale su un branch e chiedi all'utente."
exit 1
