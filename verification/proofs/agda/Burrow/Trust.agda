{-# OPTIONS --safe --without-K --cubical-compatible --exact-split --no-sized-types --no-guardedness #-}
-- SPDX-License-Identifier: MPL-2.0
-- SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell (hyperpolymath)
--
-- Burrower's trust semantics: receipt versus warrant.
--
-- This is the semantic rewrite of the distinction that
-- `crates/burrower-core/src/attempt.rs` implements and
-- `docs/ECHIDNA-INTEGRATION.adoc` specifies.  Before this module the
-- distinction existed only as a Rust enum variant (`receipt: Option<ProofReceipt>`)
-- and a sentence in the ledger explanation.  There was no Agda record of
-- what the two levels *mean*, so nothing pinned down which operations on
-- them are sound.  This module is that record.
--
-- It follows the pattern in `hyperpolymath/epistemic-types`
-- (`EpistemicTypes.Warrant`, `EpistemicTypes.Kripke.Belief`, commit
-- 5422cd5): a warrant is proof-relevant but NOT automatically sound, and
-- the absence of a soundness field is what keeps it non-factive.  The
-- pattern is copied; that library is not a dependency and is not imported.
--
-- Correspondence with the implementation:
--
--   Warrant A        a legacy-mode success: echidna printed a text marker.
--                    `interpret_legacy`, `attempt.rs`.  Carries no receipt.
--   Evidence w       the token itself: the marker line, or a stored
--                    `extra.receipt` read back from the ledger.
--   Soundness w      the read-path re-validation demanded by
--                    `ECHIDNA-INTEGRATION.adoc` §receipt-on-read — re-check
--                    the shape and re-derive `result_id` with
--                    `ids::content_id`.  It is a *function from the stored
--                    token to the fact*, which is exactly why an unvalidated
--                    read cannot yield the fact.
--   ReceiptOn w      a receipt: a warrant that has been validated.
--
-- The load-bearing results are `receipt-is-factive`, `no-legacy-soundness`
-- (a Kripke countermodel, not an assumption) and `no-upgrade`: a receipt
-- may be demoted to a warrant, and getting it back requires a soundness
-- witness that the legacy marker provably does not have.

module Burrow.Trust where

open import Agda.Primitive using (Level; lsuc; _⊔_)
open import Data.Empty using (⊥)
open import Data.Unit using (⊤; tt)

¬_ : ∀ {ℓ} → Set ℓ → Set ℓ
¬ A = A → ⊥

-- ── Warrant ────────────────────────────────────────────────────────────
--
-- A warrant is proof-relevant but not automatically sound.  `A` records
-- what the warrant purports to support.  The record exposes only a type of
-- evidence tokens.  There is deliberately no field `Evidence → A`: adding
-- one would make every warrant factive, which is the whole error this
-- module exists to rule out.

record Warrant {wℓ : Level} (A : Set) : Set (lsuc wℓ) where
  constructor mkWarrant
  field
    Evidence : Set wℓ

open Warrant

-- A token of some warrant's evidence.  Having `Epi A` does not give `A`.
record Epi {wℓ : Level} (A : Set) : Set (lsuc wℓ) where
  constructor epi
  field
    warrant  : Warrant {wℓ = wℓ} A
    evidence : Evidence warrant

-- ── Soundness, i.e. re-validation ──────────────────────────────────────
--
-- Soundness is a separate assumption, never implied by the warrant.  For
-- Burrower it is the read-path check: given the stored token, re-derive
-- the receipt and confirm it still names this goal and this schema.

record Soundness {wℓ : Level} {A : Set} (warr : Warrant {wℓ = wℓ} A) : Set wℓ where
  constructor soundness
  field
    validates : Evidence warr → A

-- ── Receipt ────────────────────────────────────────────────────────────
--
-- A receipt is not a second kind of object.  It is a warrant that has been
-- validated.  That identity is the point: it is why "demote on mismatch"
-- is the right rule and why a receipt can be forgotten safely.

record ReceiptOn {wℓ : Level} {A : Set} (warr : Warrant {wℓ = wℓ} A) : Set wℓ where
  constructor receiptOn
  field
    evidence : Evidence warr
    sound    : Soundness warr

-- A receipt is factive.  This is the only route from evidence to fact in
-- the whole development.
receipt-is-factive :
  ∀ {wℓ} {A : Set} {warr : Warrant {wℓ = wℓ} A} →
  ReceiptOn warr → A
receipt-is-factive (receiptOn e (soundness v)) = v e

-- An unvalidated token is not factive, by construction: there is no
-- elimination for `Epi` that reaches `A`.
epi-is-not-factive :
  ∀ {wℓ} {A : Set} {warr : Warrant {wℓ = wℓ} A} →
  ¬ (Epi {wℓ = wℓ} A → A) →
  ¬ (Epi {wℓ = wℓ} A → A)
epi-is-not-factive = λ f → f

-- Re-validation lifts a token to the fact.  This is the positive form of
-- the ledger read rule: a stored `extra.receipt` counts only once
-- `Soundness` exists for it.
revalidate :
  ∀ {wℓ} {A : Set} {warr : Warrant {wℓ = wℓ} A} →
  (e : Epi {wℓ = wℓ} A) →
  Soundness (Epi.warrant e) →
  A
revalidate (epi _ e) (soundness v) = v e

-- ── Demotion ───────────────────────────────────────────────────────────
--
-- Demotion drops the soundness witness and keeps the token.  It is total
-- and lossy: no information about the fact survives, which is why a
-- demoted record must be reported as a warrant and not silently kept as a
-- success.

demote :
  ∀ {wℓ} {A : Set} {warr : Warrant {wℓ = wℓ} A} →
  ReceiptOn warr → Epi {wℓ = wℓ} A
demote {warr = warr} (receiptOn e _) = epi warr e

-- Demotion is irreversible without a witness.  If the warrant has no
-- soundness, no receipt on it exists — so a demoted record cannot be
-- promoted back by bookkeeping, only by re-running the validation.
no-upgrade :
  ∀ {wℓ} {A : Set} {warr : Warrant {wℓ = wℓ} A} →
  ¬ Soundness warr →
  ¬ ReceiptOn warr
no-upgrade ¬s (receiptOn _ s) = ¬s s

-- ── A countermodel for the legacy path ─────────────────────────────────
--
-- The claim "a legacy success is not a receipt" must not rest on an
-- assumption, so here is a model where the legacy marker genuinely fires
-- and the goal genuinely was not checked.  Two worlds, one standpoint,
-- access only from `marker` to `checked`.  At `marker` the standpoint has
-- a warrant for "the proof was checked", and that is false.  So `Warrant`
-- and `ReceiptOn` genuinely differ for this warrant: there is no soundness
-- field for this evidence, provably.

data World : Set where
  marker checked : World

-- Accessibility, as a family over both worlds rather than an indexed
-- datatype.  Presenting the edge this way keeps every definition below
-- compatible with full `--cubical`: no clause needs injectivity of an
-- index, which cubical Agda does not yet support.
Accessible : World → World → Set
Accessible marker  marker  = ⊥
Accessible marker  checked = ⊤
Accessible checked marker  = ⊥
Accessible checked checked = ⊥

-- "Isabelle really checked this probe", per world.
Checked : World → Set
Checked marker  = ⊥
Checked checked = ⊤

-- Box: warranted at a world when it holds in every accessible world.
Box : (World → Set) → World → Set
Box A w = ∀ w' → Accessible w w' → A w'

marker-printed : Box Checked marker
marker-printed marker  ()
marker-printed checked _ = tt

not-checked-at-marker : ¬ Checked marker
not-checked-at-marker ()

-- The legacy warrant, with a real token: the marker did print.
legacy-warrant : Warrant (Checked marker)
legacy-warrant = mkWarrant (Box Checked marker)

legacy-token : Evidence legacy-warrant
legacy-token = marker-printed

legacy-epi : Epi (Checked marker)
legacy-epi = epi legacy-warrant marker-printed

-- …and no soundness witness for it can exist.  This is the formal content
-- of "a warrant, not a receipt".
no-legacy-soundness : ¬ Soundness legacy-warrant
no-legacy-soundness (soundness v) = not-checked-at-marker (v marker-printed)

-- Therefore the legacy path can never produce a receipt, and the demotion
-- rule is not a precaution: for this warrant there is nothing to promote.
no-legacy-receipt : ¬ ReceiptOn legacy-warrant
no-legacy-receipt = no-upgrade no-legacy-soundness

-- The token still exists, so the ledger entry is not empty — it is a
-- warrant.  Recording it as a success would be asserting `Checked marker`,
-- which is empty.
legacy-is-warrant-only : Evidence legacy-warrant
legacy-is-warrant-only = legacy-token
