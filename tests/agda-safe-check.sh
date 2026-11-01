#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
printf 'module Test where\n' > "$tmp/Test.agda"
cat > "$tmp/fake-agda" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --version ]]; then
  echo "Agda version 2.8.0"
else
  printf '%s\n' "$@" > "$AGDA_CAPTURE"
fi
SH
chmod +x "$tmp/fake-agda"
AGDA_BIN="$tmp/fake-agda" AGDA_CAPTURE="$tmp/capture" \
  bash "$root/scripts/agda-safe-check.sh" "$tmp/Test.agda"
mapfile -t args < "$tmp/capture"
[[ "${#args[@]}" -eq 4 && "${args[0]}" == --safe &&
   "${args[1]}" == --without-K && "${args[2]}" == --cubical-compatible &&
   "${args[3]}" == "$tmp/Test.agda" ]]
if AGDA_BIN="$tmp/fake-agda" AGDA_CAPTURE="$tmp/capture" \
  bash "$root/scripts/agda-safe-check.sh" --with-K; then
  echo "unsafe option incorrectly accepted" >&2; exit 1
fi
if AGDA_BIN="$tmp/not-installed" \
  bash "$root/scripts/agda-safe-check.sh" "$tmp/Test.agda"; then
  echo "missing checker incorrectly accepted" >&2; exit 1
fi
echo 'Agda flags positive and negative controls passed'
