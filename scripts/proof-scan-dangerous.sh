#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Scan first-party proof sources for escape hatches, ignoring comments.
#
# The old grep matched header comments ("no postulate here") and failed on
# main with eight false positives, which drowned any real hit. Comments are
# removed per language before matching; Agda/Idris/Lean pragmas are kept.
#   Agda, Idris 2   -- line, {- -} block (nested), {-# pragma #-} kept
#   Lean 4          -- line, /- -/ block (nested)
#   Rocq/Coq        (* *) block (nested)
# String literals are not lexed. Usage: proof-scan-dangerous.sh [DIR]
set -euo pipefail
dir="${1:-verification/proofs}"
patterns='(^|[^A-Za-z0-9_-])(believe_me|assert_total|postulate|sorry|Admitted|admit|unsafeCoerce|Obj\.magic|primTrustMe)([^A-Za-z0-9_-]|$)|\{-#[[:space:]]*(TERMINATING|NON_TERMINATING|NO_POSITIVITY_CHECK|NON_COVERING)'

# strip FILE OPEN CLOSE LINE: comments removed; LINE empty when none.
strip() {
  awk -v op="$2" -v cl="$3" -v ln_c="$4" '
    { s = $0; out = ""; i = 1; n = length(s)
      while (i <= n) {
        if (depth == 0 && op == "{-" && substr(s, i, 3) == "{-#") { prag = 1 }
        if (prag) { out = out substr(s, i, 1); if (substr(s, i, 3) == "#-}") { out = out "-}"; i += 3; prag = 0; continue } i++; continue }
        if (depth == 0 && ln_c != "" && substr(s, i, length(ln_c)) == ln_c) break
        if (substr(s, i, length(op)) == op) { depth++; i += length(op); continue }
        if (depth > 0 && substr(s, i, length(cl)) == cl) { depth--; i += length(cl); continue }
        if (depth == 0) out = out substr(s, i, 1)
        i++
      }
      print out }' "$1"
}

found=0
while IFS= read -r -d '' f; do
  case "$f" in
    *.agda|*.idr) text=$(strip "$f" '{-' '-}' '--') ;;
    *.lean)       text=$(strip "$f" '/-' '-/' '--') ;;
    *.v)          text=$(strip "$f" '(*' '*)' '') ;;
    *) continue ;;
  esac
  hits=$(grep -nE "$patterns" <<<"$text" || true)
  if [[ -n "$hits" ]]; then
    echo "  DANGEROUS: $f"
    while IFS= read -r h; do echo "    $h"; done <<<"$hits"
    found=$((found + 1))
  fi
done < <(find "$dir" -type f \( -name '*.agda' -o -name '*.idr' -o -name '*.lean' -o -name '*.v' \) -print0 | sort -z)
if [[ "$found" -gt 0 ]]; then
  echo "FAIL: $found file(s) contain escape hatches outside comments"
  exit 1
fi
echo "PASS: no escape hatches outside comments"
