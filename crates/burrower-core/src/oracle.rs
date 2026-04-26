// SPDX-License-Identifier: PMPL-1.0-or-later

//! # Pre-proof oracle (safe-learning c)
//!
//! When a goal can be ground-truthed numerically (today: tropical
//! determinant claims), Burrower runs the oracle BEFORE specialists
//! attempt the proof. A `disagree` or `fuzz-counter` verdict means the
//! lemma STATEMENT is suspect — the proof should not be attempted, and
//! the learning entry recorded in the ledger is
//! `pattern_kind = oracle-counter-example`.
//!
//! The oracle itself is the Julia process at
//! `tropical-resource-typing/tools/julia-oracle.jl`, invoked with a
//! single A2ML descriptor argument. We parse the verdict line from
//! stdout — keeping a small surface so adding new oracle families
//! (Kleene-star fixed-point, walks closure, …) only requires adding a
//! new family handler in the Julia side.
//!
//! Why a subprocess: the determinant module is small enough to embed
//! as Rust today, but oracle families will grow (matrix Kleene star,
//! walks closure, max-flow / min-cut duality). Keeping the
//! computational work in Julia means proof-burrower stays prover-agnostic
//! and the math lives next to the rest of the project's Julia helpers.

use crate::ledger::{
    goal_hash, new_id, now_iso, Approach, Learning, Ledger, LedgerRecord, RecordResult,
};
use crate::goal::Goal;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// Oracle invocation parameters supplied by the caller.
#[derive(Debug, Clone)]
pub struct OracleConfig {
    /// Path to `tools/julia-oracle.jl`.
    pub script: PathBuf,
    /// Path to the A2ML descriptor with [oracle-input] and/or
    /// [oracle-fuzz] sections.
    pub descriptor: PathBuf,
    /// Path to the `julia` binary. Defaults to the one on PATH.
    pub julia: PathBuf,
    /// Optional `--project=<dir>` for julia. Required when the oracle
    /// needs the repo's Manifest.toml (Combinatorics dep).
    pub project: Option<PathBuf>,
}

/// Verdict parsed from the oracle's stdout line.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleVerdict {
    /// Oracle agreed with the lemma statement; safe to proceed.
    Agree { detail: String },
    /// Oracle DISAGREED — lemma statement is suspect; do NOT attempt
    /// the proof. Detail carries the (computed, expected) pair.
    Disagree { detail: String },
    /// Oracle's fuzz pass failed; the determinant module itself is
    /// inconsistent. Surface as a hard learning event.
    FuzzCounter { detail: String },
    /// Oracle is self-consistent but says nothing about THIS lemma.
    /// Caller proceeds as if no oracle were available.
    FuzzClean { detail: String },
    /// Oracle could not interpret the descriptor (wrong family,
    /// missing keys). Treat as no-op.
    Inapplicable { reason: String },
    /// Oracle subprocess failed. Surface as a learning event so the
    /// gap is visible (missing julia, missing dep, etc.).
    SubprocessError { reason: String },
}

impl OracleVerdict {
    /// True iff the swarm should SKIP the proof attempt (the lemma is
    /// suspect or the oracle hit a hard failure).
    pub fn blocks_attempt(&self) -> bool {
        matches!(self, OracleVerdict::Disagree { .. } | OracleVerdict::FuzzCounter { .. })
    }

    pub fn pattern_kind(&self) -> &'static str {
        match self {
            OracleVerdict::Disagree { .. } => "oracle-counter-example",
            OracleVerdict::FuzzCounter { .. } => "oracle-fuzz-counter",
            OracleVerdict::Agree { .. } => "oracle-agree",
            OracleVerdict::FuzzClean { .. } => "oracle-fuzz-clean",
            OracleVerdict::Inapplicable { .. } => "oracle-inapplicable",
            OracleVerdict::SubprocessError { .. } => "oracle-subprocess-error",
        }
    }
}

