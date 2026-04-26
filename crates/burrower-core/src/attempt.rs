// SPDX-License-Identifier: PMPL-1.0-or-later

//! # Proof attempts
//!
//! Specialists don't just route — they *attempt*. Each specialist
//! carries a domain-specific [`Playbook`] of tactics it knows about.
//! When the swarm runs in `attempt` mode, every engaged specialist
//! tries each tactic in its playbook against the goal, the outcome
//! is recorded in the [`Ledger`], and the next swarm run can build
//! on or learn from the result.
//!
//! ## Pipeline
//!
//! 1. [`generate_probe`] wraps a goal + a candidate tactic into a
//!    self-contained Isabelle probe file.
//! 2. [`run_probe`] invokes `echidna prove --prover Isabelle` as a
//!    subprocess and parses the output.
//! 3. The result is recorded as a [`LedgerRecord`] with structured
//!    `Approach`, `Result`, and (if a clear lesson is extractable)
//!    `Learning` blocks.
//!
//! ## Today's limits
//!
//! - Only Isabelle. Lean / Coq dispatch lands in v0.2.
//! - Probes assume `imports Main` — goals referencing external
//!   theories will fail with "undefined" errors. That is itself
//!   useful data (the ledger records "failed: needs import X").
//! - No timeout enforcement beyond what `echidna prove -t` honours.

use crate::goal::Goal;
use crate::ledger::{
    goal_hash, new_id, now_iso, Approach, Learning, Ledger, LedgerRecord, RecordResult,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

/// One tactic that a specialist knows how to try.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TacticTemplate {
    /// Short name for ledger reporting (e.g. "simp", "induction-finite").
    pub name: String,
    /// The Isabelle proof script to substitute, e.g. `"by simp"` or
    /// `"by (induction rule: finite_induct) auto"`.
    pub script: String,
    /// One-line description of what the tactic does. Human-facing.
    pub description: String,
}

/// A specialist's playbook: ordered tactics to try.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Playbook {
    pub specialist: String,
    pub tactics: Vec<TacticTemplate>,
}

/// Configuration for prover-backend invocation.
#[derive(Debug, Clone)]
pub struct ProverConfig {
    /// Absolute path to the `echidna` binary.
    pub echidna_path: PathBuf,
    /// Per-attempt timeout (passed to `echidna prove -t`).
    pub timeout_secs: u32,
    /// Workdir for probe files. Defaults to `/tmp` if unset.
    pub workdir: Option<PathBuf>,
}

impl Default for ProverConfig {
    fn default() -> Self {
        Self {
            echidna_path: PathBuf::from("echidna"),
            timeout_secs: 60,
            workdir: None,
        }
    }
}

/// Outcome of one proof attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttemptResult {
    Succeeded { duration_ms: u64 },
    Failed { error: String, duration_ms: u64 },
    Timeout,
    /// Prover binary missing, probe-file write failed, etc.
    Skipped { reason: String },
}

impl AttemptResult {
    pub fn is_success(&self) -> bool {
        matches!(self, AttemptResult::Succeeded { .. })
    }
    pub fn status_string(&self) -> &'static str {
        match self {
            AttemptResult::Succeeded { .. } => "succeeded",
            AttemptResult::Failed { .. } => "failed",
            AttemptResult::Timeout => "timeout",
            AttemptResult::Skipped { .. } => "skipped",
        }
    }
}

/// One end-to-end attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofAttempt {
    pub specialist: String,
    pub tactic_name: String,
    pub goal_excerpt: String,
    pub result: AttemptResult,
}

/// Generate a self-contained Isabelle probe file for a goal + tactic.
///
/// The probe wraps the goal as a `lemma probe_lemma:` with the
/// supplied tactic as the proof script. We strip the original lemma
/// name + assumes/shows scaffolding and reformulate as a single
/// `lemma probe_lemma: "<extracted statement>" <by ...>`.
///
/// For complex goals this is best-effort. The function returns the
/// probe text on success; callers write it to disk and pass the path
/// to `echidna prove`.
pub fn generate_probe(goal_text: &str, tactic: &TacticTemplate) -> String {
    let stmt = extract_statement(goal_text);
    format!(
        "(* SPDX-License-Identifier: PMPL-1.0-or-later *)\n\
         (* Burrower probe — auto-generated, do not edit. *)\n\
         theory Probe\n\
           imports Main\n\
         begin\n\n\
         lemma probe_lemma: {stmt}\n  {script}\n\n\
         end\n",
        stmt = stmt,
        script = tactic.script,
    )
}

