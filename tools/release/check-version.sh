#!/usr/bin/env bash
# Controlla che il tag (es. v1.3.0) coincida con le versioni di Android, iOS e CHANGELOG.md.
set -euo pipefail
tag="${1:?uso: check-version.sh v1.2.3}"
version="${tag#v}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
android="$(tr -d '\r' < "$root/android/app/app/build.gradle.kts" | sed -n 's/.*versionName = "\(.*\)".*/\1/p' | head -1)"
ios="$(tr -d '\r' < "$root/ios/project.yml" | sed -n 's/.*MARKETING_VERSION: "\(.*\)".*/\1/p' | head -1)"
status=0
[ "$android" = "$version" ] || { echo "::error::versionName Android $android diverso dal tag $tag"; status=1; }
[ "$ios" = "$version" ] || { echo "::error::MARKETING_VERSION iOS $ios diverso dal tag $tag"; status=1; }
bash "$root/tools/release/changelog-section.sh" "$version" "$root/CHANGELOG.md" > /dev/null || { echo "::error::manca la sezione [$version] in CHANGELOG.md"; status=1; }
[ "$status" -eq 0 ] && echo "Versione $version coerente (Android, iOS, CHANGELOG)"
exit "$status"
