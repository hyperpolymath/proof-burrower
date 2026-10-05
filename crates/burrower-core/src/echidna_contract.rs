// SPDX-License-Identifier: MPL-2.0
// Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
//! # The `echidna.prove.result/1` contract
//!
//! `echidna prove ... --output json` prints exactly one JCS-canonical
//! (RFC 8785) I-JSON (RFC 7493) object on stdout:
//!
//! ```json
//! {"duration_ms":12,"echidna_version":"2.4.0","goal":"probe.thy",
//!  "message":"","prover":"Isabelle","schema":"echidna.prove.result/1",
//!  "status":"verified","trust":{"axioms":[],"confidence":null}}
//! ```
//!
//! (shown wrapped; on the wire it is one line, keys in JCS order).
//!
//! This module is the consumer side. It parses and validates that object
//! and detects whether an installed `echidna` offers the flag at all, so
//! [`crate::attempt::run_probe`] can fall back to the legacy text markers
//! only when the flag is absent. See `docs/ECHIDNA-INTEGRATION.adoc`.
//!
//! ## Receipt, not warrant
//!
//! A `verified` result parsed from this contract is a *receipt*: echidna's
//! own statement of what it checked, with the prover and the axioms it
//! relied on. A success inferred from text markers is only a *warrant*
//! (evidence that something printed "verified"). The distinction follows
//! the factive/non-factive split in `hyperpolymath/epistemic-types`
//! (`FactiveModality`); this crate copies the distinction as a pattern and
//! does not depend on that Agda library.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, OnceLock};

/// The only schema identifier this consumer accepts.
pub const SCHEMA: &str = "echidna.prove.result/1";

/// The `status` field of a prove result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProveStatus {
    /// The prover accepted the proof.
    Verified,
    /// The prover rejected the proof.
    Failed,
    /// echidna or the backend could not run (configuration, missing tool).
    Error,
    /// The backend exceeded its time budget.
    Timeout,
    /// The backend finished without a definite answer.
    Unknown,
}

/// The `trust` object: what the verdict rests on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trust {
    /// echidna's confidence, or `null` when it has no receipt-backed figure.
    pub confidence: Option<f64>,
    /// Axioms (or `sorry`-like escapes) the checked proof depends on.
    pub axioms: Vec<String>,
}

/// One parsed `echidna.prove.result/1` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProveResult {
    /// Always [`SCHEMA`].
    pub schema: String,
    /// The verdict.
    pub status: ProveStatus,
    /// Backend that produced the verdict, e.g. `Isabelle`.
    pub prover: String,
    /// The goal or file echidna was asked to prove.
    pub goal: String,
    /// echidna's own timing of the backend run.
    pub duration_ms: u64,
    /// Human-readable diagnostic; may be empty.
    pub message: String,
    /// What the verdict rests on.
    pub trust: Trust,
    /// The `echidna --version` that emitted this object.
    pub echidna_version: String,
}

/// Why a stdout payload is not a valid `echidna.prove.result/1` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// stdout was empty.
    Empty,
    /// stdout held more than one line (the contract allows exactly one object).
    ExtraOutput,
    /// Not I-JSON (bad JSON, duplicate keys, unsafe integers, ...).
    NotIJson(String),
    /// Valid I-JSON, but not byte-identical to its JCS canonical form.
    NotCanonical,
    /// The object does not have the contract's shape.
    Shape(String),
    /// `schema` names a version this consumer does not understand.
    WrongSchema(String),
}

