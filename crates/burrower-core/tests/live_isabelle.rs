// SPDX-License-Identifier: MPL-2.0
//! Real Burrower -> ECHIDNA -> Isabelle acceptance and rejection controls.
use burrower_core::attempt::{
    generate_probe, run_probe, AttemptResult, ProverConfig, TacticTemplate,
};

// These fixtures all leave an unproved Isabelle goal. Generic process errors,
// missing executables, malformed theories and inconclusive output are not
// evidence that Isabelle rejected the mathematical obligation.
fn is_isabelle_rejection(result: &AttemptResult) -> bool {
    matches!(result, AttemptResult::Failed { error, .. }
        if error.contains("Failed to finish proof") && error.contains("goal ("))
}

#[test]
fn negative_control_requires_an_unproved_goal_diagnostic() {
    for result in [
        AttemptResult::Failed {
            error: "exit 1 — no standard diagnostic captured".into(),
            duration_ms: 0,
        },
        AttemptResult::Failed {
            error: "inconclusive output (no success/failure marker, exit 0)".into(),
            duration_ms: 0,
        },
        AttemptResult::Failed {
            error: "error: malformed theory".into(),
            duration_ms: 0,
        },
        AttemptResult::Skipped {
            reason: "missing executable".into(),
        },
        AttemptResult::Timeout,
        AttemptResult::Succeeded { duration_ms: 0 },
    ] {
        assert!(!is_isabelle_rejection(&result), "{result:?}");
    }
    assert!(is_isabelle_rejection(&AttemptResult::Failed {
        error: "*** Failed to finish proof | *** goal (1 subgoal): | *** 1. False".into(),
        duration_ms: 1,
    }));
}

#[test]
#[ignore = "requires ECHIDNA_BIN and Isabelle; required explicitly by Proof Safety CI"]
fn isabelle_preserves_and_checks_the_complete_goal() {
    let echidna_path = std::env::var_os("ECHIDNA_BIN")
        .expect("ECHIDNA_BIN must identify the built ECHIDNA executable")
        .into();
    let work = tempfile::tempdir().expect("probe directory");
    let config = ProverConfig {
        echidna_path,
        timeout_secs: 120,
        workdir: Some(work.path().to_path_buf()),
        ..ProverConfig::default()
    };
    let mut tactic = TacticTemplate {
        name: "simp".into(),
        script: "by simp".into(),
        description: "Real Isabelle regression control".into(),
    };
    for (goal, expected) in [
        (r#"lemma good: "(x::nat) + 0 = x""#, true),
        (r#"lemma bad: "(x::nat) + 1 = x""#, false),
        (
            r#"lemma dropped_conclusion: assumes "True" shows "False""#,
            false,
        ),
        (
            r#"lemma assumed: assumes "(x::nat) = 0" shows "x + 1 = 1""#,
            true,
        ),
        (r#"lemma multiple: shows "True" and "False""#, false),
        (
            r#"lemma commented: "True" (* proof (* by *) := *) by simp"#,
            true,
        ),
        (
            r#"lemma commented_bad: "True" (* by *) and "False" by simp"#,
            false,
        ),
    ] {
        tactic.script = if goal.contains("assumes") {
            "using assms by simp"
        } else {
            "by simp"
        }
        .into();
        let probe = generate_probe(goal, &tactic);
        let result = run_probe(&probe, &config, "Probe.thy");
        eprintln!("{goal}: {result:?}");
        if expected {
            assert!(
                matches!(result, AttemptResult::Succeeded { .. }),
                "{result:?}"
            );
        } else {
            assert!(
                is_isabelle_rejection(&result),
                "not an Isabelle proof rejection: {result:?}"
            );
        }
    }
}
