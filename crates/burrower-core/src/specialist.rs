// SPDX-License-Identifier: PMPL-1.0-or-later

//! # Specialists — the agent swarm
//!
//! A `Specialist` is a domain expert: an agent that recognises a
//! particular kind of mathematical structure (algebraic, order-theoretic,
//! topological, combinatorial, ...) and locates candidate homes for a
//! goal *within its domain*.
//!
//! The architecture is designed for extensibility: new specialists can
//! be defined as Rust types implementing [`Specialist`], or (eventually)
//! authored in **007** — the agent-language — and compiled to
//! specialist instances. 007 is "agents that make agents"; Burrower
//! is the host where those agents collaborate as a coordinated proof
//! swarm. See `docs/AGENT-DSL.adoc` for the integration plan.
//!
//! ## Today
//!
//! Three seed agents are provided in this module:
//!
//! - [`Algebraist`] — recognises algebraic structure (groups, rings,
//!   semirings, monoids, ideals, homomorphisms).
//! - [`OrderTheorist`] — recognises order/lattice structure
//!   (≤, sup, inf, monotone, fixpoint, well-founded).
//! - [`Combinatorialist`] — recognises finite/discrete structure
//!   (sums over finite sets, walks, permutations, combinatorial
//!   identities).
//!
//! Together they cover the structural surface of a goal like
//! `trop_walks_sum_mono_subset`:
//! * Algebraist sees a tropical-semiring monotonicity property.
//! * OrderTheorist sees monotonicity under set inclusion.
//! * Combinatorialist sees a sum-bound over a walk set.
//!
//! Each specialist contributes a *reading* of the goal plus a list
//! of candidate homes from its own corpus subset.
//!
//! ## Tomorrow
//!
//! Planned specialists: Topologist, CategoryTheorist, Logician,
//! NumberTheorist, Geometer, Probabilist, PhilosopherOfMath
//! (foundational reading), Constructivist (proof-theoretic strength).

use crate::attempt::{run_playbook, Playbook, ProofAttempt, ProverConfig, TacticTemplate};
use crate::corpus::{Corpus, IndexedLemma};
use crate::goal::Goal;
use crate::ledger::{record_reading, Ledger};
use crate::ranking::{rank, Home};
use serde::{Deserialize, Serialize};

/// One specialist's reading of a goal: domain identification, relevance
/// score, and ranked candidate homes from the specialist's corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reading {
    /// Specialist name (e.g. "Algebraist").
    pub specialist: String,
    /// Domain (e.g. "Algebra").
    pub domain: String,
    /// Relevance of this specialist to the goal, in `[0.0, 1.0]`.
    /// Specialists below the swarm threshold do not contribute homes.
    pub relevance: f64,
    /// One-line description of how this specialist *reads* the goal.
    pub reading: String,
    /// Ranked candidate homes from the specialist's corpus subset.
    pub homes: Vec<Home>,
}

