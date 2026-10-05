<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk> -->
# Maintaining Proof Burrower

## Checks that gate `main`

* `rust-ci / Cargo check + clippy + fmt`, `rust-ci / Cargo test`,
  `rust-ci / Cargo audit (security)`, `rust-ci / llvm-cov line coverage`
  (workspace line floor 95%)
* `Burrower proof safety` — builds a pinned echidna and runs `tests/e2e.sh`
  against real Isabelle 2025-2
* secret scans (`scan / *`), CodeQL (`analyze (actions, none)`),
  `openssf-compliance`, `Build Ddraig Pages artifact`

Merge form is squash.

## Moving the echidna pin

`.github/workflows/proof-safety.yml` checks out echidna at a fixed SHA. Once
echidna ships `prove --output json`, move the pin so the live test exercises
the contract path, and confirm `tests/e2e.sh` still passes.

## Wiki

Source: `docs/wikis/*.md` (except `README.md`). Push with:

```sh
scripts/sync-wiki.sh /path/to/proof-burrower.wiki   # a clone of the .wiki.git repo
```

The script copies the pages, commits, and prints the push command; it never
pushes by itself.
