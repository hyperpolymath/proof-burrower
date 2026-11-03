{-# OPTIONS --safe --without-K --cubical-compatible --exact-split --no-sized-types --no-guardedness #-}
-- SPDX-License-Identifier: MPL-2.0
-- SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell (hyperpolymath)
--
-- Burrower's ledger read path, proved.
--
-- `Burrow.Trust` fixes what a receipt and a warrant mean.  This module
-- applies that to the case the implementation actually has to get right:
-- a record read back out of the ledger.  Today `Ledger::read_all`
-- deserialises each line and leaves `extra` as an opaque JSON value, so a
-- stored `extra.receipt` is trusted on sight.  That is latent while
-- Burrower is the only writer and live the moment a second writer exists
-- (ECHIDNA nominating, a VeriSimDB round trip, a hand-edited file).
--
-- `docs/ECHIDNA-INTEGRATION.adoc` §receipt-on-read states the rule in
-- prose.  These are the same statements, checked:
--
--   `read`              a stored record yields a token, never the fact.
--   `promote`           a token becomes a receipt only with a `Soundness`
--                       witness for that record's own warrant — which is
--                       what re-checking the shape and re-deriving
--                       `result_id` amounts to.
--   `no-launder`        there is no function from a stored record to the
--                       fact.  Applying one to the legacy record would
--                       produce `Checked marker`, which is empty.
--
-- `no-launder` is the reason the rule is a rule and not a convention: the
-- laundering path is not merely discouraged, it is uninhabited.

module Burrow.Ledger where

open import Agda.Primitive using (Level; lsuc)
open import Data.Empty using (⊥)
open import Burrow.Trust

-- `Evidence` is a field of `Warrant`, so it needs opening here rather than
-- arriving with the module import.
open Warrant

-- ── A record as the ledger stores it ───────────────────────────────────
--
-- Exactly the two things a JSONL line can carry about trust: the shape of
-- the evidence, and the token.  Notably absent: the fact.  A serialised
-- record cannot contain a proof that Isabelle checked anything; it can
-- only contain echidna's say-so.

record StoredClaim {wℓ : Level} (A : Set) : Set (lsuc wℓ) where
  constructor storedClaim
  field
    warr  : Warrant {wℓ = wℓ} A
    token : Evidence warr

-- Reading a record gives a token, and only a token.
read :
  ∀ {wℓ} {A : Set} →
  StoredClaim {wℓ = wℓ} A → Epi {wℓ = wℓ} A
read (storedClaim w t) = epi w t

-- Promotion is not a property of the record.  It needs a witness for that
-- record's warrant, supplied by the reader — i.e. by re-running the
-- validation against what is actually stored.
promote :
  ∀ {wℓ} {A : Set} {s : StoredClaim {wℓ = wℓ} A} →
  Soundness (StoredClaim.warr s) →
  ReceiptOn (StoredClaim.warr s)
promote {s = storedClaim _ t} snd = receiptOn t snd

-- ── The legacy record ──────────────────────────────────────────────────
--
-- The record a pre-contract echidna produces: the marker printed, the
-- proof was not checked.  `Burrow.Trust` supplies a model of exactly this
-- situation, so what follows is not an assumption about echidna's
-- behaviour but a consequence of the frame.

legacy-stored : StoredClaim (Checked marker)
legacy-stored = storedClaim legacy-warrant legacy-token

-- The legacy record cannot be promoted, whatever it is annotated with.
no-legacy-promotion : ¬ Soundness (StoredClaim.warr legacy-stored)
no-legacy-promotion = no-legacy-soundness

no-legacy-receipt-stored : ¬ ReceiptOn (StoredClaim.warr legacy-stored)
no-legacy-receipt-stored = no-legacy-receipt

-- ── No laundering ──────────────────────────────────────────────────────
--
-- The central result.  A generic function from stored records to facts
-- would turn the legacy record into a proof that the probe was checked.
-- No such function exists, so no amount of annotation, transport or
-- re-serialisation makes an unvalidated read into a receipt.

no-launder : ¬ (∀ {A : Set} → StoredClaim A → A)
no-launder f = not-checked-at-marker (f legacy-stored)

-- Reading alone is always safe, because it commits to nothing.  This is
-- why the fix is on the reader rather than the writer: keep reading every
-- record, but never let an unvalidated one count as verified.
read-is-harmless :
  ∀ {wℓ} {A : Set} →
  StoredClaim {wℓ = wℓ} A → Epi {wℓ = wℓ} A
read-is-harmless = read

-- Demotion stays available at the record level: a promoted record can
-- always be read back as a bare token, losing the witness.  This is the
-- "demote on mismatch" rule — total, lossy, and never silently a success.
demote-stored :
  ∀ {wℓ} {A : Set} {s : StoredClaim {wℓ = wℓ} A} →
  ReceiptOn (StoredClaim.warr s) →
  Epi {wℓ = wℓ} A
demote-stored = demote
