#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Run the Burrower -> ECHIDNA -> Isabelle accepted/rejected goal contracts.
# This is a real prover integration check; missing prerequisites are failures.
set -euo pipefail
project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
: "${ECHIDNA_BIN:?Set ECHIDNA_BIN to the built ECHIDNA executable}"
test -x "$ECHIDNA_BIN"
command -v isabelle
isabelle version
cargo test --locked -p burrower-core --test live_isabelle -- --ignored --nocapture
