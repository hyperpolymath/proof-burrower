// SPDX-License-Identifier: MPL-2.0

//! Goal parsing and normalisation.
//!
//! A `Goal` is the input side of Burrower: a proof obligation we want to
//! find a home for. We don't fully parse the source language (yet) — we
//! extract the signature *tokens* (identifiers, type names, operators)
//! that the goal depends on, since these are what determine library fit.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A normalised proof goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goal {
    /// The raw input text (preserved for output).
    pub raw: String,
    /// The signature tokens — identifiers, type names, operators that
    /// appear in the goal. Set semantics: order and multiplicity are
    /// discarded for similarity scoring.
    pub tokens: BTreeSet<String>,
    /// Heuristic tag of the source language, if detectable.
    pub language: Option<String>,
}

/// Parse a raw goal string into a normalised [`Goal`].
///
/// Strategy: strip syntactic noise (keywords, punctuation), extract
/// alphanumeric+underscore identifiers, lowercase, deduplicate. Detect
/// the source language from the presence of distinguishing keywords.
pub fn parse_goal(raw: &str) -> Goal {
    let language = detect_language(raw);
    let tokens = extract_tokens(raw, language.as_deref());
    Goal {
        raw: raw.to_string(),
        tokens,
        language,
    }
}

fn detect_language(raw: &str) -> Option<String> {
    // Order matters — check the most distinctive keywords first.
    if raw.contains("theorem ") && (raw.contains(":=") || raw.contains("by ")) {
        Some("lean".to_string())
    } else if raw.contains("lemma ") && raw.contains("\\<")  {
        Some("isabelle".to_string())
    } else if raw.contains("Theorem ") || raw.contains("Lemma ") || raw.contains("Definition ") {
        Some("coq".to_string())
    } else if raw.contains("lemma ") || raw.contains("theorem ") {
        // Ambiguous Isabelle / Lean / generic — favour Isabelle since
        // most of our corpus is Isabelle.
        Some("isabelle".to_string())
    } else {
        None
    }
}

fn extract_tokens(raw: &str, _language: Option<&str>) -> BTreeSet<String> {
    // Strip Isabelle-style `\<...\>` notation by replacing with spaces.
    let cleaned = raw.replace("\\<", " ").replace("\\>", " ");

    // Common keywords across Isabelle / Coq / Lean that are pure
    // syntactic noise — drop them to focus on semantic content.
    const NOISE: &[&str] = &[
        "lemma", "theorem", "Lemma", "Theorem", "Definition", "definition",
        "proof", "qed", "by", "using", "assumes", "shows", "fixes",
        "where", "if", "then", "else", "let", "in",
        "the", "a", "an", "of", "to", "from", "for", "with",
        "Type", "Prop", "Set", "True", "False",
    ];

    raw_tokens(&cleaned)
        .into_iter()
        .filter(|t| !NOISE.contains(&t.as_str()))
        .map(|t| t.to_lowercase())
        .collect()
}

fn raw_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
            current.push(ch);
        } else {
            if !current.is_empty() && current.len() >= 2 {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && current.len() >= 2 {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_isabelle_lemma_tokens() {
        let g = parse_goal(
            "lemma trop_walks_sum_mono_subset:\n  assumes \"finite T\" \"S \\<subseteq> T\"\n  shows \"trop_walks_sum A S \\<le> trop_walks_sum A T\""
        );
        assert!(g.tokens.contains("trop_walks_sum_mono_subset"));
        assert!(g.tokens.contains("trop_walks_sum"));
        assert!(g.tokens.contains("finite"));
        // Keywords should be filtered out.
        assert!(!g.tokens.contains("lemma"));
        assert!(!g.tokens.contains("assumes"));
        assert!(!g.tokens.contains("shows"));
    }

    #[test]
    fn detects_isabelle() {
        let g = parse_goal("lemma foo: \"x \\<le> y\"");
        assert_eq!(g.language.as_deref(), Some("isabelle"));
    }

    #[test]
    fn detects_lean() {
        let g = parse_goal("theorem foo : x = y := by rfl");
        assert_eq!(g.language.as_deref(), Some("lean"));
    }

    #[test]
    fn detects_coq() {
        let g = parse_goal("Theorem foo : x = y. Proof. reflexivity. Qed.");
        assert_eq!(g.language.as_deref(), Some("coq"));
    }
}
