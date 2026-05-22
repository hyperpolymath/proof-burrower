// SPDX-License-Identifier: MPL-2.0

//! Goal-to-corpus ranking.
//!
//! Given a parsed [`Goal`] and a [`Corpus`], compute the most similar
//! lemmas — Burrower's ranked "homes" for the goal.
//!
//! v0.1 uses Jaccard similarity over signature tokens. This is crude
//! but gives a useful baseline: lemmas that share many identifiers and
//! type names with the goal score high. The relevant signal in formal
//! mathematics is overwhelmingly carried by symbol overlap.
//!
//! Future versions:
//! - v0.2: weight tokens by inverse document frequency (rare tokens
//!         carry more signal).
//! - v0.3: tree-edit distance on parsed AST (requires real parsers).
//! - v0.4: GNN embeddings (requires the trained model that ECHIDNA's
//!         corpus is waiting for).

use crate::corpus::{Corpus, IndexedLemma};
use crate::goal::Goal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One candidate "home" for a goal: an indexed lemma plus its score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Home {
    pub lemma: IndexedLemma,
    /// Jaccard similarity in `[0.0, 1.0]`.
    pub score: f64,
    /// The intersection of goal tokens and lemma tokens — useful for
    /// explaining *why* this lemma was ranked.
    pub shared_tokens: BTreeSet<String>,
}

/// Rank corpus lemmas against a goal, returning the top `k`.
pub fn rank(goal: &Goal, corpus: &Corpus, k: usize) -> Vec<Home> {
    let mut scored: Vec<Home> = corpus
        .lemmas
        .iter()
        .filter_map(|lemma| {
            let shared: BTreeSet<String> =
                goal.tokens.intersection(&lemma.tokens).cloned().collect();
            let union_count =
                goal.tokens.union(&lemma.tokens).count();
            if union_count == 0 {
                return None;
            }
            let score = shared.len() as f64 / union_count as f64;
            if score == 0.0 {
                return None;
            }
            Some(Home {
                lemma: lemma.clone(),
                score,
                shared_tokens: shared,
            })
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::LibraryKind;
    use crate::goal::parse_goal;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn mk_lemma(name: &str, tokens: &[&str]) -> IndexedLemma {
        IndexedLemma {
            name: name.to_string(),
            statement: format!("lemma {name}: ..."),
            file: PathBuf::from("test"),
            line: 1,
            kind: LibraryKind::Isabelle,
            tokens: tokens.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        }
    }

    #[test]
    fn ranks_more_overlap_higher() {
        let goal = parse_goal("lemma foo: \"trop_walks_sum A S \\<le> trop_walks_sum A T\"");
        let corpus = Corpus {
            lemmas: vec![
                mk_lemma("trop_walks_sum_ge_member", &["trop_walks_sum", "path_weight"]),
                mk_lemma("nat_add_comm", &["nat", "add", "comm"]),
                mk_lemma(
                    "trop_walks_sum_mono",
                    &["trop_walks_sum", "mono", "subset"],
                ),
            ],
        };
        let ranked = rank(&goal, &corpus, 3);
        assert!(!ranked.is_empty());
        // Both trop_walks_sum lemmas should rank above nat_add_comm.
        assert!(ranked[0].lemma.name.starts_with("trop_walks_sum"));
    }
}
