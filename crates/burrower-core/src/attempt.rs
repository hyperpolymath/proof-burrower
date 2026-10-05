// SPDX-License-Identifier: MPL-2.0
// Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
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
//!    subprocess. When the installed echidna offers `--output json` it
//!    asks for the `echidna.prove.result/1` object and parses it (see
//!    [`crate::echidna_contract`]); otherwise it falls back to the legacy
//!    text markers.
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

use crate::echidna_contract::{
    detect_output_mode, output_tolerating_busy, parse_prove_result, rejected_output_flag,
    remember_output_mode, OutputMode, ProveResult, ProveStatus, SCHEMA,
};
use crate::goal::Goal;
use crate::ledger::{
    goal_hash, new_id, now_iso, Approach, Learning, Ledger, LedgerRecord, RecordResult,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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
    /// The `echidna` binary: a path, or a bare name looked up on `PATH`.
    pub echidna_path: PathBuf,
    /// Per-attempt timeout (passed to `echidna prove -t`).
    pub timeout_secs: u32,
    /// Parent directory for private, per-attempt probe directories.
    /// Defaults to the system temporary directory. Each attempt cleans up
    /// its own directory after the child exits.
    pub workdir: Option<PathBuf>,
    /// Project root for echidna's EI-1 `--project-root` flag (2026-04-26).
    /// When set, every probe is dispatched as
    /// `echidna prove --project-root <p> ...`, letting the probe's
    /// theory imports resolve against the project's existing ROOT.
    /// Without this, probes can only `imports Main`.
    pub project_root: Option<PathBuf>,
    /// Sandbox mode forwarded to echidna's `--sandbox` flag
    /// (safe-learning b, 2026-04-26). One of "none" | "bwrap" | "podman".
    /// Default "none" preserves backwards compatibility; recommend
    /// "bwrap" when running specialist playbooks against unverified goals.
    pub sandbox: String,
}

impl Default for ProverConfig {
    fn default() -> Self {
        Self {
            echidna_path: PathBuf::from("echidna"),
            timeout_secs: 60,
            workdir: None,
            project_root: None,
            sandbox: "none".to_string(),
        }
    }
}

/// What a success rests on, transported from `echidna.prove.result/1`.
///
/// Present only when echidna emitted the contract object; a success read
/// from legacy text markers carries no receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProofReceipt {
    /// Contract schema the receipt was parsed from.
    pub schema: String,
    /// Backend that checked the proof.
    pub prover: String,
    /// Axioms the checked proof depends on.
    pub axioms: Vec<String>,
    /// echidna's confidence, if it reported one.
    pub confidence: Option<f64>,
    /// The echidna version that issued the receipt.
    pub echidna_version: String,
    /// Content id of the prove result (UUIDv8 over the JCS bytes of its
    /// contract fields, see [`crate::ids::content_id`]); identical results
    /// share an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
}

impl ProofReceipt {
    /// Build a receipt from a parsed prove result.
    pub fn from_result(r: &ProveResult) -> Self {
        let result_id = serde_json::to_value(r)
            .ok()
            .and_then(|v| crate::ids::content_id(&v).ok())
            .map(|u| u.to_string());
        Self {
            schema: r.schema.clone(),
            prover: r.prover.clone(),
            axioms: r.trust.axioms.clone(),
            confidence: r.trust.confidence,
            echidna_version: r.echidna_version.clone(),
            result_id,
        }
    }
}

/// Outcome of one proof attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttemptResult {
    Succeeded {
        duration_ms: u64,
        /// `Some` when echidna issued an `echidna.prove.result/1` receipt;
        /// `None` when success was inferred from legacy text markers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<ProofReceipt>,
    },
    Failed {
        error: String,
        duration_ms: u64,
    },
    Timeout,
    /// Prover binary missing, probe-file write failed, etc.
    Skipped {
        reason: String,
    },
}

