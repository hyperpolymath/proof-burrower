// SPDX-License-Identifier: MPL-2.0
// CLI contract tests. Controlled subprocesses exercise transport and reporting,
// not theorem proving; live Isabelle validation lives in burrower-core/tests.
use burrower_core::{Corpus, Ledger};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const GOAL: &str =
    "lemma finite_tropical_order: \"finite walks ∧ tropical_add a b ≤ tropical_mul a b\"";

fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_burrower"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run real burrower CLI")
}

fn success(dir: &Path, args: &[&str]) -> String {
    let out = cli(dir, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn corpus(dir: &Path) {
    fs::create_dir(dir.join("corpus")).unwrap();
    fs::write(
        dir.join("corpus/Example.thy"),
        format!("{GOAL}\n  by simp\n"),
    )
    .unwrap();
    fs::write(
        dir.join("corpus/Example.v"),
        "Lemma coq_identity : forall x : nat, x = x.\nProof. auto. Qed.\n",
    )
    .unwrap();
    fs::write(
        dir.join("corpus/Example.lean"),
        "theorem lean_identity (x : Nat) : x = x := by rfl\n",
    )
    .unwrap();
    success(dir, &["index", "corpus", "--output", "index.json"]);
    assert_eq!(Corpus::load(dir.join("index.json")).unwrap().len(), 3);
}

#[test]
fn indexed_sources_are_searchable_in_text_json_and_swarm_views() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    corpus(dir);
    let ranked: Value = serde_json::from_str(&success(
        dir,
        &[
            "find",
            GOAL,
            "--index",
            "index.json",
            "--format",
            "json",
            "--top",
            "1",
        ],
    ))
    .unwrap();
    assert_eq!(ranked.as_array().unwrap().len(), 1);
    assert_eq!(ranked[0]["lemma"]["name"], "finite_tropical_order");
    let text = success(dir, &["find", GOAL, "--index", "index.json"]);
    assert!(
        text.contains("finite_tropical_order") && text.contains("Example.thy:1"),
        "{text}"
    );
    assert!(
        success(dir, &["find", GOAL, "--index", "index.json", "--top", "0"])
            .contains("no candidate homes")
    );

    let readings: Value = serde_json::from_str(&success(
        dir,
        &[
            "swarm",
            GOAL,
            "--index",
            "index.json",
            "--format",
            "json",
            "--ledger",
            "readings.jsonl",
        ],
    ))
    .unwrap();
    assert_eq!(readings.as_array().unwrap().len(), 3);
    let ledger = Ledger::open(dir.join("readings.jsonl")).unwrap();
    let records = ledger.read_all().unwrap();
    assert_eq!(records.len(), 3);
    assert!(records
        .iter()
        .all(|r| r.goal_excerpt == GOAL && r.result.as_ref().unwrap().status == "proposed"));
    let text = success(dir, &["swarm", GOAL, "--index", "index.json"]);
    assert!(
        text.contains("HEAD-AGENT SYNTHESIS") && text.contains("finite_tropical_order"),
        "{text}"
    );
    assert!(
        success(dir, &["swarm", "unrelated_xyz", "--index", "index.json"])
            .contains("no homes returned")
    );

    fs::write(dir.join("broken.json"), "{not JSON}").unwrap();
    for index in ["broken.json", "missing.json"] {
        let out = cli(dir, &["find", GOAL, "--index", index]);
        assert!(!out.status.success());
        assert!(!out.stderr.is_empty());
    }
}

#[test]
fn ledger_commands_preserve_provenance_and_visibility() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let records = [
        json!({"id":"1","timestamp":"epoch:1","goal_hash":"goal-a","goal_excerpt":"first goal","specialist":"Algebraist","approach":{"description":"try simp"},"result":{"status":"failed","explanation":"Undefined fact"},"learning":{"pattern_extracted":"unknown-fact","pattern_kind":"anti-pattern","generalisation":"supply imports","visible_to":["Algebraist"]}}),
        json!({"id":"2","timestamp":"epoch:2","goal_hash":"goal-b","goal_excerpt":"second goal","specialist":"OrderTheorist"}),
    ];
    fs::write(
        dir.join("ledger.jsonl"),
        format!("{}\n{}\n", records[0], records[1]),
    )
    .unwrap();
    let recent = success(
        dir,
        &["ledger", "recent", "--path", "ledger.jsonl", "--limit", "1"],
    );
    assert!(recent.contains("second goal") && !recent.contains("first goal"));
    let all = success(dir, &["ledger", "recent", "--path", "ledger.jsonl"]);
    assert!(
        all.contains("try simp")
            && all.contains("Undefined fact")
            && all.contains("supply imports")
    );
    let by = success(
        dir,
        &[
            "ledger",
            "by-specialist",
            "--path",
            "ledger.jsonl",
            "Algebraist",
        ],
    );
    assert!(by.contains("first goal") && !by.contains("second goal"));
    assert!(success(
        dir,
        &[
            "ledger",
            "anti-patterns",
            "--path",
            "ledger.jsonl",
            "--for-specialist",
            "Algebraist"
        ]
    )
    .contains("unknown-fact"));
    assert!(!success(
        dir,
        &[
            "ledger",
            "anti-patterns",
            "--path",
            "ledger.jsonl",
            "--for-specialist",
            "OrderTheorist"
        ]
    )
    .contains("unknown-fact"));
    let digest = success(dir, &["ledger", "digest", "--path", "ledger.jsonl"]);
    assert!(
        digest.contains("2 record(s)")
            && digest.contains("failed")
            && digest.contains("unknown-fact")
    );
}

