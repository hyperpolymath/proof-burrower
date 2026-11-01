// SPDX-License-Identifier: MPL-2.0
//! Typed *evidence claims* for the interim Agda -> Rocq/Coq and Lean
//! reconciliation process. Validating this document does NOT run a checker,
//! verify a signature or authenticate a CI receipt. Do not treat it as a
//! kernel-level proof; only the cited checker runs can establish that.
use crate::ledger::{goal_hash, new_id, now_iso, Ledger, LedgerRecord, RecordResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const RECONCILIATION_SCHEMA: &str = "proof.reconciliation/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checker {
    Agda,
    Rocq,
    Lean,
}

/// These describe a single leg; they do not replace the estate's closed
/// proof-ledger status vocabulary (proven/argued/open/etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegOutcome {
    Open,
    Unavailable,
    Failed,
    Checked,
    CheckedWithAxioms,
}

impl LegOutcome {
    fn checked(self) -> bool {
        matches!(self, Self::Checked | Self::CheckedWithAxioms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    OriginalAgda,
    Converted,
    Reproved,
    Transported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Correspondence {
    /// Source/target propositions and assumptions were compared.
    Verified,
    /// No comparison established; a transported accepted term is insufficient.
    Unchecked,
    /// Only appropriate for the Agda source leg.
    NotApplicable,
}

/// A claim about a real checker run, rather than a proof-checking operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLeg {
    pub checker: Checker,
    pub route: Route,
    pub outcome: LegOutcome,
    pub artifact_path: Option<String>,
    /// SHA-256 digest of the checked source or witness.
    pub artifact_sha256: Option<String>,
    pub checker_version: Option<String>,
    pub invocation: Option<String>,
    /// Immutable CI run URL, local signed execution receipt, or equivalent.
    pub run_ref: Option<String>,
    pub correspondence: Correspondence,
    #[serde(default)]
    pub axioms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalProof {
    pub language: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofReconciliation {
    pub schema: String,
    pub theorem_id: String,
    pub original: OriginalProof,
    pub agda: CheckLeg,
    pub rocq: CheckLeg,
    pub lean: CheckLeg,
}

fn filled(s: &Option<String>) -> bool {
    s.as_deref().is_some_and(|v| !v.trim().is_empty())
}

fn sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl ProofReconciliation {
    /// Reject contradictory or under-specified evidence metadata.
    ///
    /// A successful validation is NOT a checked proof; callers must still
    /// authenticate the run references and check the referenced artefacts.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.schema != RECONCILIATION_SCHEMA {
            return Err("unsupported reconciliation schema".into());
        }
        if self.theorem_id.trim().is_empty()
            || self.original.language.trim().is_empty()
            || self.original.path.trim().is_empty()
            || !sha256(&self.original.sha256)
        {
            return Err("theorem identity or original proof provenance missing".into());
        }
        for (expected, leg) in [
            (Checker::Agda, &self.agda),
            (Checker::Rocq, &self.rocq),
            (Checker::Lean, &self.lean),
        ] {
            if leg.checker != expected {
                return Err(format!("leg checker mismatch: expected {expected:?}"));
            }
            let source_leg = expected == Checker::Agda;
            let valid_route = if source_leg {
                matches!(
                    leg.route,
                    Route::OriginalAgda | Route::Converted | Route::Reproved
                )
            } else {
                matches!(leg.route, Route::Transported | Route::Reproved)
            };
            if !valid_route {
                return Err(format!("invalid {expected:?} reconciliation route"));
            }
            if leg.outcome.checked() {
                if !filled(&leg.artifact_path)
                    || !leg.artifact_sha256.as_deref().is_some_and(sha256)
                    || !filled(&leg.checker_version)
                    || !filled(&leg.invocation)
                    || !filled(&leg.run_ref)
                {
                    return Err(format!("checked {expected:?} leg lacks run evidence"));
                }
                if !source_leg && leg.correspondence != Correspondence::Verified {
                    return Err(format!(
                        "checked {expected:?} target lacks statement/assumptions comparison"
                    ));
                }
                if source_leg && leg.correspondence != Correspondence::NotApplicable {
                    return Err("Agda source leg must not claim a cross-prover comparison".into());
                }
                if leg.outcome == LegOutcome::Checked && !leg.axioms.is_empty() {
                    return Err(format!("unchecked axioms in {expected:?} no-axiom claim"));
                }
                if leg.outcome == LegOutcome::CheckedWithAxioms && leg.axioms.is_empty() {
                    return Err(format!("{expected:?} axiomatic claim names no axioms"));
                }
                if source_leg {
                    // The canonical policy requires this exact command-line option set.
                    // A shell line is metadata only: it must be backed by run_ref.
                    let words: Vec<&str> = leg
                        .invocation
                        .as_deref()
                        .unwrap_or_default()
                        .split_whitespace()
                        .collect();
                    if words.len() < 5
                        || !words[0].rsplit('/').next().is_some_and(|cmd| cmd == "agda")
                        || words[1..4] != ["--safe", "--without-K", "--cubical-compatible"]
                    {
                        return Err("Agda check missing canonical safe flags".into());
                    }
                }
            } else if leg.correspondence == Correspondence::Verified && !source_leg {
                return Err(format!(
                    "unchecked {expected:?} leg claims verified correspondence"
                ));
            }
        }
        Ok(())
    }
}

/// Append the *claim* to Burrower's existing interim JSONL ledger. The
/// status remains "proposed": an imported receipt is not proof of execution.
/// No second persistent database or new state store is introduced.
pub fn record_reconciliation(
    ledger: &Ledger,
    theorem_statement: &str,
    receipt: &ProofReconciliation,
) -> Result<()> {
    receipt.validate().map_err(anyhow::Error::msg)?;
    ledger.append(&LedgerRecord {
        id: new_id(),
        timestamp: now_iso(),
        goal_hash: goal_hash(theorem_statement),
        goal_id: Some(crate::ids::goal_content_id(theorem_statement)),
        goal_excerpt: theorem_statement.chars().take(200).collect(),
        specialist: "ProofReconciliation".to_string(),
        approach: None,
        result: Some(RecordResult {
            status: "proposed".to_string(),
            explanation:
                "Reconciliation evidence submitted; checker runs not authenticated by Burrower"
                    .into(),
            artifacts: Vec::new(),
        }),
        learning: None,
        extra: serde_json::json!({"proof_reconciliation": receipt}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn checked(checker: Checker, route: Route, invocation: &str) -> CheckLeg {
        CheckLeg {
            checker,
            route,
            outcome: LegOutcome::Checked,
            artifact_path: Some("verification/theorem".into()),
            artifact_sha256: Some(DIGEST.into()),
            checker_version: Some("pinned-1.0".into()),
            invocation: Some(invocation.into()),
            run_ref: Some("https://github.com/hyperpolymath/proof-burrower/actions/runs/1".into()),
            correspondence: if checker == Checker::Agda {
                Correspondence::NotApplicable
            } else {
                Correspondence::Verified
            },
            axioms: Vec::new(),
        }
    }

    fn valid() -> ProofReconciliation {
        ProofReconciliation {
            schema: RECONCILIATION_SCHEMA.into(),
            theorem_id: "theory/example/theorem1".into(),
            original: OriginalProof {
                language: "Idris2".into(),
                path: "verification/theorem.idr".into(),
                sha256: DIGEST.into(),
            },
            agda: checked(
                Checker::Agda,
                Route::Reproved,
                "agda --safe --without-K --cubical-compatible verification/Theorem.agda",
            ),
            rocq: checked(Checker::Rocq, Route::Reproved, "rocq c theorem.v"),
            lean: checked(Checker::Lean, Route::Reproved, "lake env lean Theorem.lean"),
        }
    }

    #[test]
    fn honest_record_roundtrip() {
        let record = valid();
        assert_eq!(record.validate(), Ok(()));
        let roundtrip: ProofReconciliation =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(roundtrip, record);
    }

    #[test]
    fn cannot_promote_uncompared_transport() {
        let mut r = valid();
        r.lean.route = Route::Transported;
        r.lean.correspondence = Correspondence::Unchecked;
        assert!(r.validate().is_err());
    }

    #[test]
    fn cannot_hide_unsafe_agda_invocation() {
        let mut r = valid();
        r.agda.invocation = Some("agda --safe --without-K theorem.agda".into());
        assert!(r.validate().is_err());
    }

    #[test]
    fn cannot_promote_unattributed_result_or_axioms() {
        let mut r = valid();
        r.rocq.run_ref = None;
        assert!(r.validate().is_err());
        r = valid();
        r.lean.axioms.push("classical_choice".into());
        assert!(r.validate().is_err());
        r.lean.outcome = LegOutcome::CheckedWithAxioms;
        assert_eq!(r.validate(), Ok(()));
    }

    #[test]
    fn unavailable_checker_is_not_a_success() {
        let mut r = valid();
        r.rocq.outcome = LegOutcome::Unavailable;
        r.rocq.correspondence = Correspondence::Unchecked;
        assert_eq!(r.validate(), Ok(()));
    }

    #[test]
    fn persistence_never_claims_proven_without_external_check() {
        let tmp = tempdir().unwrap();
        let ledger = Ledger::open(tmp.path().join("ledger.jsonl")).unwrap();
        record_reconciliation(&ledger, "Theorem1", &valid()).unwrap();
        let rows = ledger.read_all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result.as_ref().unwrap().status, "proposed");
        assert_eq!(
            rows[0].extra["proof_reconciliation"]["schema"],
            RECONCILIATION_SCHEMA
        );
    }
}