impl AttemptResult {
    /// True for [`AttemptResult::Succeeded`].
    pub fn is_success(&self) -> bool {
        matches!(self, AttemptResult::Succeeded { .. })
    }
    /// Ledger status word for this outcome.
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
/// name and proof script while retaining assumptions and every conclusion.
///
/// For complex goals this is best-effort. The function returns the
/// probe text on success; callers write it to disk and pass the path
/// to `echidna prove`.
pub fn generate_probe(goal_text: &str, tactic: &TacticTemplate) -> String {
    let stmt = extract_statement(goal_text);
    format!(
        "(* SPDX-License-Identifier: MPL-2.0 *)\n\
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

/// Extract a simple Isabelle statement, retaining every quoted clause.
/// This is not a complete Isabelle outer-syntax parser; unsupported syntax
/// is retained for the real prover to reject rather than dropping clauses.
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
            if let Some(c) = rest.find(':').filter(|c| {
                // A colon inside a quoted proposition (e.g. x::nat) is
                // not the separator after a theorem name.
                rest.find('"').is_none_or(|quote| *c < quote)
            }) {
                after_colon = Some(&rest[c + 1..]);
                break;
            } else if rest.trim_start().starts_with('"') {
                after_colon = Some(rest);
                break;
            }
        }
    }
    let body = after_colon.unwrap_or(goal_text).trim();
    // Preserve the complete statement, including fixes/assumes/shows and
    // multiple quoted propositions. Taking the first quote can silently
    // replace the conclusion with an assumption. Stop at an existing proof
    // command only outside quoted terms; Isabelle checks the retained syntax.
    let mut quoted = false;
    let mut escaped = false;
    let mut comment_depth = 0usize;
    let mut skip_until = 0;
    let mut end = body.len();
    for (index, ch) in body.char_indices() {
        if index < skip_until {
            continue;
        }
        let tail = &body[index..];
        if !quoted && tail.starts_with("(*") {
            comment_depth += 1;
            skip_until = index + 2;
            continue;
        }
        if comment_depth > 0 {
            if tail.starts_with("*)") {
                comment_depth -= 1;
                skip_until = index + 2;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted
            && (index == 0 || body[..index].ends_with(char::is_whitespace))
            && ["by", "proof", ":="].iter().any(|marker| {
                tail.strip_prefix(marker)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            })
        {
            end = index;
            break;
        }
    }
    let statement = body[..end].trim();
    if statement.contains('"') {
        statement.to_string()
    } else {
        format!("\"{}\"", statement)
    }
}

/// Run a single probe through the prover. Returns the raw outcome.
pub fn run_probe(probe_text: &str, config: &ProverConfig, probe_filename: &str) -> AttemptResult {
    use std::fs;
    let filename = std::path::Path::new(probe_filename);
    if filename.file_name() != Some(filename.as_os_str()) {
        return AttemptResult::Skipped {
            reason: "probe filename must be a single file name".into(),
        };
    }
    let workdir = match &config.workdir {
        Some(parent) => fs::create_dir_all(parent).and_then(|()| {
            tempfile::Builder::new()
                .prefix("burrower-probe-")
                .tempdir_in(parent)
        }),
        None => tempfile::Builder::new().prefix("burrower-probe-").tempdir(),
    };
    let workdir = match workdir {
        Ok(dir) => dir,
        Err(e) => {
            return AttemptResult::Skipped {
                reason: format!("workdir create failed: {e}"),
            }
        }
    };
    let probe_path = workdir.path().join(probe_filename);
    if let Err(e) = fs::write(&probe_path, probe_text) {
        return AttemptResult::Skipped {
            reason: format!("probe write failed: {e}"),
        };
    }
    let Some(echidna) =
        resolve_executable(&config.echidna_path, std::env::var_os("PATH").as_deref())
    else {
        return AttemptResult::Skipped {
            reason: format!(
                "echidna binary not found at {}",
                config.echidna_path.display()
            ),
        };
    };

    let mut mode = detect_output_mode(&echidna);
    loop {
        let start = Instant::now();
        let output =
            output_tolerating_busy(&mut prove_command(&echidna, &probe_path, config, mode));
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let o = match output {
            Ok(o) => o,
            Err(e) => {
                return AttemptResult::Skipped {
                    reason: format!("subprocess failed: {e}"),
                }
            }
        };
        match mode {
            OutputMode::Contract
                if o.status.code() == Some(2)
                    && rejected_output_flag(&String::from_utf8_lossy(&o.stderr)) =>
            {
                // `prove --help` advertised the flag but the binary refused
                // it: treat this binary as pre-contract from now on.
                remember_output_mode(&echidna, OutputMode::Legacy);
                mode = OutputMode::Legacy;
            }
            OutputMode::Contract => return interpret_contract(&o, elapsed_ms),
            OutputMode::Legacy => return interpret_legacy(&o, elapsed_ms),
        }
    }
}

/// Resolve the configured echidna to an existing file.
///
/// A value with more than one path component is used as given; a bare
/// name (the default `echidna`) is searched for in `path_var`, the
/// colon-separated `PATH`. Returns `None` when nothing exists.
pub fn resolve_executable(configured: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if configured.components().count() != 1 || configured.is_absolute() {
        return configured.exists().then(|| configured.to_path_buf());
    }
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(configured))
        .find(|candidate| candidate.is_file())
}

/// Build the `echidna prove` invocation for one probe file.
fn prove_command(echidna: &Path, probe: &Path, config: &ProverConfig, mode: OutputMode) -> Command {
    let mut cmd = Command::new(echidna);
    cmd.arg("prove")
        .arg(probe)
        .arg("--prover")
        .arg("Isabelle")
        .arg("-t")
        .arg(config.timeout_secs.to_string());
    // EI-1: forward the project root so probes importing project-
    // specific theories (Tropical_v2, walks_def, ...) resolve.
    if let Some(p) = config.project_root.as_ref() {
        cmd.arg("--project-root").arg(p);
    }
    // safe-learning b: forward the sandbox mode. echidna defaults to
    // "none" so an unset value is the same as the legacy path.
    if config.sandbox != "none" && !config.sandbox.is_empty() {
        cmd.arg("--sandbox").arg(&config.sandbox);
    }
    if mode == OutputMode::Contract {
        cmd.arg("--output").arg("json");
    }
    cmd
}

/// Map an `echidna.prove.result/1` run onto an [`AttemptResult`].
///
/// The contract object is authoritative. A malformed object is a
/// contract violation and fails the attempt; it is never re-read as text.
/// A `verified` status with a non-zero exit is also treated as a
/// violation, matching the legacy rule that failure evidence wins.
fn interpret_contract(o: &Output, elapsed_ms: u64) -> AttemptResult {
    let stdout = String::from_utf8_lossy(&o.stdout);
    let result = match parse_prove_result(&stdout) {
        Ok(r) => r,
        Err(e) => {
            return AttemptResult::Failed {
                error: format!(
                    "{SCHEMA} contract violation (exit {}): {e}",
                    o.status.code().unwrap_or(-1)
                ),
                duration_ms: elapsed_ms,
            }
        }
    };
    let message = if result.message.is_empty() {
        format!("echidna reported {:?} with no message", result.status)
    } else {
        result.message.clone()
    };
    match result.status {
        ProveStatus::Verified if o.status.success() => AttemptResult::Succeeded {
            duration_ms: elapsed_ms,
            receipt: Some(ProofReceipt::from_result(&result)),
        },
        ProveStatus::Verified => AttemptResult::Failed {
            error: format!(
                "{SCHEMA} contract violation: status verified but exit {}",
                o.status.code().unwrap_or(-1)
            ),
            duration_ms: elapsed_ms,
        },
        ProveStatus::Failed => AttemptResult::Failed {
            error: message,
            duration_ms: elapsed_ms,
        },
        ProveStatus::Unknown => AttemptResult::Failed {
            error: format!("inconclusive (status unknown): {message}"),
            duration_ms: elapsed_ms,
        },
        ProveStatus::Timeout => AttemptResult::Timeout,
        ProveStatus::Error => AttemptResult::Skipped {
            reason: format!("echidna error: {message}"),
        },
    }
}

/// Map a pre-contract echidna run onto an [`AttemptResult`] from text markers.
fn interpret_legacy(o: &Output, elapsed_ms: u64) -> AttemptResult {
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);

