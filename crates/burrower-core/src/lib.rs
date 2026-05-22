// SPDX-License-Identifier: MPL-2.0

//! # Proof Burrower — core engine
//!
//! Burrower locates the *mathematical home* of a proof goal: given a goal
//! statement (e.g. an Isabelle `lemma ...` or a Lean `theorem ...`), it
//! searches a corpus of existing formal-mathematics libraries and returns
//! ranked candidate "homes" — locations in the library where the goal
//! naturally fits and where neighbouring lemmas live.
//!
//! ## Design
//!
//! Burrower is **not** a theorem prover. It does not attempt to close
//! goals — it locates them. ECHIDNA is the prover; Burrower is the
//! library-fit engine that runs *before* any prover is invoked.
//!
//! The pipeline:
//!
//! 1. [`parse_goal`] — extract a normalised goal representation (signature
//!    tokens + free identifiers) from the input string.
//! 2. [`Corpus::index`] — walk a directory of `.thy` / `.v` / `.lean`
//!    sources, extract lemma signatures, build an inverted index.
//! 3. [`Corpus::rank`] — score each indexed lemma against the goal using
//!    Jaccard similarity over signature tokens. Returns top-k matches.
//!
//! Future work: tree-edit distance on parsed AST (v2), GNN embeddings
//! once the ECHIDNA corpus training lands (v3).

pub mod goal;
pub mod corpus;
pub mod ranking;
pub mod specialist;
pub mod ledger;
pub mod attempt;
pub mod serve;
pub mod oracle;

pub use goal::{parse_goal, Goal};
pub use corpus::{Corpus, IndexedLemma, LibraryKind};
pub use ranking::{Home, rank};
pub use specialist::{
    Algebraist, Combinatorialist, ConsensusHome, OrderTheorist, Reading,
    Specialist, Swarm, Synthesis,
};
pub use ledger::{
    goal_hash, new_id, now_iso, record_reading,
    Approach, Learning, Ledger, LedgerRecord, RecordResult,
};
pub use attempt::{
    generate_probe, run_playbook, run_probe,
    AttemptResult, Playbook, ProofAttempt, ProverConfig, TacticTemplate,
};