/// Heuristic: extract the lemma statement (the part inside the outer
/// quotes, or the whole text if quotes aren't present).
///
/// Examples handled:
///   `lemma foo: "x + 0 = x" by simp`  → `"x + 0 = x"`
///   `lemma foo: "x + 0 = x"`          → `"x + 0 = x"`
///   `theorem foo : x = y := by rfl`   → `"x = y"` (best-effort)
fn extract_statement(goal_text: &str) -> String {
    // Find the first `:` after `lemma`/`theorem`/`Lemma`/`Theorem`.
    let lower_kws = ["lemma ", "theorem ", "Lemma ", "Theorem "];
    let mut after_colon: Option<&str> = None;
    for kw in &lower_kws {
        if let Some(start) = goal_text.find(kw) {
            let rest = &goal_text[start + kw.len()..];
            if let Some(c) = rest.find(':') {
                after_colon = Some(&rest[c + 1..]);
                break;
            }
        }
    }
    let body = after_colon.unwrap_or(goal_text).trim();
    // If the body has a quoted segment, take the first quoted region.
    if let Some(q1) = body.find('"') {
        let after_q1 = &body[q1 + 1..];
        if let Some(q2) = after_q1.find('"') {
            return format!("\"{}\"", &after_q1[..q2]);
        }
    }
    // No quotes — wrap the body up to the first `by`/`proof` marker.
    let trimmed: String = body
        .lines()
        .take_while(|l| {
            let t = l.trim_start();
            !(t.starts_with("by ") || t.starts_with("proof") || t.starts_with(":="))
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\"{}\"", trimmed.trim())
}

/// Run a single probe through the prover. Returns the raw outcome.
pub fn run_probe(
    probe_text: &str,
    config: &ProverConfig,
    probe_filename: &str,
) -> AttemptResult {
    use std::fs;
    let workdir = config
        .workdir
        .clone()
        .unwrap_or_else(|| PathBuf::from("/tmp/burrower_probes"));
    if let Err(e) = fs::create_dir_all(&workdir) {
        return AttemptResult::Skipped {
            reason: format!("workdir create failed: {e}"),
        };
    }
    let probe_path = workdir.join(probe_filename);
    if let Err(e) = fs::write(&probe_path, probe_text) {
        return AttemptResult::Skipped {
            reason: format!("probe write failed: {e}"),
        };
    }
    if !config.echidna_path.exists() {
        return AttemptResult::Skipped {
            reason: format!(
                "echidna binary not found at {}",
                config.echidna_path.display()
            ),
        };
    }

    let start = Instant::now();
    let output = Command::new(&config.echidna_path)
        .arg("prove")
        .arg(&probe_path)
        .arg("--prover")
        .arg("Isabelle")
        .arg("-t")
        .arg(config.timeout_secs.to_string())
        .output();
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stdout.contains("Proof verified successfully") {
                AttemptResult::Succeeded {
                    duration_ms: elapsed_ms,
                }
            } else if stdout.contains("Proof verification failed")
                || stderr.contains("FAILED")
                || stderr.contains("error")
            {
                let err_excerpt: String = stdout
                    .lines()
                    .chain(stderr.lines())
                    .filter(|l| {
                        l.contains("***") || l.contains("Failed") || l.contains("error")
                    })
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" | ");
                AttemptResult::Failed {
                    error: if err_excerpt.is_empty() {
                        "non-success exit; no diagnostic captured".to_string()
                    } else {
                        err_excerpt
                    },
                    duration_ms: elapsed_ms,
                }
            } else {
                // Inconclusive — treat as failed with raw context.
                AttemptResult::Failed {
                    error: format!("inconclusive output (first 200 chars): {}",
                                   stdout.chars().take(200).collect::<String>()),
                    duration_ms: elapsed_ms,
                }
            }
        }
        Err(e) => AttemptResult::Skipped {
            reason: format!("subprocess failed: {e}"),
        },
    }
}

