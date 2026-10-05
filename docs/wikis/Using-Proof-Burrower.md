<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk> -->
# Using Proof Burrower

## Build

```sh
cargo build --release   # needs Rust stable (rust-toolchain.toml)
```

The binary is `target/release/burrower`.

## Index a library

```sh
burrower index /path/to/HOL/Library --output idx.json
```

Walks `.thy`, `.v` and `.lean` files and stores lemma signatures.

## Find a home for one goal

```sh
burrower find 'lemma foo: "a + b = b + (a::nat)"' --index idx.json --top 5
```

## Ask the swarm

```sh
burrower swarm 'lemma foo: "a + b = b + (a::nat)"' --index idx.json --ledger burrow.jsonl
```

Each specialist scores its relevance, gives a reading in its own vocabulary,
and proposes homes. The synthesis lists consensus homes and boundary objects.

## Attempt proofs

Needs `echidna` and Isabelle on `PATH` (or `--echidna /path/to/echidna`).

```sh
burrower attempt 'lemma foo: "a + b = b + (a::nat)"' --ledger burrow.jsonl
```

Options: `--timeout`, `--project-root` (goals that import project theories),
`--sandbox none|bwrap|podman`, and `--oracle tools/julia-oracle.jl
--oracle-descriptor d.a2ml` for the optional tropical-types pre-proof oracle.

## Read the ledger

```sh
burrower ledger recent --path burrow.jsonl
burrower ledger digest --path burrow.jsonl
```

A success recorded with a *receipt* came from echidna's structured result; a
success recorded as a *warrant* came from an older echidna's text output. See
[ECHIDNA Contract](ECHIDNA-Contract).

## Serve

```sh
burrower serve --socket /tmp/burrower.sock
```

Line-delimited JSON: `{"cmd":"swarm"|"attempt"|"ledger"|"ping","args":{...}}`.
