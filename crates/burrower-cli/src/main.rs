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
use burrower_core::{parse_goal, rank, Corpus};
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
    /// Locate the home of a proof goal.
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
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