/// The Specialist trait. A specialist is a domain expert that can
/// score its own relevance to a goal and locate candidate homes.
pub trait Specialist {
    fn name(&self) -> &'static str;
    fn domain(&self) -> &'static str;
    /// Domain keywords — used for both relevance scoring and corpus
    /// filtering. A lemma is "in this specialist's domain" if its
    /// tokens overlap these keywords.
    fn keywords(&self) -> &'static [&'static str];
    /// One-line reading of the goal in this specialist's vocabulary.
    /// Uses the SAME prefix/substring matching as relevance() so the
    /// reading and the score agree.
    fn read(&self, goal: &Goal) -> String {
        let kws: Vec<String> = self.keywords().iter().map(|s| s.to_lowercase()).collect();
        let mut witnesses: Vec<&String> = Vec::new();
        for kw in &kws {
            if let Some(t) = goal.tokens.iter().find(|t| {
                *t == kw || t.starts_with(&format!("{}_", kw)) || t.contains(kw)
            }) {
                witnesses.push(t);
            }
            if witnesses.len() >= 5 { break; }
        }
        if witnesses.is_empty() {
            format!(
                "no recognised {} structure in this goal",
                self.domain().to_lowercase()
            )
        } else {
            format!(
                "{} structure detected via {:?}",
                self.domain(),
                witnesses
            )
        }
    }
    /// Default relevance: how many of the specialist's keywords appear
    /// in the goal — either as an exact token, as a prefix of a token
    /// (e.g. `trop` matches `trop_walks_sum`), or as a substring of a
    /// token (e.g. `walk` matches `walks`). Normalised by keyword count
    /// so the score lies in `[0.0, 1.0]`.
    fn relevance(&self, goal: &Goal) -> f64 {
        let kws = self.keywords();
        if kws.is_empty() {
            return 0.0;
        }
        let hits = kws
            .iter()
            .filter(|kw| {
                let kw_lc = kw.to_lowercase();
                goal.tokens.iter().any(|t| {
                    t == &kw_lc
                        || t.starts_with(&format!("{}_", kw_lc))
                        || t.contains(&kw_lc)
                })
            })
            .count();
        hits as f64 / kws.len() as f64
    }
    /// Locate candidate homes from the specialist's corpus subset.
    fn locate(&self, goal: &Goal, corpus: &Corpus, top: usize) -> Vec<Home> {
        let domain_corpus = self.filter(corpus);
        rank(goal, &domain_corpus, top)
    }
    /// Filter the corpus to only lemmas in this specialist's domain.
    /// More discriminating than relevance: a lemma must hit at least
    /// `MIN_DOMAIN_HITS` distinct keywords to count as "in domain".
    /// This makes specialists actually differentiate the corpus.
    fn filter(&self, corpus: &Corpus) -> Corpus {
        let kws: Vec<String> = self.keywords().iter().map(|s| s.to_lowercase()).collect();
        let lemmas: Vec<IndexedLemma> = corpus
            .lemmas
            .iter()
            .filter(|l| {
                let hits = kws
                    .iter()
                    .filter(|kw| {
                        l.tokens.iter().any(|t| {
                            *t == **kw
                                || t.starts_with(&format!("{}_", kw))
                                || t.contains(kw.as_str())
                        })
                    })
                    .count();
                hits >= MIN_DOMAIN_HITS
            })
            .cloned()
            .collect();
        Corpus { lemmas }
    }
    /// Produce a full Reading.
    fn read_goal(&self, goal: &Goal, corpus: &Corpus, top: usize) -> Reading {
        Reading {
            specialist: self.name().to_string(),
            domain: self.domain().to_string(),
            relevance: self.relevance(goal),
            reading: self.read(goal),
            homes: if self.relevance(goal) >= SWARM_RELEVANCE_THRESHOLD {
                self.locate(goal, corpus, top)
            } else {
                Vec::new()
            },
        }
    }

    /// The specialist's playbook — domain-specific tactics it will
    /// try when actively attempting a proof. Default is empty;
    /// concrete specialists override.
    fn playbook(&self) -> Playbook {
        Playbook {
            specialist: self.name().to_string(),
            tactics: Vec::new(),
        }
    }
}

/// Below this relevance score a specialist does not contribute homes.
/// Tunable; 0.02 means roughly "at least one keyword overlaps."
pub const SWARM_RELEVANCE_THRESHOLD: f64 = 0.02;

/// A lemma must hit this many distinct domain keywords to count as
/// "in" a specialist's domain. Higher = more discriminating filter.
pub const MIN_DOMAIN_HITS: usize = 2;

// ---------------------------------------------------------------------
// Seed specialists
// ---------------------------------------------------------------------

