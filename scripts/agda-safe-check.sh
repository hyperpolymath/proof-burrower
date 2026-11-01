#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# First-party Agda theory checks must use the exact safety option set.
set -euo pipefail
if (( $# == 0 )); then
  echo "usage: $0 FILE.agda [FILE.agda ...]" >&2
  exit 2
fi
agda_bin="${AGDA_BIN:-agda}"
if ! command -v "$agda_bin" >/dev/null 2>&1; then
  echo "Agda checker unavailable: $agda_bin" >&2
  exit 127
fi
"$agda_bin" --version >&2
for source in "$@"; do
  if [[ "$source" == -* || "$source" != *.agda || ! -f "$source" ]]; then
    echo "Not a readable Agda source (options forbidden): $source" >&2
    exit 2
  fi
  "$agda_bin" --safe --without-K --cubical-compatible "$source"
done
