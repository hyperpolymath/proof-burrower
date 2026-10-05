<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk> -->
# Proof Burrower

Find the mathematical home of a proof goal.

Proof Burrower searches theorem-prover libraries for the place a stuck lemma
already lives (or nearly lives), reads the goal through a small swarm of
specialists, and can attempt proofs through
[ECHIDNA](https://github.com/hyperpolymath/echidna). Every attempt goes into an
append-only ledger, so the next run starts from what the last one learned.

ECHIDNA is the prover. Burrower is the librarian and the lab notebook.

**Status:** pre-alpha (v0.0.1).

## Pages

* [Using Proof Burrower](Using-Proof-Burrower) — install, index, find, swarm, attempt
* [ECHIDNA Contract](ECHIDNA-Contract) — how Burrower reads `echidna prove` results
* [Maintaining](Maintaining) — CI, releases, wiki sync

The source of this wiki lives in the repository under `docs/wikis/` and is
pushed here with `scripts/sync-wiki.sh`. Edit it there, not here.