pub struct Algebraist;
impl Specialist for Algebraist {
    fn name(&self) -> &'static str { "Algebraist" }
    fn domain(&self) -> &'static str { "Algebra" }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "group", "ring", "field", "semiring", "monoid", "semigroup",
            "ideal", "module", "vector", "homomorphism", "isomorphism",
            "kernel", "image", "quotient", "polynomial", "matrix",
            "abelian", "commutative", "associative", "distributive",
            "identity", "inverse", "comm_monoid", "comm_semiring",
            "tropical", "trop", "tropm", "plus", "mult", "add", "mul",
            "zero", "one", "neginf", "fin",
        ]
    }
    fn playbook(&self) -> Playbook {
        Playbook {
            specialist: "Algebraist".to_string(),
            tactics: vec![
                TacticTemplate {
                    name: "simp".to_string(),
                    script: "by simp".to_string(),
                    description: "Isabelle simplifier — closes equational arithmetic goals.".to_string(),
                },
                TacticTemplate {
                    name: "auto".to_string(),
                    script: "by auto".to_string(),
                    description: "Combined simp+blast+intro — broader than simp alone.".to_string(),
                },
                TacticTemplate {
                    name: "algebra-simps".to_string(),
                    script: "by (simp add: algebra_simps)".to_string(),
                    description: "Simplifier with algebra rewriting rules.".to_string(),
                },
                TacticTemplate {
                    name: "ring-arith".to_string(),
                    script: "by (simp add: field_simps)".to_string(),
                    description: "Field/ring arithmetic simplification.".to_string(),
                },
                TacticTemplate {
                    name: "ac-simps".to_string(),
                    script: "by (simp add: ac_simps)".to_string(),
                    description: "Associativity + commutativity rewriting — replaces looping metis chains on +/* monoids.".to_string(),
                },
                TacticTemplate {
                    name: "semiring-distrib".to_string(),
                    script: "by (simp add: distrib_left distrib_right)".to_string(),
                    description: "Both-sided distributivity for semiring goals.".to_string(),
                },
                TacticTemplate {
                    name: "metis-empty".to_string(),
                    script: "by metis".to_string(),
                    description: "Equational reasoning without hints — succeeds where simp loses orientation.".to_string(),
                },
                TacticTemplate {
                    name: "linarith-after-simp".to_string(),
                    script: "by (simp; linarith)".to_string(),
                    description: "Simplifier reduction followed by linear arithmetic — Isabelle 2025-1 omega→linarith drift fallback.".to_string(),
                },
                TacticTemplate {
                    name: "comm-monoid-add".to_string(),
                    script: "by (simp add: ac_simps add.commute)".to_string(),
                    description: "Commutative-monoid rewriting (add.commute is the post-rebrand name).".to_string(),
                },
                TacticTemplate {
                    name: "transfer-then-simp".to_string(),
                    script: "by (transfer, simp add: algebra_simps)".to_string(),
                    description: "Transfer to representation type then simplify — useful for tropical/quotient goals.".to_string(),
                },
            ],
        }
    }
}