impl fmt::Display for ContractError {
    /// Render the violation as a one-line diagnostic.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractError::Empty => write!(f, "empty stdout"),
            ContractError::ExtraOutput => write!(f, "more than one line on stdout"),
            ContractError::NotIJson(e) => write!(f, "not I-JSON: {e}"),
            ContractError::NotCanonical => write!(f, "not JCS-canonical (RFC 8785)"),
            ContractError::Shape(e) => write!(f, "wrong shape: {e}"),
            ContractError::WrongSchema(s) => write!(f, "unsupported schema {s:?}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Parse and validate echidna's stdout as one `echidna.prove.result/1` object.
///
/// Accepts exactly one line (a single trailing newline is allowed). The line
/// must be I-JSON, must equal its own JCS canonicalisation byte for byte,
/// must name [`SCHEMA`], and must carry every contract field with the right
/// type. Unknown extra fields are tolerated so that additive changes do not
/// break older consumers; removing or retyping a field needs a new schema.
pub fn parse_prove_result(stdout: &str) -> Result<ProveResult, ContractError> {
    let line = stdout.strip_suffix('\n').unwrap_or(stdout);
    if line.is_empty() {
        return Err(ContractError::Empty);
    }
    if line.contains('\n') {
        return Err(ContractError::ExtraOutput);
    }
    let value = ijson_jcs::parse_json(line, ijson_jcs::JsonMode::Strict)
        .map_err(|e| ContractError::NotIJson(e.to_string()))?;
    let canonical =
        ijson_jcs::to_jcs_string(&value).map_err(|e| ContractError::NotIJson(e.to_string()))?;
    if canonical != line {
        return Err(ContractError::NotCanonical);
    }
    match value.get("schema").and_then(|s| s.as_str()) {
        Some(SCHEMA) => {}
        Some(other) => return Err(ContractError::WrongSchema(other.to_string())),
        None => return Err(ContractError::Shape("missing string field `schema`".into())),
    }
    serde_json::from_value(value).map_err(|e| ContractError::Shape(e.to_string()))
}

/// How proof-burrower talks to a given `echidna` binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// `echidna prove ... --output json`, parsed with [`parse_prove_result`].
    Contract,
    /// Pre-contract echidna: success and failure are read from text markers.
    Legacy,
}

/// Process-wide cache of [`OutputMode`] per echidna path.
fn mode_cache() -> &'static Mutex<HashMap<PathBuf, OutputMode>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, OutputMode>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Decide whether `echidna prove --help` text advertises `--output`.
///
/// Matches the option as a whole whitespace-delimited token (clap prints
/// `--output <OUTPUT>` or `-o, --output <OUTPUT>`), so options such as
/// `--output-dir` do not count.
pub fn help_advertises_output(help: &str) -> bool {
    help.split_whitespace().any(|token| {
        let token = token.trim_end_matches(',');
        token == "--output" || token.starts_with("--output=")
    })
}

/// Detect, once per binary and process, whether `echidna` supports the contract.
///
/// Runs `<echidna> prove --help` and looks for the `--output` option. Any
/// failure to run the binary yields [`OutputMode::Legacy`]; the real attempt
/// then reports the underlying problem.
pub fn detect_output_mode(echidna: &Path) -> OutputMode {
    if let Some(mode) = mode_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(echidna).copied())
    {
        return mode;
    }
    let mut help = Command::new(echidna);
    help.arg("prove").arg("--help");
    let mode = match output_tolerating_busy(&mut help) {
        Ok(out) if help_advertises_output(&String::from_utf8_lossy(&out.stdout)) => {
            OutputMode::Contract
        }
        Ok(_) => OutputMode::Legacy,
        // Could not run the binary at all: answer Legacy for this call but
        // do not cache it, so a transient failure is not remembered.
        Err(_) => return OutputMode::Legacy,
    };
    remember_output_mode(echidna, mode);
    mode
}

/// Run `cmd` to completion, retrying briefly while the executable is busy.
///
/// `ETXTBSY` ("Text file busy") is transient: it means some process still
/// holds the binary open for writing, typically during an install or while
/// another thread is between `fork` and `exec`. Any other error is returned
/// at once.
pub fn output_tolerating_busy(cmd: &mut Command) -> std::io::Result<Output> {
    const ETXTBSY: i32 = 26;
    let mut attempt = 0;
    loop {
        match cmd.output() {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
            }
            other => return other,
        }
    }
}

/// Record `mode` for `echidna`, e.g. after a binary rejected `--output`.
pub fn remember_output_mode(echidna: &Path, mode: OutputMode) {
    if let Ok(mut cache) = mode_cache().lock() {
        cache.insert(echidna.to_path_buf(), mode);
    }
}