#[cfg(unix)]
fn executable(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn attempts_report_rejection_missing_provers_and_oracle_vetoes() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let prover = executable(
        dir,
        "prover-contract.sh",
        "printf 'Proof verification failed: error fixture rejection\\n' >&2\nexit 1",
    );
    let attempts = success(
        dir,
        &[
            "attempt",
            GOAL,
            "--echidna",
            prover.to_str().unwrap(),
            "--ledger",
            "attempts.jsonl",
        ],
    );
    assert!(
        attempts.contains("0 succeeded") && attempts.contains("No tactic"),
        "{attempts}"
    );
    let ledger = Ledger::open(dir.join("attempts.jsonl")).unwrap();
    assert!(!ledger.read_all().unwrap().is_empty());
    assert!(ledger
        .read_all()
        .unwrap()
        .iter()
        .all(|r| r.result.as_ref().unwrap().status == "failed"));

    let skipped: Value = serde_json::from_str(&success(
        dir,
        &[
            "attempt",
            GOAL,
            "--echidna",
            "./missing-prover",
            "--ledger",
            "skipped.jsonl",
            "--format",
            "json",
        ],
    ))
    .unwrap();
    assert!(!skipped.as_array().unwrap().is_empty());
    assert!(skipped
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["result"].get("Skipped").is_some()));
    assert!(success(
        dir,
        &[
            "attempt",
            GOAL,
            "--echidna",
            "./missing-prover",
            "--ledger",
            "skipped-text.jsonl"
        ]
    )
    .contains("skipped"));

    let oracle = executable(
        dir,
        "oracle-contract.sh",
        "printf 'oracle: disagree (fixture numerical counterexample)\\n'",
    );
    let sentinel = executable(dir, "must-not-run.sh", "touch prover-was-run\nexit 99");
    let blocked: Value = serde_json::from_str(&success(
        dir,
        &[
            "attempt",
            GOAL,
            "--echidna",
            sentinel.to_str().unwrap(),
            "--ledger",
            "oracle.jsonl",
            "--oracle",
            "oracle.jl",
            "--oracle-descriptor",
            "fixture.a2ml",
            "--oracle-julia",
            oracle.to_str().unwrap(),
            "--format",
            "json",
        ],
    ))
    .unwrap();
    assert_eq!(blocked["blocked_by_oracle"], true);
    assert!(!dir.join("prover-was-run").exists());
    let oracle_records = Ledger::open(dir.join("oracle.jsonl"))
        .unwrap()
        .read_all()
        .unwrap();
    assert_eq!(oracle_records.len(), 1);
    assert_eq!(oracle_records[0].result.as_ref().unwrap().status, "blocked");
    assert_eq!(oracle_records[0].goal_excerpt, GOAL);
}