pub struct OrderTheorist;
impl Specialist for OrderTheorist {
    fn name(&self) -> &'static str { "OrderTheorist" }
    fn domain(&self) -> &'static str { "Order Theory" }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "le", "leq", "lt", "ge", "geq", "gt", "subseteq", "subset",
            "linorder", "preorder", "order_bot", "order_top",
            "lattice", "complete_lattice", "sup", "inf",
            "monotone", "mono", "antitone", "cofinal",
            "fixpoint", "lfp", "gfp", "least", "greatest",
            "well_founded", "well_order", "chain", "directed",
            "max", "min", "supremum", "infimum",
            "absorb", "idempotent",
        ]
    }
    fn playbook(&self) -> Playbook {
        Playbook {
            specialist: "OrderTheorist".to_string(),
            tactics: vec![
                TacticTemplate {
                    name: "simp".to_string(),
                    script: "by simp".to_string(),
                    description: "Simplifier on basic order facts.".to_string(),
                },
                TacticTemplate {
                    name: "order-trans".to_string(),
                    script: "by (rule order_trans)".to_string(),
                    description: "Transitivity of ≤ (Isabelle 2025-1 canonical name; le_trans was deprecated).".to_string(),
                },
                TacticTemplate {
                    name: "order-auto".to_string(),
                    script: "by (auto intro: order_trans)".to_string(),
                    description: "Auto with transitivity hint.".to_string(),
                },
                TacticTemplate {
                    name: "subset-decompose".to_string(),
                    script: "by (auto simp: subset_iff)".to_string(),
                    description: "Element-wise subset reasoning.".to_string(),
                },
                TacticTemplate {
                    name: "linarith".to_string(),
                    script: "by linarith".to_string(),
                    description: "Linear arithmetic decision procedure — replaces omega in Isabelle 2025-1.".to_string(),
                },
                TacticTemplate {
                    name: "sum-mono".to_string(),
                    script: "by (rule sum_mono) auto".to_string(),
                    description: "Pointwise monotonicity of sums (Kleene 237 pattern).".to_string(),
                },
                TacticTemplate {
                    name: "monoI-then-auto".to_string(),
                    script: "by (intro monoI) auto".to_string(),
                    description: "Introduce monotonicity then resolve elementwise.".to_string(),
                },
                TacticTemplate {
                    name: "force".to_string(),
                    script: "by force".to_string(),
                    description: "Stronger than blast for goals needing list/sequence decomposition (Kleene 403 pattern: [v] vs v#[]).".to_string(),
                },
                TacticTemplate {
                    name: "meson-order".to_string(),
                    script: "by (meson order_trans)".to_string(),
                    description: "Meson with transitive chain — succeeds on multi-hop ≤ goals where blast loops.".to_string(),
                },
                TacticTemplate {
                    name: "le-supI".to_string(),
                    script: "by (auto intro: le_supI1 le_supI2)".to_string(),
                    description: "Sup introduction on either side — for goals against a join.".to_string(),
                },
                TacticTemplate {
                    name: "subset-trans".to_string(),
                    script: "by (meson subset_trans)".to_string(),
                    description: "Subset transitivity chain — replaces blast loops on nested ⊆ obligations.".to_string(),
                },
                TacticTemplate {
                    name: "order-trans-OF".to_string(),
                    script: "by (rule order_trans[OF assms]) simp".to_string(),
                    description: "Apply transitivity with the assumption pre-substituted — often the missing instantiation.".to_string(),
                },
            ],
        }
    }
}

pub struct Combinatorialist;
impl Specialist for Combinatorialist {
    fn name(&self) -> &'static str { "Combinatorialist" }
    fn domain(&self) -> &'static str { "Combinatorics" }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "finite", "card", "sum", "prod", "card_eq",
            "set", "list", "seq", "permutation", "perm",
            "walk", "walks", "path", "path_weight", "cycle",
            "graph", "vertex", "edge", "tree", "forest",
            "n_choose_k", "binomial", "factorial",
            "induction", "induct", "base", "step",
            "sigma", "union", "insert", "filter",
        ]
    }
    fn playbook(&self) -> Playbook {
        Playbook {
            specialist: "Combinatorialist".to_string(),
            tactics: vec![
                TacticTemplate {
                    name: "simp".to_string(),
                    script: "by simp".to_string(),
                    description: "Simplifier on finite-set facts.".to_string(),
                },
                TacticTemplate {
                    name: "auto".to_string(),
                    script: "by auto".to_string(),
                    description: "General-purpose first attempt.".to_string(),
                },
                TacticTemplate {
                    name: "induction-finite".to_string(),
                    script: "by (induction rule: finite_induct) auto".to_string(),
                    description: "Modern finite-set induction (NB: induction not induct).".to_string(),
                },
                TacticTemplate {
                    name: "induction-list".to_string(),
                    script: "by (induction xs) auto".to_string(),
                    description: "Structural induction on a list named xs.".to_string(),
                },
                TacticTemplate {
                    name: "induction-arbitrary".to_string(),
                    script: "by (induction k arbitrary: j) auto".to_string(),
                    description: "Generalised induction (Matrices_Full helper pattern: arbitrary: j keeps the IH usable across positions).".to_string(),
                },
                TacticTemplate {
                    name: "cases-list".to_string(),
                    script: "by (cases w; auto)".to_string(),
                    description: "Case-split on list constructors then auto — the list-non-empty pattern.".to_string(),
                },
                TacticTemplate {
                    name: "sum-insert".to_string(),
                    script: "by (subst sum.insert) auto".to_string(),
                    description: "Step a sum at an insert (replaces sum.atLeastAtMost_Suc, deprecated in 2025-1).".to_string(),
                },
                TacticTemplate {
                    name: "sum-distrib".to_string(),
                    script: "by (simp add: sum.distrib)".to_string(),
                    description: "Sum distributivity (replaces sum_add_distrib, renamed in 2025-1).".to_string(),
                },
                TacticTemplate {
                    name: "unfolding-walks-def".to_string(),
                    script: "unfolding walks_def by simp".to_string(),
                    description: "Unfold walks_def then simp — beats `proof (intro conjI)` because walks_def yields a Collect set.".to_string(),
                },
                TacticTemplate {
                    name: "metis-append".to_string(),
                    script: "by (metis append_Cons append_Nil)".to_string(),
                    description: "List concatenation lemmas — closes obtain-decompositions of [v]@ys vs v#ys (Kleene 403 pattern).".to_string(),
                },
                TacticTemplate {
                    name: "force".to_string(),
                    script: "by force".to_string(),
                    description: "Stronger than blast for list-decomposition obtain goals.".to_string(),
                },
                TacticTemplate {
                    name: "permutes-in-image".to_string(),
                    script: "by (rule permutes_in_image[OF assms])".to_string(),
                    description: "Permutations as image-preserving — closes Det 153/207 type-class regression goals.".to_string(),
                },
                TacticTemplate {
                    name: "transposition-2x2".to_string(),
                    script: "by (cases a; cases b) (auto simp: Transposition.transpose)".to_string(),
                    description: "2x2 case split using Transposition.transpose (Fun.swap is input-only in 2025-1).".to_string(),
                },
            ],
        }
    }
}