    // Discovered 2026-04-26 during the swarm-dogfood session:
    // echidna's "Proof verified successfully" / "Proof verification
    // failed" lines come from `OutputFormatter`, which writes to
    // STDERR, not stdout. The previous stdout-only check made every
    // attempt look "inconclusive" and demoted real failures to
    // generic-failure anti-patterns. We now scan BOTH streams and
    // also fall back on the exit code so a non-zero exit with no
    // standard marker still becomes Failed (not Inconclusive).
    let combined_lines: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    let says_success = combined_lines
        .iter()
        .any(|l| l.contains("Proof verified successfully") || l.contains("✓ Proof verified"));
    let says_failure = combined_lines.iter().any(|l| {
        l.contains("Proof verification failed")
            || l.contains("✗ Proof verification failed")
            || l.contains("FAILED")
    });
    let exit_failed = !o.status.success();

    if says_success && !says_failure && !exit_failed {
        AttemptResult::Succeeded {
            duration_ms: elapsed_ms,
            receipt: None,
        }
    } else if says_failure || exit_failed {
        let err_excerpt: String = combined_lines
            .iter()
            .filter(|l| {
                l.contains("***")
                    || l.contains("Failed")
                    || l.contains("error")
                    || l.contains("Unable")
            })
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(" | ");
        AttemptResult::Failed {
            error: if err_excerpt.is_empty() {
                format!(
                    "exit {} — no standard diagnostic captured",
                    o.status.code().unwrap_or(-1)
                )
            } else {
                err_excerpt
            },
            duration_ms: elapsed_ms,
        }
    } else {
        // Truly inconclusive — exit 0, no markers either way.
        AttemptResult::Failed {
            error: format!(
                "inconclusive output (no success/failure marker, exit 0): {}",
                stdout.chars().take(200).collect::<String>()
            ),
            duration_ms: elapsed_ms,
        }
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
    let goal_id = crate::ids::goal_content_id(&goal.raw);

    for (i, tactic) in playbook.tactics.iter().enumerate() {
        let probe = generate_probe(&goal.raw, tactic);
        let probe_filename = format!(
            "probe_{}_{}.thy",
            sanitize_filename(&playbook.specialist),
            i
        );
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
                goal_id: Some(goal_id.clone()),
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
                extra: receipt_extra(&result),
            };
            if let Err(e) = l.append(&record) {
                eprintln!("warning: ledger append failed: {e}");
            }
        }
    }
    Ok(attempts)
}

