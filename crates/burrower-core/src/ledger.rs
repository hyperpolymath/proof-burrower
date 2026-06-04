// SPDX-License-Identifier: MPL-2.0
// Copyright (c) Jonathan D.A. Jewell <j.d.a.jewell@open.ac.uk>
//! # The Burrow Ledger
//!
//! Append-only log of every approach a specialist tried, with the result
//! and what was learned. The substrate that turns the swarm from an
//! ensemble into a learning organisation.
//!
//! Without the ledger, N parallel agents are N memoryless guessers.
//! With the ledger, every dead end shrinks the search space for the
//! next agent, and every win becomes a citable precedent.
//!
//! ## Storage
//!
//! v0.1: JSON-lines (one record per line) on local disk. Atomic
//! single-record append via `OpenOptions::append`. No locking yet —
//! single-writer assumed.
//!
//! v0.2 (planned): VeriSimDB per estate policy (`feedback_verisimdb_policy.md`).
//!
//! ## Record schema
//!
//! See [`LedgerRecord`]. Every field is optional except `id`,
//! `timestamp`, `goal_hash`, and `specialist`. Records are forward-
//! compatible: future fields land in `extra`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One ledger record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerRecord {
    /// Unique time-orderable id (millis since epoch + counter).
    pub id: String,
    /// ISO-8601-ish timestamp string, UTC.
    pub timestamp: String,
    /// SHA-style hash of the goal (8 hex chars is enough at our scale).
    pub goal_hash: String,
    /// First 200 chars of the goal text, for human reading.
    pub goal_excerpt: String,
    /// Which specialist authored this record.
    pub specialist: String,

    /// Optional structured fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RecordResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning: Option<Learning>,

    /// Forward-compatible bag for fields not yet promoted to first-class.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approach {
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics_attempted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions_assumed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordResult {
    /// `succeeded` | `failed` | `partial` | `abandoned` | `proposed`
    pub status: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub pattern_extracted: String,
    /// `positive` | `anti-pattern` | `specialisation` | `boundary`
    pub pattern_kind: String,
    pub generalisation: String,
    /// Specialist names that should see this learning. Empty = visible to all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_to: Vec<String>,
}

/// The ledger handle. One writer; many readers.
#[derive(Debug, Clone)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    /// Open or create a ledger at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Touch the file so existence is consistent.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening ledger at {}", path.display()))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. Atomic per-record (single fs write call).
    pub fn append(&self, record: &LedgerRecord) -> Result<()> {
        let line = serde_json::to_string(record)?;
        let mut f = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening ledger for append at {}", self.path.display()))?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Read all records.
    pub fn read_all(&self) -> Result<Vec<LedgerRecord>> {
        let f = File::open(&self.path)
            .with_context(|| format!("opening ledger for read at {}", self.path.display()))?;
        let reader = BufReader::new(f);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LedgerRecord>(&line) {
                Ok(r) => out.push(r),
                Err(e) => eprintln!("warning: skipping malformed ledger line: {e}"),
            }
        }
        Ok(out)
    }

    /// Filter records matching a predicate.
    pub fn query(&self, mut predicate: impl FnMut(&LedgerRecord) -> bool) -> Result<Vec<LedgerRecord>> {
        Ok(self.read_all()?.into_iter().filter(|r| predicate(r)).collect())
    }

    /// Most-recent N records.
    pub fn recent(&self, limit: usize) -> Result<Vec<LedgerRecord>> {
        let mut all = self.read_all()?;
        all.reverse();
        all.truncate(limit);
        Ok(all)
    }

    /// Records authored by a specific specialist.
    pub fn by_specialist(&self, name: &str) -> Result<Vec<LedgerRecord>> {
        self.query(|r| r.specialist == name)
    }

    /// Records about a specific goal (by hash).
    pub fn by_goal_hash(&self, hash: &str) -> Result<Vec<LedgerRecord>> {
        self.query(|r| r.goal_hash == hash)
    }

    /// Anti-patterns visible to (or not restricted away from) `specialist`.
    pub fn anti_patterns_for(&self, specialist: &str) -> Result<Vec<Learning>> {
        let records = self.read_all()?;
        Ok(records
            .into_iter()
            .filter_map(|r| r.learning)
            .filter(|l| l.pattern_kind == "anti-pattern")
            .filter(|l| l.visible_to.is_empty() || l.visible_to.iter().any(|s| s == specialist))
            .collect())
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Make a goal-hash from text. 8 hex chars from a fast non-cryptographic
/// hash is enough for a per-repo ledger; the goal text itself is also
/// kept (excerpted) in each record for verification.
pub fn goal_hash(goal_text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    goal_text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// New record id: epoch millis + sequence counter (process-local).
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    format!("{millis:013}-{n:04}")
}

pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Bare seconds-since-epoch isn't ISO but it's monotonic enough for
    // local debug. v0.2 will use chrono for proper ISO formatting.
    format!("epoch:{secs}")
}

/// Convenience constructor: a "Reading recorded" entry.
pub fn record_reading(
    specialist: &str,
    goal_text: &str,
    reading: &str,
    relevance: f64,
) -> LedgerRecord {
    LedgerRecord {
        id: new_id(),
        timestamp: now_iso(),
        goal_hash: goal_hash(goal_text),
        goal_excerpt: goal_text.chars().take(200).collect(),
        specialist: specialist.to_string(),
        approach: Some(Approach {
            description: format!("read goal in {} vocabulary", specialist),
            tactics_attempted: vec![],
            preconditions_assumed: vec![],
        }),
        result: Some(RecordResult {
            status: "proposed".to_string(),
            explanation: format!("relevance={:.3}, reading: {}", relevance, reading),
            artifacts: vec![],
        }),
        learning: None,
        extra: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_append_read_roundtrip() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p).unwrap();
        let r = LedgerRecord {
            id: new_id(),
            timestamp: now_iso(),
            goal_hash: goal_hash("lemma foo: x = y"),
            goal_excerpt: "lemma foo: x = y".to_string(),
            specialist: "Algebraist".to_string(),
            approach: Some(Approach {
                description: "tried add_mono".to_string(),
                tactics_attempted: vec!["add_mono".to_string()],
                preconditions_assumed: vec!["ordered_ab_semigroup_add".to_string()],
            }),
            result: Some(RecordResult {
                status: "failed".to_string(),
                explanation: "tropical lacks the class".to_string(),
                artifacts: vec![],
            }),
            learning: Some(Learning {
                pattern_extracted: "tropical-not-ordered-class".to_string(),
                pattern_kind: "anti-pattern".to_string(),
                generalisation: "Tropical algebra needs explicit hierarchy class instance".to_string(),
                visible_to: vec![],
            }),
            extra: serde_json::Value::Null,
        };
        l.append(&r).unwrap();
        l.append(&r).unwrap();
        let all = l.read_all().unwrap();
        assert_eq!(all.len(), 2);
        let antis = l.anti_patterns_for("OrderTheorist").unwrap();
        assert_eq!(antis.len(), 2);
        assert_eq!(antis[0].pattern_extracted, "tropical-not-ordered-class");
    }

    #[test]
    fn unique_ids() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }

    #[test]
    fn goal_hash_is_deterministic() {
        let a = goal_hash("lemma foo");
        let b = goal_hash("lemma foo");
        let c = goal_hash("lemma bar");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