// ---------------------------------------------------------------------
// Swarm
// ---------------------------------------------------------------------

/// A swarm of specialists. Routes a goal to all relevant specialists
/// in parallel (today: sequential; tomorrow: rayon / actor-model).
pub struct Swarm {
    specialists: Vec<Box<dyn Specialist>>,
}

impl Default for Swarm {
    fn default() -> Self {
        Self {
            specialists: vec![
                Box::new(Algebraist),
                Box::new(OrderTheorist),
                Box::new(Combinatorialist),
            ],
        }
    }
}

impl Swarm {
    pub fn new() -> Self { Self::default() }

    /// Add a specialist. Used by the planned 007 loader to register
    /// dynamically-defined agents.
    pub fn add(&mut self, s: Box<dyn Specialist>) {
        self.specialists.push(s);
    }

    /// Run all specialists. Returns one Reading per specialist (even
    /// if relevance is low, so the caller can see who *didn't* engage
    /// — a useful negative signal).
    pub fn route(&self, goal: &Goal, corpus: &Corpus, top: usize) -> Vec<Reading> {
        self.route_with_ledger(goal, corpus, top, None)
    }

    /// Run all specialists; if a ledger is supplied, append one record
    /// per Reading. This is the operational heart of the
    /// trial-and-error multiplier: every approach an agent takes is
    /// persisted, so subsequent agents (on later goals) can build on
    /// or learn from it.
    pub fn route_with_ledger(
        &self,
        goal: &Goal,
        corpus: &Corpus,
        top: usize,
        ledger: Option<&Ledger>,
    ) -> Vec<Reading> {
        let mut readings: Vec<Reading> = self
            .specialists
            .iter()
            .map(|s| s.read_goal(goal, corpus, top))
            .collect();
        readings.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(l) = ledger {
            for r in &readings {
                let rec = record_reading(&r.specialist, &goal.raw, &r.reading, r.relevance);
                if let Err(e) = l.append(&rec) {
                    eprintln!("warning: ledger append failed for {}: {e}", r.specialist);
                }
            }
        }
        readings
    }