/// Run the entire playbook against a goal. Records every attempt to
/// the ledger if one is supplied. Returns one [`ProofAttempt`] per
/// tactic tried.
pub fn run_playbook(
    goal: &Goal,
    playbook: &Playbook,
    config: &ProverConfig,
    ledger: Option<&Ledger>,
) -> Result<Vec<ProofAttempt>> {
    let mut attempts = Vec::new();
    let goal_h = goal_hash(&goal.raw);

    for (i, tactic) in playbook.tactics.iter().enumerate() {
        let probe = generate_probe(&goal.raw, tactic);
        let probe_filename = format!("probe_{}_{}.thy",
                                      sanitize_filename(&playbook.specialist),
                                      i);
        let result = run_probe(&probe, config, &probe_filename);

        let attempt = ProofAttempt {
            specialist: playbook.specialist.clone(),
            tactic_name: tactic.name.clone(),
            goal_excerpt: goal.raw.chars().take(200).collect(),
            result: result.clone(),
        };
        attempts.push(attempt.clone());

        if let Some(l) = ledger {
            let learning = derive_learning(&playbook.specialist, tactic, &result);
            let record = LedgerRecord {
                id: new_id(),
                timestamp: now_iso(),
                goal_hash: goal_h.clone(),
                goal_excerpt: goal.raw.chars().take(200).collect(),
                specialist: playbook.specialist.clone(),
                approach: Some(Approach {
                    description: format!("tried `{}`: {}", tactic.name, tactic.description),
                    tactics_attempted: vec![tactic.script.clone()],
                    preconditions_assumed: vec![],
                }),
                result: Some(RecordResult {
                    status: result.status_string().to_string(),
                    explanation: explain(&result),
                    artifacts: vec![],
                }),
                learning,
                extra: serde_json::Value::Null,
            };
            if let Err(e) = l.append(&record) {
                eprintln!("warning: ledger append failed: {e}");
            }
        }
    }
    Ok(attempts)
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

fn explain(r: &AttemptResult) -> String {
    match r {
        AttemptResult::Succeeded { duration_ms } => {
            format!("verified in {} ms", duration_ms)
        }
        AttemptResult::Failed { error, duration_ms } => {
            format!("failed in {} ms: {}", duration_ms, error)
        }
        AttemptResult::Timeout => "exceeded timeout".to_string(),
        AttemptResult::Skipped { reason } => format!("skipped: {}", reason),
    }
}

fn derive_learning(
    specialist: &str,
    tactic: &TacticTemplate,
    result: &AttemptResult,
) -> Option<Learning> {
    match result {
        AttemptResult::Succeeded { .. } => Some(Learning {
            pattern_extracted: format!("{}-tactic-works", tactic.name),
            pattern_kind: "positive".to_string(),
            generalisation: format!(
                "Specialist {specialist} succeeds with `{}` ({}) on goals of this shape.",
                tactic.script, tactic.description
            ),
            visible_to: vec![],
        }),
        AttemptResult::Failed { error, .. } => {
            // Classify failure flavour for richer anti-patterns.
            let kind = if error.contains("Undefined fact") || error.contains("Unknown") {
                "undefined-reference"
            } else if error.contains("Failed to apply")
                || error.contains("Failed to finish")
                || error.contains("Failed to refine")
                || error.contains("no unifiers")
            {
                "tactic-mismatch"
            } else if error.contains("Bad context") {
                "structural-mismatch"
            } else {
                "generic-failure"
            };
            Some(Learning {
                pattern_extracted: format!("{}-tactic-fails-{}", tactic.name, kind),
                pattern_kind: "anti-pattern".to_string(),
                generalisation: format!(
                    "Specialist {specialist} fails with `{}` ({}); consider {}.",
                    tactic.script,
                    kind,
                    suggest_next(tactic)
                ),
                visible_to: vec![specialist.to_string()],
            })
        }
        _ => None,
    }
}

fn suggest_next(t: &TacticTemplate) -> String {
    match t.name.as_str() {
        "simp" => "auto, blast, or fastforce",
        "auto" => "explicit case split + simp",
        "induct" => "induction (modern variant)",
        "induction" => "induct (classical variant) or rule: finite_induct",
        _ => "alternative tactic from the playbook",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::parse_goal;

    #[test]
    fn extract_statement_handles_quoted() {
        let s = extract_statement("lemma foo: \"x + 0 = x\" by simp");
        assert_eq!(s, "\"x + 0 = x\"");
    }

    #[test]
    fn extract_statement_handles_no_proof_marker() {
        let s = extract_statement("lemma foo: \"y + y = 2 * y\"");
        assert_eq!(s, "\"y + y = 2 * y\"");
    }

    #[test]
    fn generate_probe_makes_well_formed_theory() {
        let g = parse_goal("lemma foo: \"x + 0 = x\" by simp");
        let t = TacticTemplate {
            name: "simp".to_string(),
            script: "by simp".to_string(),
            description: "Isabelle simplifier".to_string(),
        };
        let probe = generate_probe(&g.raw, &t);
        assert!(probe.contains("theory Probe"));
        assert!(probe.contains("lemma probe_lemma"));
        assert!(probe.contains("by simp"));
        assert!(probe.contains("end"));
    }

    #[test]
    fn skipped_result_when_echidna_missing() {
        let probe = "theory Probe imports Main begin lemma foo: \"True\" by simp end";
        let cfg = ProverConfig {
            echidna_path: PathBuf::from("/nonexistent/echidna"),
            timeout_secs: 5,
            workdir: None,
        };
        let r = run_probe(probe, &cfg, "missing_test.thy");
        assert!(matches!(r, AttemptResult::Skipped { .. }));
    }
}
