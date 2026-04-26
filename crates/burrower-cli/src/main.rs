// SPDX-License-Identifier: PMPL-1.0-or-later

//! Proof Burrower CLI.
//!
//! Two subcommands:
//!
//! - `burrower index <corpus-root> --output <index.json>` — walk a
//!   library directory and persist a JSON index.
//! - `burrower find <goal-string> --index <index.json> [--top 10]` —
//!   given a goal, return ranked candidate "homes".

use anyhow::Result;
use burrower_core::{parse_goal, rank, Corpus, Ledger, ProverConfig, Swarm};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "burrower",
    about = "Find the mathematical home of a proof goal.",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a corpus directory.
    Index {
        /// Root directory to index (recursively).
        root: PathBuf,
        /// Output index file (JSON).
        #[arg(long)]
        output: PathBuf,
    },
    /// Locate the home of a proof goal (single-ranker, no swarm).
    Find {
        /// The proof goal as a string. Quote it.
        goal: String,
        /// Path to a previously-built index (see `index` subcommand).
        #[arg(long)]
        index: PathBuf,
        /// Number of candidate homes to return.
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Output format: `text` (default) or `json`.
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Route a goal through the specialist swarm.
    ///
    /// Each specialist scores its relevance, gives a one-line reading
    /// of the goal in its vocabulary, and (if relevant) returns ranked
    /// candidate homes from its corpus subset. The synthesis layer
    /// then identifies boundary objects and consensus homes.
    ///
    /// If `--ledger` is supplied, every reading is appended to the
    /// Burrow Ledger — the shared knowledge layer the swarm uses to
    /// build on and learn from prior work.
    Swarm {
        /// The proof goal as a string. Quote it.
        goal: String,
        /// Path to a previously-built index.
        #[arg(long)]
        index: PathBuf,
        /// Top-k homes per specialist.
        #[arg(long, default_value_t = 5)]
        top: usize,
        /// Output format: `text` (default) or `json`.
        #[arg(long, default_value = "text")]
        format: String,
        /// Optional path to the Burrow Ledger for auto-recording.
        #[arg(long)]
        ledger: Option<PathBuf>,
    },
    /// Run every engaged specialist's playbook against the goal —
    /// actually attempt proofs through ECHIDNA. Each (specialist,
    /// tactic) attempt is recorded in the ledger.
    ///
    /// Today: dispatches to ECHIDNA's Isabelle backend. The probe
    /// file uses `imports Main` only; goals referencing external
    /// theories will fail (which is itself useful data for the
    /// ledger).
    Attempt {
        /// The proof goal as a string. Quote it.
        goal: String,
        /// Path to the echidna binary.
        #[arg(long, default_value = "/var/mnt/eclipse/repos/echidna/target/debug/echidna")]
        echidna: PathBuf,
        /// Per-attempt timeout (seconds), passed to `echidna prove -t`.
        #[arg(long, default_value_t = 60)]
        timeout: u32,
        /// Append every attempt to this ledger file.
        #[arg(long)]
        ledger: PathBuf,
        /// Output format: `text` (default) or `json`.
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Burrow Ledger inspection.
    Ledger {
        #[command(subcommand)]
        sub: LedgerCmd,
    },
}

#[derive(Subcommand)]
enum LedgerCmd {
    /// Show the most-recent N records.
    Recent {
        #[arg(long)]
        path: PathBuf,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Records authored by a specialist.
    BySpecialist {
        #[arg(long)]
        path: PathBuf,
        name: String,
    },
    /// Anti-patterns visible to a specialist (i.e. things to avoid).
    AntiPatterns {
        #[arg(long)]
        path: PathBuf,
        /// Specialist requesting the visibility view.
        #[arg(long)]
        for_specialist: String,
    },
    /// Aggregate digest: counts per specialist, top patterns.
    Digest {
        #[arg(long)]
        path: PathBuf,
    },
}

fn handle_ledger(cmd: LedgerCmd) -> Result<()> {
    use std::collections::BTreeMap;
    match cmd {
        LedgerCmd::Recent { path, limit } => {
            let l = Ledger::open(&path)?;
            let recs = l.recent(limit)?;
            println!("Burrow Ledger — last {} record(s) (newest first):\n", recs.len());
            for r in recs {
                println!("[{}] {} · {}", r.timestamp, r.specialist, r.goal_hash);
                println!("    goal: {}", r.goal_excerpt);
                if let Some(a) = &r.approach {
                    println!("    approach: {}", a.description);
                }
                if let Some(rs) = &r.result {
                    println!("    result [{}]: {}", rs.status, rs.explanation);
                }
                if let Some(le) = &r.learning {
                    println!(
                        "    learning [{}]: {} ⇒ {}",
                        le.pattern_kind, le.pattern_extracted, le.generalisation
                    );
                }
                println!();
            }
        }
        LedgerCmd::BySpecialist { path, name } => {
            let l = Ledger::open(&path)?;
            let recs = l.by_specialist(&name)?;
            println!("Records by {}: {}", name, recs.len());
            for r in recs {
                println!(
                    "  [{}] {} — {}",
                    r.timestamp,
                    r.goal_hash,
                    r.goal_excerpt.chars().take(80).collect::<String>()
                );
            }
        }
        LedgerCmd::AntiPatterns { path, for_specialist } => {
            let l = Ledger::open(&path)?;
            let antis = l.anti_patterns_for(&for_specialist)?;
            println!(
                "Anti-patterns visible to {}: {}",
                for_specialist,
                antis.len()
            );
            for a in antis {
                println!(
                    "  · {} — {}",
                    a.pattern_extracted,
                    a.generalisation
                );
            }
        }
        LedgerCmd::Digest { path } => {
            let l = Ledger::open(&path)?;
            let recs = l.read_all()?;
            let mut by_spec: BTreeMap<String, usize> = BTreeMap::new();
            let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
            let mut patterns: BTreeMap<String, usize> = BTreeMap::new();
            for r in &recs {
                *by_spec.entry(r.specialist.clone()).or_insert(0) += 1;
                if let Some(rs) = &r.result {
                    *by_status.entry(rs.status.clone()).or_insert(0) += 1;
                }
                if let Some(le) = &r.learning {
                    *patterns.entry(le.pattern_extracted.clone()).or_insert(0) += 1;
                }
            }
            println!("Burrow Ledger digest — {} record(s):\n", recs.len());
            println!("By specialist:");
            for (k, v) in &by_spec {
                println!("  {:>20}  {}", k, v);
            }
            println!("\nBy result status:");
            for (k, v) in &by_status {
                println!("  {:>20}  {}", k, v);
            }
            println!("\nTop patterns (cited):");
            let mut sorted: Vec<_> = patterns.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));
            for (k, v) in sorted.iter().take(10) {
                println!("  {:>4}× {}", v, k);
            }
        }
    }
    Ok(())
}