    /// Run every engaged specialist's playbook against the goal.
    /// Returns one [`ProofAttempt`] per (specialist, tactic) pair.
    /// All attempts are recorded to the ledger if one is supplied.
    ///
    /// This is where Burrower stops being a router and starts being
    /// a worker: specialists actually try proofs, the ledger records
    /// what worked, the swarm gets smarter over time.
    pub fn attempt_all(
        &self,
        goal: &Goal,
        prover: &ProverConfig,
        ledger: Option<&Ledger>,
    ) -> Vec<ProofAttempt> {
        let mut all = Vec::new();
        for s in &self.specialists {
            if s.relevance(goal) < SWARM_RELEVANCE_THRESHOLD {
                continue;
            }
            let playbook = s.playbook();
            if playbook.tactics.is_empty() {
                continue;
            }
            match run_playbook(goal, &playbook, prover, ledger) {
                Ok(mut attempts) => all.append(&mut attempts),
                Err(e) => eprintln!("warning: playbook failed for {}: {e}", s.name()),
            }
        }
        all
    }

    /// Synthesise the readings into a single picture. The "head agent":
    /// reads all specialist reports, identifies which domains co-engaged
    /// (boundary-object signal), and picks consensus homes (lemmas that
    /// multiple specialists ranked highly).
    pub fn synthesise(&self, readings: &[Reading]) -> Synthesis {
        let engaged: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.relevance >= SWARM_RELEVANCE_THRESHOLD)
            .collect();

        // Tally home appearances across engaged specialists. A home cited
        // by multiple specialists is a stronger signal than one specialist
        // alone — that's the "consensus home".
        use std::collections::HashMap;
        let mut tally: HashMap<String, (usize, f64, Option<&Home>)> = HashMap::new();
        for r in &engaged {
            for h in &r.homes {
                let entry = tally.entry(h.lemma.name.clone()).or_insert((0, 0.0, None));
                entry.0 += 1;
                entry.1 += h.score * r.relevance;
                if entry.2.is_none() {
                    entry.2 = Some(h);
                }
            }
        }
        let mut consensus: Vec<ConsensusHome> = tally
            .into_iter()
            .filter_map(|(name, (votes, weighted, home_opt))| {
                home_opt.map(|h| ConsensusHome {
                    lemma_name: name,
                    file: h.lemma.file.display().to_string(),
                    line: h.lemma.line,
                    votes,
                    weighted_score: weighted,
                })
            })
            .collect();
        consensus.sort_by(|a, b| {
            b.votes
                .cmp(&a.votes)
                .then_with(|| b.weighted_score.partial_cmp(&a.weighted_score).unwrap_or(std::cmp::Ordering::Equal))
        });

        let domains: Vec<String> = engaged.iter().map(|r| r.domain.clone()).collect();
        let boundary = domains.len() >= 2;
        let summary = if engaged.is_empty() {
            "No specialist engaged. This goal may be a novel construction with no recognised mathematical home.".to_string()
        } else if boundary {
            format!(
                "Boundary object: this goal lives in the intersection of {}. Cross-domain readings often suggest the deepest results.",
                domains.join(", ")
            )
        } else {
            format!(
                "Single-domain object: best read as {}.",
                domains[0]
            )
        };

        Synthesis {
            engaged_specialists: engaged.iter().map(|r| r.specialist.clone()).collect(),
            domains,
            boundary_object: boundary,
            summary,
            consensus_homes: consensus.into_iter().take(10).collect(),
        }
    }
}

/// A consensus home — a lemma cited by multiple specialists, with vote
/// count and weighted score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusHome {
    pub lemma_name: String,
    pub file: String,
    pub line: usize,
    pub votes: usize,
    pub weighted_score: f64,
}

