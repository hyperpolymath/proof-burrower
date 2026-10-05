#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
#
# Copy the wiki source in docs/wikis/ into a clone of the forge wiki
# (https://github.com/hyperpolymath/proof-burrower.wiki.git) and commit it.
# It never pushes: it prints the push command for a human to run.
#
# Usage: scripts/sync-wiki.sh <path-to-wiki-clone>
set -euo pipefail

# Print usage to stderr and exit with status 2.
usage() {
  echo "usage: $0 <path-to-proof-burrower.wiki-clone>" >&2
  exit 2
}

# Copy every docs/wikis/*.md page except README.md into the wiki clone,
# remove pages that no longer have a source, and commit if anything changed.
sync_wiki() {
  local src=$1 dest=$2 page name
  for page in "$src"/*.md; do
    name=$(basename "$page")
    [ "$name" = "README.md" ] && continue
    cp "$page" "$dest/$name"
  done
  for page in "$dest"/*.md; do
    name=$(basename "$page")
    [ -f "$src/$name" ] || git -C "$dest" rm -q -- "$name"
  done
  git -C "$dest" add -A -- '*.md'
  if git -C "$dest" diff --cached --quiet; then
    echo "wiki already up to date"
    return 0
  fi
  git -C "$dest" commit -q -m "docs(wiki): sync from proof-burrower docs/wikis"
  echo "committed; push with: git -C '$dest' push origin HEAD"
}

[ $# -eq 1 ] || usage
dest=$1
[ -d "$dest/.git" ] || { echo "not a git clone: $dest" >&2; exit 1; }
repo_root=$(cd "$(dirname "$0")/.." && pwd)
sync_wiki "$repo_root/docs/wikis" "$dest"
