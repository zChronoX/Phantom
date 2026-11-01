#!/usr/bin/env bash
# Compila l'app iOS e crea un .ipa NON firmato (lo firma Sideloadly con il tuo Apple ID).
# Gira su macOS con Xcode: in cloud su Codemagic, oppure su un Mac in futuro.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME=PhantomF0
OUT=build
rm -rf "$OUT"
mkdir -p "$OUT"

# Core Rust di Phantom: libreria statica per iPhone + binding Swift generati da UniFFI (core/crates/phantom-ffi).
# Va fatto prima di XcodeGen, che include il file Swift generato tra i sorgenti.
RUST="$PWD/$OUT/rust"
mkdir -p "$RUST/swift" "$RUST/lib"
echo "== core Rust"
(
  cd ../core
  rustup target add aarch64-apple-ios
  cargo build -p phantom-ffi --release
  cargo run -q -p uniffi-bindgen --release -- generate --library target/release/libphantom_ffi.dylib --language swift --out-dir "$RUST/swift"
  cargo rustc -p phantom-ffi --release --target aarch64-apple-ios --crate-type staticlib
  cp target/aarch64-apple-ios/release/libphantom_ffi.a "$RUST/lib/"
) > "$OUT/rust.log" 2>&1 || { echo "BUILD DEL CORE RUST FALLITA. Ultime righe del log:"; tail -n 60 "$OUT/rust.log"; exit 1; }
ls -l "$RUST/lib" "$RUST/swift"

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
  CURRENT_PROJECT_VERSION="${BUILD_NUMBER:-1}" \
  build > "$OUT/xcodebuild.log" 2>&1 && XC_STATUS=0 || XC_STATUS=$?

# Errori e avvisi sempre visibili nel log della build.
grep -E "error:|warning:|BUILD (SUCCEEDED|FAILED)" "$OUT/xcodebuild.log" | sort -u | head -n 80 || true

APP_PATH="$OUT/dd/Build/Products/Release-iphoneos/$APP_NAME.app"
# Xcode crea la cartella .app anche quando la compilazione fallisce: si controlla l'eseguibile.
if [ "$XC_STATUS" -ne 0 ] || [ ! -f "$APP_PATH/$APP_NAME" ]; then
  echo "BUILD FALLITA (xcodebuild $XC_STATUS). Ultime righe del log:"
  tail -n 80 "$OUT/xcodebuild.log"
  exit 1
fi
echo "Eseguibile: $(du -h "$APP_PATH/$APP_NAME" | cut -f1) · versione $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Info.plist") ($(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP_PATH/Info.plist"))"

mkdir -p "$OUT/ipa/Payload"
cp -R "$APP_PATH" "$OUT/ipa/Payload/"
(cd "$OUT/ipa" && zip -qry "../$APP_NAME.ipa" Payload)
rm -rf "$OUT/ipa"
ls -l "$OUT/$APP_NAME.ipa"