/// The synthesised "head-agent" read of a swarm response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Synthesis {
    pub engaged_specialists: Vec<String>,
    pub domains: Vec<String>,
    pub boundary_object: bool,
    pub summary: String,
    pub consensus_homes: Vec<ConsensusHome>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::parse_goal;

    #[test]
    fn algebraist_recognises_tropical_goal() {
        let g = parse_goal("lemma trop_walks_sum_mono: trop_walks_sum A S \\<le> trop_walks_sum A T");
        let s = Algebraist;
        assert!(s.relevance(&g) > 0.0, "should engage on tropical content");
    }

    #[test]
    fn order_theorist_recognises_le_subseteq() {
        let g = parse_goal("lemma foo: \"S \\<subseteq> T \\<Longrightarrow> f S \\<le> f T\"");
        let s = OrderTheorist;
        assert!(s.relevance(&g) > 0.0, "should engage on ≤ + ⊆");
    }

    #[test]
    fn combinatorialist_recognises_walks() {
        let g = parse_goal("lemma foo: \"finite (walks n k i j)\"");
        let s = Combinatorialist;
        assert!(s.relevance(&g) > 0.0, "should engage on walks + finite");
    }

    #[test]
    fn playbooks_expanded_2026_04_26() {
        // After the swarm-dogfood session against Tropical_Semirings, each
        // playbook was expanded with proof-repair patterns observed in real
        // failure sites (cycle-excise, Det 153/207, Kleene 403, etc.) plus
        // Isabelle 2025-1 drift fixes (omega→linarith, sum.atLeastAtMost_Suc
        // deprecation, Fun.swap input-only). Lock in the new minimums so
        // future shrink-regressions are caught.
        assert!(Algebraist.playbook().tactics.len() >= 10,
            "Algebraist playbook should have ≥10 tactics post-2026-04-26 expansion");
        assert!(OrderTheorist.playbook().tactics.len() >= 12,
            "OrderTheorist playbook should have ≥12 tactics post-2026-04-26 expansion");
        assert!(Combinatorialist.playbook().tactics.len() >= 12,
            "Combinatorialist playbook should have ≥12 tactics post-2026-04-26 expansion");
    }

    #[test]
    fn combinatorialist_carries_session_repair_patterns() {
        // The Tropical_Semirings session-close added these specific tactics
        // because real failures pointed at them. They must stay named-and-
        // findable so a future ledger entry can cite them by name.
        let names: Vec<String> = Combinatorialist.playbook().tactics
            .iter().map(|t| t.name.clone()).collect();
        assert!(names.iter().any(|n| n == "permutes-in-image"),
            "Combinatorialist must carry the Det 153/207 fix");
        assert!(names.iter().any(|n| n == "metis-append"),
            "Combinatorialist must carry the Kleene 403 list-decomp fix");
        assert!(names.iter().any(|n| n == "induction-arbitrary"),
            "Combinatorialist must carry the Matrices_Full generalised-induction pattern");
    }

    #[test]
    fn order_theorist_carries_2025_drift_fixes() {
        let names: Vec<String> = OrderTheorist.playbook().tactics
            .iter().map(|t| t.name.clone()).collect();
        assert!(names.iter().any(|n| n == "linarith"),
            "OrderTheorist must carry linarith (omega→linarith drift in 2025-1)");
        assert!(names.iter().any(|n| n == "force"),
            "OrderTheorist must carry force (Kleene 403 list-decomp fallback)");
    }

    #[test]
    fn algebraist_carries_ac_simps_replacement() {
        let names: Vec<String> = Algebraist.playbook().tactics
            .iter().map(|t| t.name.clone()).collect();
        assert!(names.iter().any(|n| n == "ac-simps"),
            "Algebraist must carry ac-simps (replaces looping metis chains)");
    }

    #[test]
    fn swarm_routes_to_relevant_specialists_first() {
        let g = parse_goal("lemma foo: \"finite T \\<Longrightarrow> trop_walks_sum A S \\<le> trop_walks_sum A T\"");
        let swarm = Swarm::new();
        let corpus = Corpus::default();
        let readings = swarm.route(&g, &corpus, 3);
        assert_eq!(readings.len(), 3);
        // First reading should be the most relevant.
        assert!(readings[0].relevance >= readings[1].relevance);
        assert!(readings[1].relevance >= readings[2].relevance);
    }
}
