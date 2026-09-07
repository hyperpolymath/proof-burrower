// SPDX-License-Identifier: MPL-2.0
//! Real Burrower -> ECHIDNA -> Isabelle acceptance and rejection controls.
use burrower_core::attempt::{
    generate_probe, run_probe, AttemptResult, ProverConfig, TacticTemplate,
};

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
            assert!(matches!(result, AttemptResult::Failed { .. }), "{result:?}");
        }
    }
}
