# Crea il keystore di firma della release Android (una volta sola) e prepara i 4 secret per GitHub Actions.
# Serve Docker Desktop (Java non è necessario sul PC). Uso, dalla cartella del progetto:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/release/create-keystore.ps1
#
# CONSERVA il file .jks e la password fuori dal repository (es. password manager): se li perdi, gli aggiornamenti
# futuri dell'app non si potranno installare sopra quelli vecchi.
param(
    [string]$OutDir = "$env:USERPROFILE\Documents\phantom-release-keys",
    [string]$Alias = "phantom"
)
$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$jks = Join-Path $OutDir "phantom-release.jks"
if (Test-Path $jks) {
    Write-Host "Esiste già $jks: non lo sovrascrivo." -ForegroundColor Yellow
    exit 1
}

$secure = Read-Host "Scegli una password per il keystore (almeno 8 caratteri)" -AsSecureString
$password = [Runtime.InteropServices.Marshal]::PtrToStringAuto([Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
if ($password.Length -lt 8) { throw "Password troppo corta" }

docker run --rm -v "${OutDir}:/keys" eclipse-temurin:17-jdk keytool -genkeypair -v `
    -keystore /keys/phantom-release.jks -alias $Alias -keyalg RSA -keysize 4096 -validity 10000 `
    -storepass $password -keypass $password `
    -dname "CN=Phantom, O=Phantom, C=IT"
if ($LASTEXITCODE -ne 0) { throw "keytool non riuscito (Docker Desktop è avviato?)" }

$b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($jks))
Set-Clipboard -Value $b64

Write-Host ""
Write-Host "Keystore creato: $jks" -ForegroundColor Green
Write-Host "Il contenuto in base64 è negli appunti." -ForegroundColor Green
Write-Host ""
Write-Host "Su GitHub: repository -> Settings -> Secrets and variables -> Actions -> New repository secret"
Write-Host "  PHANTOM_KEYSTORE_BASE64   = (incolla dagli appunti)"
Write-Host "  PHANTOM_KEYSTORE_PASSWORD = la password appena scelta"
Write-Host "  PHANTOM_KEY_ALIAS         = $Alias"
Write-Host "  PHANTOM_KEY_PASSWORD      = la stessa password"
Write-Host ""
Write-Host "Dalla prossima build l'APK sarà firmato sempre con questa chiave: disinstalla una volta la versione attuale,"
Write-Host "poi gli aggiornamenti si installeranno sopra."
