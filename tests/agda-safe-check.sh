#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/nested" "$tmp/stdlib"
printf 'module Test where\n' > "$tmp/nested/Test.agda"
cat > "$tmp/fake-agda" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --version ]]; then
  printf 'Agda version 2.8.0\n'
else
  printf '%s\n' "$@" > "$AGDA_CAPTURE"
  pwd > "$AGDA_CWD_CAPTURE"
fi
SH
chmod +x "$tmp/fake-agda"
AGDA_BIN="$tmp/fake-agda" AGDA_CAPTURE="$tmp/capture" \
 AGDA_CWD_CAPTURE="$tmp/cwd" bash "$root/scripts/agda-safe-check.sh" "$tmp/nested/Test.agda"
mapfile -t args < "$tmp/capture"
[[ "${#args[@]}" -eq 4 && "${args[0]}" == --safe ]]
[[ "${args[1]}" == --without-K && "${args[2]}" == --cubical-compatible ]]
[[ "${args[3]}" == Test.agda ]]
[[ "$(cat "$tmp/cwd")" == "$tmp/nested" ]]
AGDA_BIN="$tmp/fake-agda" AGDA_CAPTURE="$tmp/capture" AGDA_CWD_CAPTURE="$tmp/cwd" \
 AGDA_LIBRARY_INCLUDE="$tmp/stdlib" bash "$root/scripts/agda-safe-check.sh" "$tmp/nested/Test.agda"
mapfile -t args < "$tmp/capture"
[[ "${#args[@]}" -eq 6 && "${args[3]}" == -i ]]
[[ "${args[4]}" == "$tmp/stdlib" && "${args[5]}" == Test.agda ]]
if AGDA_BIN="$tmp/fake-agda" AGDA_CAPTURE="$tmp/capture" \
  bash "$root/scripts/agda-safe-check.sh" --with-K; then
  echo "unsafe option was accepted" >&2; exit 1
fi
if AGDA_BIN="$tmp/not-installed" bash "$root/scripts/agda-safe-check.sh" "$tmp/nested/Test.agda"; then
  echo "missing checker was accepted" >&2; exit 1
fi
if AGDA_BIN="$tmp/fake-agda" AGDA_LIBRARY_INCLUDE="$tmp/missing" \
  bash "$root/scripts/agda-safe-check.sh" "$tmp/nested/Test.agda"; then
  echo "missing include directory was accepted" >&2; exit 1
fi
echo 'Agda safe invocation, nested module, include path, and negative controls passed'