/// Ledger `extra` payload: the transported receipt, if any.
fn receipt_extra(r: &AttemptResult) -> serde_json::Value {
    match r {
        AttemptResult::Succeeded {
            receipt: Some(receipt),
            ..
        } => serde_json::json!({ "receipt": receipt }),
        _ => serde_json::Value::Null,
    }
}

/// Keep only characters that are safe in a probe file name.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// One-line ledger explanation of an attempt outcome.
///
/// Says whether a success is backed by an echidna receipt or only by
/// legacy text markers, so a reader can tell proof from hearsay.
fn explain(r: &AttemptResult) -> String {
    match r {
        AttemptResult::Succeeded {
            duration_ms,
            receipt: Some(rc),
        } => format!(
            "verified in {} ms (receipt: {} from echidna {}, prover {}, axioms [{}])",
            duration_ms,
            rc.schema,
            rc.echidna_version,
            rc.prover,
            rc.axioms.join(", ")
        ),
        AttemptResult::Succeeded {
            duration_ms,
            receipt: None,
        } => {
            format!(
                "verified in {} ms (legacy text markers: a warrant, not a receipt)",
                duration_ms
            )
        }
        AttemptResult::Failed { error, duration_ms } => {
            format!("failed in {} ms: {}", duration_ms, error)
        }
        AttemptResult::Timeout => "exceeded timeout".to_string(),
        AttemptResult::Skipped { reason } => format!("skipped: {}", reason),
    }
}