/// Run the oracle subprocess and parse the verdict.
pub fn run(config: &OracleConfig) -> OracleVerdict {
    let _t0 = Instant::now();
    let mut cmd = Command::new(&config.julia);
    if let Some(p) = config.project.as_ref() {
        cmd.arg(format!("--project={}", p.display()));
    }
    cmd.arg(&config.script).arg(&config.descriptor);

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return OracleVerdict::SubprocessError {
                reason: format!("failed to spawn julia: {e}"),
            }
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return OracleVerdict::SubprocessError {
            reason: format!(
                "julia exit {} — stderr tail: {}",
                output.status.code().unwrap_or(-1),
                stderr.lines().last().unwrap_or("").chars().take(160).collect::<String>()
            ),
        };
    }

    parse_verdict(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the oracle's stdout: scan for an `oracle: ...` line and
/// classify it. The first verdict line wins; if more than one stanza
/// produced a verdict, the strongest blocking one takes precedence.
pub fn parse_verdict(stdout: &str) -> OracleVerdict {
    let mut last: Option<OracleVerdict> = None;
    for raw in stdout.lines() {
        let line = raw.trim();
        let payload = if let Some(rest) = line.strip_prefix("oracle:") {
            rest.trim()
        } else {
            continue;
        };

        let verdict = if payload.starts_with("agree") {
            OracleVerdict::Agree { detail: payload.to_string() }
        } else if payload.starts_with("disagree") {
            OracleVerdict::Disagree { detail: payload.to_string() }
        } else if payload.starts_with("fuzz-counter") {
            OracleVerdict::FuzzCounter { detail: payload.to_string() }
        } else if payload.starts_with("fuzz-clean") {
            OracleVerdict::FuzzClean { detail: payload.to_string() }
        } else if payload.starts_with("inapplicable") {
            OracleVerdict::Inapplicable { reason: payload.to_string() }
        } else {
            OracleVerdict::Inapplicable {
                reason: format!("unparsed verdict shape: {payload}"),
            }
        };

        // Promote a blocking verdict over a non-blocking one.
        last = Some(match last {
            None => verdict,
            Some(_) if verdict.blocks_attempt() => verdict,
            Some(prev) => prev,
        });
    }
    last.unwrap_or(OracleVerdict::Inapplicable {
        reason: "no `oracle:` line in stdout".to_string(),
    })
}

/// Append an oracle verdict to the ledger as a Burrower learning event.
pub fn record_to_ledger(
    ledger: &Ledger,
    goal: &Goal,
    config: &OracleConfig,
    verdict: &OracleVerdict,
) {
    let learning = Some(Learning {
        pattern_extracted: format!("oracle-{}", config.descriptor.display()),
        pattern_kind: verdict.pattern_kind().to_string(),
        generalisation: match verdict {
            OracleVerdict::Disagree { detail } => {
                format!("LEMMA SUSPECT: oracle disagreed — {detail}; skip proof attempt.")
            }
            OracleVerdict::FuzzCounter { detail } => {
                format!("ORACLE INCONSISTENT: fuzz pass found counter-example — {detail}.")
            }
            OracleVerdict::Agree { detail } => {
                format!("Oracle ground-truthed lemma statement: {detail}.")
            }
            OracleVerdict::FuzzClean { detail } => {
                format!("Oracle is self-consistent on this family: {detail}.")
            }
            OracleVerdict::Inapplicable { reason } => {
                format!("Oracle had nothing to say: {reason}.")
            }
            OracleVerdict::SubprocessError { reason } => {
                format!("Oracle subprocess failed: {reason}.")
            }
        },
        visible_to: vec![],
    });
    let record = LedgerRecord {
        id: new_id(),
        timestamp: now_iso(),
        goal_hash: goal_hash(&goal.raw),
        goal_excerpt: goal.raw.chars().take(200).collect(),
        specialist: "Oracle".to_string(),
        approach: Some(Approach {
            description: format!(
                "ran julia-oracle.jl on descriptor {}",
                config.descriptor.display()
            ),
            tactics_attempted: vec!["julia-oracle".to_string()],
            preconditions_assumed: vec![],
        }),
        result: Some(RecordResult {
            status: if verdict.blocks_attempt() { "blocked" } else { "advisory" }.to_string(),
            explanation: format!("{:?}", verdict),
            artifacts: vec![],
        }),
        learning,
        extra: Value::Null,
    };
    if let Err(e) = ledger.append(&record) {
        eprintln!("warning: ledger append failed during oracle record: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agree_line() {
        let v = parse_verdict("oracle: agree (family=tropical-determinant-min, computed=5, expected=5)");
        assert!(matches!(v, OracleVerdict::Agree { .. }));
        assert!(!v.blocks_attempt());
    }

    #[test]
    fn parse_disagree_line_blocks_attempt() {
        let v = parse_verdict("oracle: disagree (family=foo, computed=5, expected=7)");
        assert!(matches!(v, OracleVerdict::Disagree { .. }));
        assert!(v.blocks_attempt());
    }

    #[test]
    fn parse_fuzz_counter_blocks_attempt() {
        let v = parse_verdict("oracle: fuzz-counter (family=foo, trials=50, disagreements=3, sample=...)");
        assert!(v.blocks_attempt());
        assert_eq!(v.pattern_kind(), "oracle-fuzz-counter");
    }

    #[test]
    fn empty_stdout_is_inapplicable() {
        let v = parse_verdict("");
        assert!(matches!(v, OracleVerdict::Inapplicable { .. }));
        assert!(!v.blocks_attempt());
    }

    #[test]
    fn blocking_verdict_promoted_over_advisory() {
        // Two stanzas: first agrees, second disagrees. Disagree wins
        // because the blocking verdict is the conservative read.
        let v = parse_verdict(
            "oracle: agree (family=A, computed=1, expected=1)\n\
             oracle: disagree (family=B, computed=2, expected=99)\n",
        );
        assert!(v.blocks_attempt());
    }
}
