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
# The pinned toolchain (scripts/ci/setup-agda.sh) names its libraries file in
# AGDA_LIBRARIES_FILE. Agda reads no such variable, only --library-file, so
# without this a root carrying an .agda-lib (burrow.agda-lib depends on
# standard-library-2.1) resolves against ~/.config/agda/libraries instead.
if [[ -n "${AGDA_LIBRARIES_FILE:-}" ]]; then
  if [[ ! -f "$AGDA_LIBRARIES_FILE" ]]; then
    echo "Agda libraries file unavailable: $AGDA_LIBRARIES_FILE" >&2
    exit 2
  fi
  include_args=(--no-default-libraries --library-file "$(realpath -e -- "$AGDA_LIBRARIES_FILE")")
fi
if [[ -n "${AGDA_LIBRARY_INCLUDE:-}" ]]; then
  if [[ ! -d "$AGDA_LIBRARY_INCLUDE" ]]; then
    echo "Agda library source directory unavailable: $AGDA_LIBRARY_INCLUDE" >&2
    exit 2
  fi
  include_args+=(-i "$(realpath -e -- "$AGDA_LIBRARY_INCLUDE")")
fi
for source in "$@"; do
  if [[ "$source" == -* || "$source" != *.agda || ! -f "$source" ]]; then
    echo "Not a readable Agda source (options forbidden): $source" >&2
    exit 2
  fi
  file="$(realpath -e -- "$source")"
  # Agda checks a module from its include root: module Burrow.Trust in
  # path/to/Burrow/Trust.agda is checked from path/to as Burrow/Trust.agda.
  # Read the module name from the file and walk up one directory per
  # component, so hierarchical modules work as well as flat ones.
  name="$(sed -nE 's/^[[:space:]]*module[[:space:]]+([A-Za-z0-9_.]+)[[:space:]].*/\1/p' "$file" | head -n 1)"
  if [[ -z "$name" ]]; then
    echo "No module declaration in $source" >&2
    exit 2
  fi
  rel="${name//.//}.agda"
  root="${file%/"$rel"}"
  if [[ "$root" == "$file" || "$root/$rel" != "$file" ]]; then
    echo "Module $name does not match its path $source" >&2
    exit 2
  fi
  ( cd -- "$root" &&
    "$checker" --safe --without-K --cubical-compatible \
      "${include_args[@]}" "$rel" )
done