/// Turn an attempt outcome into a ledger learning, if one is extractable.
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

/// Suggest the next tactic to try after `t` failed.
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

    #[test]
    fn extract_statement_preserves_nested_comments_and_the_conclusion() {
        for statement in [
            "\"True\" (* by *)",
            "\"True\" (* proof (* by *) := *)",
            "\"True\" (* \" by *) and \"False\"",
        ] {
            assert_eq!(
                extract_statement(&format!("lemma foo: {statement} by simp")),
                statement
            );
        }
    }
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
    fn extract_statement_preserves_assumptions_and_conclusion() {
        let statement = "assumes \"True\" shows \"False\"";
        assert_eq!(
            extract_statement(&format!("lemma bad: {statement} by simp")),
            statement
        );
    }

    #[test]
    fn extract_statement_preserves_multiple_conclusions() {
        assert_eq!(
            extract_statement("lemma bad: \"True\" and \"False\" by simp"),
            "\"True\" and \"False\""
        );
    }

    #[test]
    fn extract_statement_keeps_type_annotation_in_anonymous_lemma() {
        assert_eq!(
            extract_statement("lemma \"(x::nat) = x\" by simp"),
            "\"(x::nat) = x\""
        );
    }

    #[test]
    fn extract_statement_keeps_proof_words_inside_terms() {
        assert_eq!(
            extract_statement("lemma foo: \"by = proof\" by simp"),
            "\"by = proof\""
        );
    }

    /// Legacy path: a non-zero exit or failure text beats a success marker.
    #[cfg(unix)]
    #[test]
    fn subprocess_failure_overrides_success_text() {
        use std::os::unix::fs::PermissionsExt;
        let workdir = std::env::temp_dir().join(format!(
            "burrower-output-regression-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workdir).unwrap();
        let executable = workdir.join("output-fixture.sh");
        let config = ProverConfig {
            echidna_path: executable.clone(),
            timeout_secs: 5,
            workdir: Some(workdir.clone()),
            project_root: None,
            sandbox: "none".into(),
        };
        // These subprocesses test the output contract, not theorem proving.
        for (diagnostic, exit, expected) in [
            ("", 0, true),
            ("", 1, false),
            ("Proof verification failed", 0, false),
        ] {
            std::fs::write(&executable, format!(
                "#!/bin/sh\nprintf 'Proof verified successfully\\n'\nprintf '{diagnostic}\\n' >&2\nexit {exit}\n"
            )).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                run_probe("fixture", &config, "fixture.thy").is_success(),
                expected
            );
            // Each body is a new binary at the same path: forget the
            // cached output mode so detection runs again.
            crate::echidna_contract::remember_output_mode(&executable, OutputMode::Legacy);
        }
        std::fs::remove_dir_all(workdir).unwrap();
    }

    /// Write an executable fake echidna into `dir` and return its path.
    #[cfg(unix)]
    fn fake_echidna(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    /// A probe-runner config pointing at `echidna`.
    fn config_for(echidna: PathBuf) -> ProverConfig {
        ProverConfig {
            echidna_path: echidna,
            timeout_secs: 5,
            ..ProverConfig::default()
        }
    }

    const RESULT: &str = r#"{"duration_ms":7,"echidna_version":"2.4.0","goal":"p.thy","message":"MSG","prover":"Isabelle","schema":"echidna.prove.result/1","status":"STATUS","trust":{"axioms":["sorry"],"confidence":0.5}}"#;

    /// A contract-speaking fake: advertises `--output`, prints `json`, exits `exit`.
    #[cfg(unix)]
    fn contract_echidna(dir: &std::path::Path, name: &str, json: &str, exit: i32) -> PathBuf {
        fake_echidna(
            dir,
            name,
            &format!(
                "case \"$*\" in\n  *--help*) echo '      --output <OUTPUT>  Output format'; exit 0 ;;\n  *'--output json'*) printf '%s\\n' '{json}'; exit {exit} ;;\n  *) echo 'Proof verified successfully' >&2; exit 0 ;;\nesac"
            ),
        )
    }

    /// Contract path: every status maps to the right outcome; success carries the receipt.
    #[cfg(unix)]
    #[test]
    fn contract_mode_maps_every_status_and_carries_the_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let verified = RESULT.replace("STATUS", "verified").replace("MSG", "");
        let path = contract_echidna(dir.path(), "verified.sh", &verified, 0);
        match run_probe("p", &config_for(path), "p.thy") {
            AttemptResult::Succeeded {
                receipt: Some(rc), ..
            } => {
                assert_eq!(rc.prover, "Isabelle");
                assert_eq!(rc.axioms, vec!["sorry".to_string()]);
                assert_eq!(rc.confidence, Some(0.5));
                assert_eq!(rc.echidna_version, "2.4.0");
                assert_eq!(rc.schema, SCHEMA);
            }
            other => panic!("expected receipt-backed success, got {other:?}"),
        }

        let cases: [(&str, &str, i32, &str); 6] = [
            ("failed", "Failed to apply", 1, "failed"),
            ("failed", "", 1, "failed"),
            ("unknown", "no answer", 0, "failed"),
            ("timeout", "", 0, "timeout"),
            ("error", "Isabelle missing", 1, "skipped"),
            ("verified", "", 1, "failed"),
        ];
        for (i, (status, msg, exit, expected)) in cases.iter().enumerate() {
            let json = RESULT.replace("STATUS", status).replace("MSG", msg);
            let path = contract_echidna(dir.path(), &format!("c{i}.sh"), &json, *exit);
            let r = run_probe("p", &config_for(path), "p.thy");
            assert_eq!(r.status_string(), *expected, "{status}/{exit}: {r:?}");
            if !msg.is_empty() && *expected != "timeout" {
                assert!(format!("{r:?}").contains(msg), "{r:?}");
            }
        }
    }

    /// Contract path: malformed or non-canonical stdout fails, never falls back to text.
    #[cfg(unix)]
    #[test]
    fn contract_mode_rejects_malformed_output_instead_of_string_matching() {
        let dir = tempfile::tempdir().unwrap();
        // Advertises the flag, then prints the legacy success text: a
        // contract violation, never a success.
        let path = contract_echidna(dir.path(), "liar.sh", "Proof verified successfully", 0);
        match run_probe("p", &config_for(path), "p.thy") {
            AttemptResult::Failed { error, .. } => {
                assert!(error.contains("contract violation"), "{error}")
            }
            other => panic!("expected contract violation, got {other:?}"),
        }
        // Planted non-canonical mutant: right data, wrong key order.
        let mutant = RESULT
            .replace("STATUS", "verified")
            .replace("MSG", "")
            .replacen(
                r#"{"duration_ms":7,"echidna_version":"2.4.0","#,
                r#"{"echidna_version":"2.4.0","duration_ms":7,"#,
                1,
            );
        let path = contract_echidna(dir.path(), "mutant.sh", &mutant, 0);
        assert!(!run_probe("p", &config_for(path), "p.thy").is_success());
    }

    /// A pre-contract echidna is driven through the legacy text markers.
    #[cfg(unix)]
    #[test]
    fn legacy_echidna_without_the_flag_uses_text_markers() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-contract echidna: no `--output` in help, and it would reject it.
        let path = fake_echidna(
            dir.path(),
            "legacy.sh",
            "case \"$*\" in\n  *--help*) echo '      --project-root <P>'; exit 0 ;;\n  *--output*) echo \"error: unexpected argument '--output' found\" >&2; exit 2 ;;\n  *) echo 'Proof verified successfully' >&2; exit 0 ;;\nesac",
        );
        match run_probe("p", &config_for(path), "p.thy") {
            AttemptResult::Succeeded { receipt: None, .. } => {}
            other => panic!("expected legacy success without receipt, got {other:?}"),
        }
    }

    /// A binary whose help lies about `--output` is demoted to legacy mode.
    #[cfg(unix)]
    #[test]
    fn binary_that_advertises_but_rejects_the_flag_falls_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = fake_echidna(
            dir.path(),
            "fibber.sh",
            "case \"$*\" in\n  *--help*) echo '  --output <OUTPUT>'; exit 0 ;;\n  *--output*) echo \"error: unexpected argument '--output' found\" >&2; exit 2 ;;\n  *) echo 'Proof verified successfully' >&2; exit 0 ;;\nesac",
        );
        let cfg = config_for(path.clone());
        assert!(run_probe("p", &cfg, "p.thy").is_success());
        assert_eq!(
            crate::echidna_contract::detect_output_mode(&path),
            OutputMode::Legacy
        );
    }

    /// Bare names resolve through PATH; explicit paths are used as given.
    #[cfg(unix)]
    #[test]
    fn bare_name_is_resolved_on_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let found = fake_echidna(dir.path(), "echidna-fake", "exit 0");
        let path_var =
            std::env::join_paths([PathBuf::from("/nonexistent"), dir.path().to_path_buf()])
                .unwrap();
        assert_eq!(
            resolve_executable(std::path::Path::new("echidna-fake"), Some(&path_var)),
            Some(found.clone())
        );
        assert_eq!(
            resolve_executable(std::path::Path::new("echidna-fake"), None),
            None
        );
        assert_eq!(
            resolve_executable(std::path::Path::new("absent-echidna"), Some(&path_var)),
            None
        );
        assert_eq!(resolve_executable(&found, None), Some(found.clone()));
        assert_eq!(
            resolve_executable(&dir.path().join("absent"), Some(&path_var)),
            None
        );
    }

    /// Ledger text and `extra` tell receipt-backed successes from marker-only ones.
    #[test]
    fn ledger_explanations_distinguish_receipts_from_warrants() {
        let receipt = ProofReceipt {
            schema: SCHEMA.into(),
            prover: "Isabelle".into(),
            axioms: vec!["sorry".into()],
            confidence: None,
            echidna_version: "2.4.0".into(),
            result_id: None,
        };
        let with = AttemptResult::Succeeded {
            duration_ms: 3,
            receipt: Some(receipt),
        };
        let without = AttemptResult::Succeeded {
            duration_ms: 3,
            receipt: None,
        };
        assert!(explain(&with).contains("receipt: echidna.prove.result/1"));
        assert!(explain(&with).contains("axioms [sorry]"));
        assert!(explain(&without).contains("warrant, not a receipt"));
        assert_eq!(receipt_extra(&with)["receipt"]["prover"], "Isabelle");
        assert!(receipt_extra(&without).is_null());
        let json = serde_json::to_string(&without).unwrap();
        assert!(!json.contains("receipt"), "{json}");
    }

    /// `run_playbook` stores the transported receipt in the ledger record.
    #[cfg(unix)]
    #[test]
    fn playbook_ledger_records_the_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let verified = RESULT.replace("STATUS", "verified").replace("MSG", "");
        let path = contract_echidna(dir.path(), "pb.sh", &verified, 0);
        let ledger = crate::ledger::Ledger::open(dir.path().join("l.jsonl")).unwrap();
        let playbook = Playbook {
            specialist: "Algebraist".into(),
            tactics: vec![TacticTemplate {
                name: "simp".into(),
                script: "by simp".into(),
                description: "simplifier".into(),
            }],
        };
        let goal = parse_goal("lemma foo: \"True\" by simp");
        let attempts = run_playbook(&goal, &playbook, &config_for(path), Some(&ledger)).unwrap();
        assert!(attempts[0].result.is_success());
        let records = ledger.read_all().unwrap();
        assert_eq!(records[0].extra["receipt"]["schema"], SCHEMA);
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
            project_root: None,
            sandbox: "none".to_string(),
        };
        let r = run_probe(probe, &cfg, "missing_test.thy");
        assert!(matches!(r, AttemptResult::Skipped { .. }));
    }
}