/// True when stderr shows clap rejecting the `--output` flag itself.
pub fn rejected_output_flag(stderr: &str) -> bool {
    stderr.contains("unexpected argument '--output'")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"duration_ms":12,"echidna_version":"2.4.0","goal":"probe.thy","message":"","prover":"Isabelle","schema":"echidna.prove.result/1","status":"verified","trust":{"axioms":["sorry"],"confidence":null}}"#;

    /// A canonical object parses, with or without one trailing newline.
    #[test]
    fn parses_a_canonical_result_with_or_without_trailing_newline() {
        for out in [GOOD.to_string(), format!("{GOOD}\n")] {
            let r = parse_prove_result(&out).unwrap();
            assert_eq!(r.status, ProveStatus::Verified);
            assert_eq!(r.prover, "Isabelle");
            assert_eq!(r.duration_ms, 12);
            assert_eq!(r.trust.axioms, vec!["sorry".to_string()]);
            assert_eq!(r.trust.confidence, None);
        }
    }

    /// Each of the five statuses parses and serialises back unchanged.
    #[test]
    fn every_status_round_trips() {
        for s in ["verified", "failed", "error", "timeout", "unknown"] {
            let out = GOOD.replace("\"verified\"", &format!("\"{s}\""));
            let r = parse_prove_result(&out).unwrap();
            assert_eq!(serde_json::to_value(r.status).unwrap(), s);
        }
    }

    /// Planted mutants (key order, whitespace) are rejected as non-canonical.
    #[test]
    fn planted_non_canonical_mutant_is_rejected() {
        // Same data, keys out of JCS order: valid JSON, invalid contract.
        let mutant = GOOD.replacen(
            r#"{"duration_ms":12,"echidna_version":"2.4.0","#,
            r#"{"echidna_version":"2.4.0","duration_ms":12,"#,
            1,
        );
        assert_ne!(mutant, GOOD);
        assert_eq!(
            parse_prove_result(&mutant),
            Err(ContractError::NotCanonical)
        );
        // Whitespace is a non-canonical mutant too.
        let spaced = GOOD.replace("\"goal\":", "\"goal\": ");
        assert_eq!(
            parse_prove_result(&spaced),
            Err(ContractError::NotCanonical)
        );
    }

    /// Empty, multi-line, non-I-JSON, wrong-schema and wrong-shape inputs are rejected.
    #[test]
    fn rejects_empty_extra_lines_bad_json_schema_and_shape() {
        assert_eq!(parse_prove_result(""), Err(ContractError::Empty));
        assert_eq!(parse_prove_result("\n"), Err(ContractError::Empty));
        assert_eq!(
            parse_prove_result(&format!("notice\n{GOOD}\n")),
            Err(ContractError::ExtraOutput)
        );
        assert!(matches!(
            parse_prove_result("Proof verified successfully"),
            Err(ContractError::NotIJson(_))
        ));
        assert!(matches!(
            parse_prove_result(r#"{"a":1,"a":2}"#),
            Err(ContractError::NotIJson(_))
        ));
        assert!(matches!(
            parse_prove_result(r#"{"duration_ms":9007199254740993}"#),
            Err(ContractError::NotIJson(_))
        ));
        assert_eq!(
            parse_prove_result(&GOOD.replace("/1", "/2")),
            Err(ContractError::WrongSchema("echidna.prove.result/2".into()))
        );
        assert!(matches!(
            parse_prove_result(r#"{"status":"verified"}"#),
            Err(ContractError::Shape(_))
        ));
        assert!(matches!(
            parse_prove_result(&GOOD.replace("\"verified\"", "\"proved\"")),
            Err(ContractError::Shape(_))
        ));
    }

    /// Every contract error renders as a single non-empty line.
    #[test]
    fn errors_render_as_one_line() {
        for e in [
            ContractError::Empty,
            ContractError::ExtraOutput,
            ContractError::NotIJson("x".into()),
            ContractError::NotCanonical,
            ContractError::Shape("y".into()),
            ContractError::WrongSchema("z".into()),
        ] {
            let s = e.to_string();
            assert!(!s.is_empty() && !s.contains('\n'), "{s}");
        }
    }

    /// Help detection matches `--output` only as a whole option.
    #[test]
    fn help_detection_matches_whole_option_only() {
        assert!(help_advertises_output(
            "Options:\n      --output <OUTPUT>  Output format [default: text]"
        ));
        assert!(help_advertises_output("  -o, --output <OUTPUT>"));
        assert!(help_advertises_output("  --output=json"));
        assert!(!help_advertises_output(
            "  --output-dir <DIR>\n  --format <F>"
        ));
        assert!(!help_advertises_output(""));
    }

    /// clap's rejection of `--output` is recognised from stderr.
    #[test]
    fn rejected_flag_is_recognised_from_clap_stderr() {
        assert!(rejected_output_flag(
            "error: unexpected argument '--output' found\n"
        ));
        assert!(!rejected_output_flag("error: something else"));
    }

    /// An unrunnable binary detects as legacy; a remembered mode is reused.
    #[test]
    fn missing_binary_detects_as_legacy_and_is_cached() {
        let p = Path::new("/nonexistent/echidna-contract-detect");
        assert_eq!(detect_output_mode(p), OutputMode::Legacy);
        remember_output_mode(p, OutputMode::Contract);
        assert_eq!(detect_output_mode(p), OutputMode::Contract);
    }
}
