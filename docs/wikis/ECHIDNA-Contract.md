<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk> -->
# ECHIDNA Contract: `echidna.prove.result/1`

`echidna prove ... --output json` prints exactly one JCS-canonical (RFC 8785)
I-JSON (RFC 7493) object on stdout:

```json
{"duration_ms":12,"echidna_version":"2.4.0","goal":"probe.thy","message":"","prover":"Isabelle","schema":"echidna.prove.result/1","status":"verified","trust":{"axioms":[],"confidence":null}}
```

`status` is one of `verified`, `failed`, `error`, `timeout`, `unknown`.

## How Burrower uses it

1. Once per echidna binary, Burrower runs `echidna prove --help`. If `--output`
   is listed it uses the contract; otherwise it uses the old text markers.
2. In contract mode the line must be I-JSON, byte-identical to its JCS form,
   and carry every field. Anything else is a contract violation and the
   attempt fails. It is never re-read as text.
3. `verified` with exit 0 becomes a success with a **receipt** (prover,
   axioms, confidence, echidna version), stored in the ledger record's
   `extra.receipt`. `failed`/`unknown` fail, `timeout` times out, `error` is
   skipped (an infrastructure problem, not a lesson).
4. With an older echidna, a success is recorded as a **warrant**: evidence
   that echidna printed "verified", without the receipt.

The full specification is `docs/ECHIDNA-INTEGRATION.adoc` in the repository.
