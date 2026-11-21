#!/usr/bin/env bash
# Stampa le note della versione indicata (es. 1.3.0) prese da CHANGELOG.md. Esce con errore se la sezione manca.
set -euo pipefail
version="${1#v}"
file="${2:-CHANGELOG.md}"
notes="$(tr -d '\r' < "$file" | awk -v v="$version" '
  $0 ~ "^## \\[" v "\\]" { on = 1; next }
  on && /^## \[/ { exit }
  on { print }
')"
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
  echo "Nessuna sezione [$version] in $file" >&2
  exit 1
fi
printf '%s\n' "$notes"
