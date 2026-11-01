#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# First-party Agda theory checks: keep the safety flags and module roots.
set -euo pipefail
if (( $# == 0 )); then
  echo "usage: $0 FILE.agda [FILE.agda ...]" >&2
  exit 2
fi
agda_bin="${AGDA_BIN:-agda}"
checker="$(command -v -- "$agda_bin" || true)"
if [[ -z "$checker" ]]; then
  echo "Agda checker unavailable: $agda_bin" >&2
  exit 127
fi
# Preserve an executable path when checking modules from their own roots.
if [[ "$checker" != /* ]]; then
  checker="$(realpath -e -- "$checker")"
fi
"$checker" --version >&2
include_args=()
if [[ -n "${AGDA_LIBRARY_INCLUDE:-}" ]]; then
  if [[ ! -d "$AGDA_LIBRARY_INCLUDE" ]]; then
    echo "Agda library source directory unavailable: $AGDA_LIBRARY_INCLUDE" >&2
    exit 2
  fi
  include_args=(-i "$(realpath -e -- "$AGDA_LIBRARY_INCLUDE")")
fi
for source in "$@"; do
  if [[ "$source" == -* || "$source" != *.agda || ! -f "$source" ]]; then
    echo "Not a readable Agda source (options forbidden): $source" >&2
    exit 2
  fi
  module="$(realpath -e -- "$source")"
  # Module Properties in path/to/Properties.agda must be checked from
  # path/to, rather than forcing a false module/path mismatch at repo root.
  ( cd -- "$(dirname -- "$module")" &&
    "$checker" --safe --without-K --cubical-compatible \
      "${include_args[@]}" "$(basename -- "$module")" )
done
