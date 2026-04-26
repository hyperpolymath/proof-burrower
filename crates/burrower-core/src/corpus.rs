// SPDX-License-Identifier: PMPL-1.0-or-later

//! Corpus indexing.
//!
//! A `Corpus` is a collection of indexed lemmas extracted from one or
//! more library directories. Burrower walks the directory tree, parses
//! each source file enough to identify lemma boundaries and extract the
//! lemma name + statement, then stores them as [`IndexedLemma`]s.

use crate::goal::{parse_goal, Goal};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Recognised source languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LibraryKind {
    Isabelle,
    Coq,
    Lean,
}

impl LibraryKind {
    fn from_path(p: &Path) -> Option<Self> {
        let ext = p.extension()?.to_str()?;
        match ext {
            "thy" => Some(LibraryKind::Isabelle),
            "v" => Some(LibraryKind::Coq),
            "lean" => Some(LibraryKind::Lean),
            _ => None,
        }
    }
}

/// A single lemma extracted from a corpus file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedLemma {
    pub name: String,
    pub statement: String,
    pub file: PathBuf,
    pub line: usize,
    pub kind: LibraryKind,
    pub tokens: BTreeSet<String>,
}

/// An indexed corpus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Corpus {
    pub lemmas: Vec<IndexedLemma>,
}

impl Corpus {
    /// Walk `root` recursively, extracting all lemmas from recognised files.
    pub fn index(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let mut lemmas = Vec::new();
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(kind) = LibraryKind::from_path(path) else {
                continue;
            };
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue, // skip unreadable files
            };
            let extracted = extract_lemmas(&content, path, kind);
            lemmas.extend(extracted);
        }
        Ok(Corpus { lemmas })
    }

    /// Number of indexed lemmas.
    pub fn len(&self) -> usize {
        self.lemmas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lemmas.is_empty()
    }

    /// Persist the index to a JSON file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path.as_ref(), json).context("writing corpus index")?;
        Ok(())
    }

    /// Load a previously-persisted index.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path.as_ref()).context("reading corpus index")?;
        let corpus: Corpus = serde_json::from_str(&raw)?;
        Ok(corpus)
    }
}

/// Extract `(name, statement)` pairs from a source file.
///
/// Heuristic: scan for lines starting with a recognised lemma keyword,
/// take the identifier following it, and the statement up to the proof
/// marker (`proof`, `by`, `Proof.`, `:=`, etc.) or next blank line.
fn extract_lemmas(content: &str, path: &Path, kind: LibraryKind) -> Vec<IndexedLemma> {
    let mut out = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    let lemma_keywords: &[&str] = match kind {
        LibraryKind::Isabelle => &["lemma ", "theorem ", "corollary "],
        LibraryKind::Coq => &["Lemma ", "Theorem ", "Corollary ", "Definition "],
        LibraryKind::Lean => &["theorem ", "lemma ", "def "],
    };

    let proof_markers: &[&str] = match kind {
        LibraryKind::Isabelle => &["proof", "by ", "by(", "by\n", "  by "],
        LibraryKind::Coq => &["Proof.", "Proof "],
        LibraryKind::Lean => &[":=", "by\n"],
    };

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        if let Some(kw) = lemma_keywords.iter().find(|k| trimmed.starts_with(*k)) {
            let after_kw = &trimmed[kw.len()..];
            // Name = identifier up to `:` or whitespace.
            let name = after_kw
                .split(|c: char| c == ':' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .trim_end_matches(':')
                .to_string();
            if name.is_empty() {
                i += 1;
                continue;
            }

            // Statement = current line + following lines until a proof marker,
            // a blank line, or another lemma start. Cap at 20 lines to avoid
            // runaway captures.
            let start_line = i + 1;
            let mut stmt = String::from(line);
            let mut j = i + 1;
            let cap = j + 20;
            while j < lines.len() && j < cap {
                let nl = lines[j];
                let nl_trim = nl.trim_start();
                if nl_trim.is_empty() {
                    break;
                }
                if proof_markers.iter().any(|m| nl_trim.starts_with(*m)) {
                    break;
                }
                if lemma_keywords.iter().any(|k| nl_trim.starts_with(*k)) {
                    break;
                }
                stmt.push('\n');
                stmt.push_str(nl);
                j += 1;
            }

            let goal: Goal = parse_goal(&stmt);
            out.push(IndexedLemma {
                name,
                statement: stmt,
                file: path.to_path_buf(),
                line: start_line,
                kind,
                tokens: goal.tokens,
            });

            i = j;
        } else {
            i += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn indexes_isabelle_lemmas() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("Test.thy");
        let mut f = fs::File::create(&p).unwrap();
        writeln!(
            f,
            "theory Test imports Main begin\n\
             lemma foo: \"x + 0 = x\" by simp\n\
             lemma bar: \"y + y = 2 * y\"\n  by simp\n\
             end"
        )
        .unwrap();

        let corpus = Corpus::index(dir.path()).unwrap();
        assert_eq!(corpus.len(), 2);
        let names: Vec<_> = corpus.lemmas.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn skips_unreadable_files() {
        let dir = tempdir().unwrap();
        let corpus = Corpus::index(dir.path()).unwrap();
        assert!(corpus.is_empty());
    }
}
