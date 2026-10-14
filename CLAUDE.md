<!--
SPDX-License-Identifier: MPL-2.0
SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell (hyperpolymath)
-->

# CLAUDE.md — proof-burrower

Instructions for AI agent sessions in this repository. Architecture and
status live in `docs/ARCHITECTURE.adoc` and `docs/EXPLAINME.adoc`.

## Owner norms (2026-10-14)

These come from the owner. The format rules point at their normative
source in `hyperpolymath/standards`; read that, not a paraphrase.

### Asking the owner

- Ask decisions with the selector tool (structured options, the
  recommended one first, with what each option implies). Use prose only
  for reports and explanations.
- Work lands as **stacked PRs**: small, each independently reviewable.
- Fix a finding at its source before reaching for a baseline or ledger.
  The governance exemption ratchet rejects ledger growth.

### Formats (standard, estate-wide)

| Kind | Standard | Source |
|------|----------|--------|
| YAML | KYAML, formatted with standards `scripts/kyaml-format.sh` (mikefarah yq v4 with the KYAML encoder). Estate-owned config uses `.kyaml` unless a tool fixes the name. No Python YAML parser. | `3-practice/YAML-POLICY.adoc` §3.4 (D280, D307, D293) |
| Workflows | KYAML too, but only through the step 6 gate in `standards#1025` (Dependabot / `gh actions-lock` drift accepted in writing, lock coverage, `actionlint`, a run proof per workflow). Lockfiles such as `actions.lock` are tool-owned: never hand-edit or reformat them. | `YAML-POLICY.adoc` §5–§6 |
| JSON | I-JSON (RFC 7493) serialised as JCS (RFC 8785), plus exactly one final LF. Extensions do not change. JSONL is one JCS object per line. JSONC files (`devcontainer.json`, `.vscode/`) are exempt. | `3-practice/JSON-POLICY.adoc` (D306) |
| UUID | Current policy: UUIDv7 for records and events, and UUIDv8 over JCS content bytes for content IDs. Keep this split until the corresponding `ids.rs` migration lands. Apply the Profile T/C rule and prohibit new v4/v7 minting after that migration. This repo mints only through `crates/burrower-core/src/ids.rs`. | `docs/decisions/ADR-008-uuid-v8-estate-standard.adoc` (D305, D305b, D320) |

Keep another format only where a consumer the owner does not control
would break (for example a tool that rewrites the file). Record why
beside the file. Tools the owner controls are changed to handle the
standard instead of being exempted.