fn handle_attempt(
    goal: String,
    echidna: PathBuf,
    timeout: u32,
    ledger: PathBuf,
    format: String,
) -> Result<()> {
    let parsed = parse_goal(&goal);
    let swarm = Swarm::new();
    let l = Ledger::open(&ledger)?;
    let prover = ProverConfig {
        echidna_path: echidna,
        timeout_secs: timeout,
        workdir: None,
    };
    let attempts = swarm.attempt_all(&parsed, &prover, Some(&l));

    match format.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&attempts)?),
        _ => {
            println!(
                "Burrower attempt — {} attempt(s) across {} specialist(s):\n",
                attempts.len(),
                attempts
                    .iter()
                    .map(|a| &a.specialist)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            );
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            let mut skipped = 0usize;
            for a in &attempts {
                let badge = if a.result.is_success() { "✓" } else { "✗" };
                println!(
                    "  {} [{}] {} → {}",
                    badge,
                    a.specialist,
                    a.tactic_name,
                    a.result.status_string()
                );
                match a.result.is_success() {
                    true => succeeded += 1,
                    false => match &a.result {
                        burrower_core::AttemptResult::Skipped { .. } => skipped += 1,
                        _ => failed += 1,
                    },
                }
            }
            println!(
                "\nSummary: {} succeeded · {} failed · {} skipped",
                succeeded, failed, skipped
            );
            if succeeded == 0 && failed > 0 {
                println!(
                    "\nNo tactic in any specialist's playbook closed this goal. \
                     Anti-patterns recorded in {}.",
                    ledger.display()
                );
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Attempt { goal, echidna, timeout, ledger, format } => {
            return handle_attempt(goal, echidna, timeout, ledger, format)
        }
        Cmd::Ledger { sub } => return handle_ledger(sub),
        Cmd::Index { root, output } => {
            let corpus = Corpus::index(&root)?;
            corpus.save(&output)?;
            eprintln!(
                "Indexed {} lemmas from {} into {}",
                corpus.len(),
                root.display(),
                output.display()
            );
        }
        Cmd::Swarm {
            goal,
            index,
            top,
            format,
            ledger,
        } => {
            let corpus = Corpus::load(&index)?;
            let parsed = parse_goal(&goal);
            let swarm = Swarm::new();
            let ledger_handle = ledger
                .as_ref()
                .map(|p| Ledger::open(p))
                .transpose()?;
            let readings = swarm.route_with_ledger(
                &parsed,
                &corpus,
                top,
                ledger_handle.as_ref(),
            );
            match format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&readings)?);
                }
                _ => {
                    let synthesis = swarm.synthesise(&readings);
                    println!("══ HEAD-AGENT SYNTHESIS ══");
                    println!("{}\n", synthesis.summary);
                    if !synthesis.consensus_homes.is_empty() {
                        println!("Consensus homes (cited by multiple specialists):");
                        for (i, c) in synthesis.consensus_homes.iter().enumerate().take(5) {
                            println!(
                                "  {:>2}. {} ({} vote{}, weighted {:.3}) — {}:{}",
                                i + 1,
                                c.lemma_name,
                                c.votes,
                                if c.votes == 1 { "" } else { "s" },
                                c.weighted_score,
                                c.file,
                                c.line
                            );
                        }
                        println!();
                    }
                    println!(
                        "── per-specialist readings ({} specialists, top {} each):\n",
                        readings.len(),
                        top
                    );
                    for r in &readings {
                        println!(
                            "── {} [{}] · relevance {:.3}",
                            r.specialist, r.domain, r.relevance
                        );
                        println!("   reading: {}", r.reading);
                        if r.homes.is_empty() {
                            println!("   (relevance below swarm threshold; no homes returned)");
                        } else {
                            for (i, h) in r.homes.iter().enumerate() {
                                println!(
                                    "   {:>2}. [{:.3}] {} — {}:{}",
                                    i + 1,
                                    h.score,
                                    h.lemma.name,
                                    h.lemma.file.display(),
                                    h.lemma.line
                                );
                            }
                        }
                        println!();
                    }
                }
            }
        }
        Cmd::Find {
            goal,
            index,
            top,
            format,
        } => {
            let corpus = Corpus::load(&index)?;
            let parsed = parse_goal(&goal);
            let homes = rank(&parsed, &corpus, top);
            match format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&homes)?);
                }
                _ => {
                    if homes.is_empty() {
                        println!("(no candidate homes found in this corpus)");
                    } else {
                        println!(
                            "Found {} candidate home(s) for goal in {} (top {}):\n",
                            homes.len(),
                            index.display(),
                            top
                        );
                        for (i, h) in homes.iter().enumerate() {
                            println!(
                                "{:>3}. [{:.3}] {} ({:?})\n     {}:{}\n     shared: {:?}\n",
                                i + 1,
                                h.score,
                                h.lemma.name,
                                h.lemma.kind,
                                h.lemma.file.display(),
                                h.lemma.line,
                                h.shared_tokens
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
