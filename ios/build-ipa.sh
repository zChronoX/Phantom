#!/usr/bin/env bash
# Compila l'app iOS e crea un .ipa NON firmato (lo firma Sideloadly con il tuo Apple ID).
# Gira su macOS con Xcode: in cloud su Codemagic, oppure su un Mac in futuro.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME=PhantomF0
OUT=build
rm -rf "$OUT"
mkdir -p "$OUT"

command -v xcodegen >/dev/null || brew install xcodegen
xcodegen generate --spec project.yml

xcodebuild \
  -project "$APP_NAME.xcodeproj" \
  -scheme "$APP_NAME" \
  -configuration Release \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -derivedDataPath "$OUT/dd" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" \
  build 2>&1 | tee "$OUT/xcodebuild.log" | grep -E "error:|warning:|BUILD (SUCCEEDED|FAILED)" || true

APP_PATH="$OUT/dd/Build/Products/Release-iphoneos/$APP_NAME.app"
if [ ! -d "$APP_PATH" ]; then
  echo "Build fallita: vedi $OUT/xcodebuild.log"
  tail -n 60 "$OUT/xcodebuild.log"
  exit 1
fi

mkdir -p "$OUT/ipa/Payload"
cp -R "$APP_PATH" "$OUT/ipa/Payload/"
(cd "$OUT/ipa" && zip -qry "../$APP_NAME.ipa" Payload)
rm -rf "$OUT/ipa"
ls -l "$OUT/$APP_NAME.ipa"
