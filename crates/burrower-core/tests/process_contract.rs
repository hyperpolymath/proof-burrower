// SPDX-License-Identifier: MPL-2.0
//! Controlled children validate process isolation, argument forwarding and
//! failure reporting. They are not mathematical proof backends.
#![cfg(unix)]
use burrower_core::attempt::{run_playbook, run_probe, Playbook, ProverConfig, TacticTemplate};
use burrower_core::oracle::{self, OracleConfig, OracleVerdict};
use burrower_core::{parse_goal, AttemptResult, Ledger};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[test]
fn concurrent_attempts_keep_their_own_input_and_clean_up() {
    let dir = tempfile::tempdir().unwrap();
    // Both children wait until both have started. Reusing the same probe path
    // therefore makes at least one child read the other request's input.
    let first = executable(
        dir.path(),
        "first.sh",
        r#"
control=${0%/*}
touch "$control/first.ready"
count=0
while [ ! -f "$control/second.ready" ]; do
  count=$((count + 1)); [ "$count" -lt 100 ] || exit 2; sleep 0.01
done
[ "$(cat "$2")" = 'first obligation' ] || exit 1
printf 'Proof verified successfully\n'
"#,
    );
    let second = executable(
        dir.path(),
        "second.sh",
        r#"
control=${0%/*}
touch "$control/second.ready"
count=0
while [ ! -f "$control/first.ready" ]; do
  count=$((count + 1)); [ "$count" -lt 100 ] || exit 2; sleep 0.01
done
[ "$(cat "$2")" = 'second obligation' ] || exit 1
printf 'Proof verified successfully\n'
"#,
    );
    let workdir = dir.path().join("probes");
    let a = ProverConfig {
        echidna_path: first,
        workdir: Some(workdir.clone()),
        ..ProverConfig::default()
    };
    let b = ProverConfig {
        echidna_path: second,
        workdir: Some(workdir.clone()),
        ..ProverConfig::default()
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| run_probe("first obligation", &a, "Probe.thy"));
        let b = scope.spawn(|| run_probe("second obligation", &b, "Probe.thy"));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a.is_success(), "{a:?}");
    assert!(b.is_success(), "{b:?}");
    assert_eq!(fs::read_dir(workdir).unwrap().count(), 0);
}

#[test]
fn prover_contract_forwards_options_and_records_distinct_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let child = executable(
        dir.path(),
        "options.sh",
        r#"
[ "$1" = prove ]
[ "$3" = --prover ]
[ "$4" = Isabelle ]
[ "$5" = -t ]
[ "$6" = 7 ]
[ "$7" = --project-root ]
[ "$8" = 'project root' ]
[ "$9" = --sandbox ]
[ "${10}" = bwrap ]
case "$(cat "$2")" in
  *'by simp'*) printf 'Proof verified successfully\n' >&2 ;;
  *) printf 'Proof verification failed: error Failed to apply tactic\n' >&2; exit 1 ;;
esac
"#,
    );
    let cfg = ProverConfig {
        echidna_path: child,
        timeout_secs: 7,
        workdir: Some(dir.path().join("probes")),
        project_root: Some("project root".into()),
        sandbox: "bwrap".into(),
    };
    let ledger = Ledger::open(dir.path().join("ledger.jsonl")).unwrap();
    let playbook = Playbook {
        specialist: "Contract/fixture".into(),
        tactics: vec![
            TacticTemplate {
                name: "simp".into(),
                script: "by simp".into(),
                description: "acceptance fixture".into(),
            },
            TacticTemplate {
                name: "auto".into(),
                script: "by auto".into(),
                description: "rejection fixture".into(),
            },
        ],
    };
    let goal = parse_goal("lemma preserved: assumes \"True\" shows \"False\"");
    let attempts = run_playbook(&goal, &playbook, &cfg, Some(&ledger)).unwrap();
    assert!(attempts[0].result.is_success(), "{:?}", attempts[0]);
    assert!(!attempts[1].result.is_success(), "{:?}", attempts[1]);
    let records = ledger.read_all().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].id, records[1].id);
    assert!(records
        .iter()
        .all(|r| r.goal_excerpt == goal.raw && r.specialist == "Contract/fixture"));
    assert_eq!(
        records[0].learning.as_ref().unwrap().pattern_kind,
        "positive"
    );
    assert!(records[1]
        .learning
        .as_ref()
        .unwrap()
        .pattern_extracted
        .contains("tactic-mismatch"));
    assert_eq!(ledger.by_goal_hash(&records[0].goal_hash).unwrap().len(), 2);
}

#[test]
fn unavailable_storage_invalid_filenames_and_nonexecutable_children_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain-file");
    fs::write(&path, "not executable").unwrap();
    let mut cfg = ProverConfig {
        echidna_path: path.clone(),
        workdir: Some(dir.path().join("probes")),
        ..ProverConfig::default()
    };
    for name in [
        "../outside.thy",
        "/outside.thy",
        "",
        ".",
        "..",
        "child/probe.thy",
    ] {
        assert!(matches!(
            run_probe("fixture", &cfg, name),
            AttemptResult::Skipped { .. }
        ));
    }
    assert!(matches!(
        run_probe("fixture", &cfg, "Probe.thy"),
        AttemptResult::Skipped { .. }
    ));
    assert_eq!(
        fs::read_dir(cfg.workdir.as_ref().unwrap()).unwrap().count(),
        0
    );
    cfg.workdir = Some(path);
    assert!(matches!(
        run_probe("fixture", &cfg, "Probe.thy"),
        AttemptResult::Skipped { .. }
    ));
}

#[test]
fn oracle_transport_keeps_blocking_evidence_and_subprocess_errors_visible() {
    let dir = tempfile::tempdir().unwrap();
    let child = executable(
        dir.path(),
        "oracle.sh",
        r#"
[ "$1" = '--project=oracle project' ]
[ "$2" = oracle.jl ]
[ "$3" = descriptor.a2ml ]
printf 'startup notice\noracle: agree (first stanza)\noracle: disagree (counterexample)\noracle: fuzz-clean (last stanza)\n'
"#,
    );
    let cfg = OracleConfig {
        julia: child,
        script: "oracle.jl".into(),
        descriptor: "descriptor.a2ml".into(),
        project: Some("oracle project".into()),
    };
    let verdict = oracle::run(&cfg);
    assert!(matches!(verdict, OracleVerdict::Disagree { .. }));
    let ledger = Ledger::open(dir.path().join("oracle.jsonl")).unwrap();
    let goal = parse_goal("original oracle obligation");
    let verdicts = [
        verdict,
        oracle::parse_verdict("oracle: agree (computed=expected)"),
        oracle::parse_verdict("oracle: fuzz-counter (sample)"),
        oracle::parse_verdict("oracle: fuzz-clean (sample)"),
        oracle::parse_verdict("oracle: inapplicable (different family)"),
        oracle::parse_verdict("oracle: unsupported-shape"),
        oracle::run(&OracleConfig {
            julia: dir.path().join("missing"),
            ..cfg.clone()
        }),
        oracle::run(&OracleConfig {
            julia: executable(
                dir.path(),
                "failed.sh",
                "printf 'oracle: agree\\n'; printf 'failure diagnostic\\n' >&2; exit 1",
            ),
            ..cfg.clone()
        }),
    ];
    assert!(
        matches!(&verdicts[7], OracleVerdict::SubprocessError { reason } if reason.contains("failure diagnostic"))
    );
    for verdict in &verdicts {
        oracle::record_to_ledger(&ledger, &goal, &cfg, verdict);
    }
    let records = ledger.read_all().unwrap();
    assert_eq!(records.len(), verdicts.len());
    for (record, verdict) in records.iter().zip(&verdicts) {
        assert_eq!(record.goal_excerpt, goal.raw);
        assert_eq!(
            record.learning.as_ref().unwrap().pattern_kind,
            verdict.pattern_kind()
        );
        assert_eq!(
            record.result.as_ref().unwrap().status,
            if verdict.blocks_attempt() {
                "blocked"
            } else {
                "advisory"
            }
        );
    }
}
