//! The command line, exercised end to end.
//!
//! These run the real binary. The library tests cover what the engine does;
//! these cover the part a person actually touches — that every subcommand
//! runs, that failures exit non-zero and say something useful, and that
//! nothing prints escape codes into a pipe.

use std::process::{Command, Output};

fn saturn(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_saturn"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("the binary should run")
}

fn stdout(args: &[&str]) -> String {
    let out = saturn(args);
    assert!(
        out.status.success(),
        "`saturn {}` failed:\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("output should be utf-8")
}

fn fails(args: &[&str]) -> String {
    let out = saturn(args);
    assert!(
        !out.status.success(),
        "`saturn {}` should have failed but printed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn help_and_version_work() {
    assert!(stdout(&["--help"]).contains("USAGE"));
    assert!(stdout(&["help"]).contains("COMMANDS"));
    assert!(stdout(&["--version"]).contains(env!("CARGO_PKG_VERSION")));
    // No arguments is a usage error, not a success.
    assert!(!saturn(&[]).status.success());
}

#[test]
fn opt_reports_the_saving() {
    let out = stdout(&["opt", "u / w + v / w", "--rules", "all"]);
    assert!(out.contains("(u + v) / w"), "{}", out);
    assert!(out.contains("cheaper"), "{}", out);
    assert!(out.contains("saturated"), "{}", out);
}

#[test]
fn opt_reports_when_nothing_improves() {
    let out = stdout(&["opt", "x + y", "--rules", "all"]);
    assert!(out.contains("unchanged"), "{}", out);
}

#[test]
fn stats_list_the_rules_that_fired() {
    let out = stdout(&[
        "opt",
        "a*x^3 + b*x^2 + c*x + d",
        "--rules",
        "all",
        "--stats",
    ]);
    assert!(out.contains("rules that fired"), "{}", out);
    assert!(out.contains("iteration"), "{}", out);
}

#[test]
fn eval_binds_variables() {
    assert_eq!(stdout(&["eval", "2 * x + 1", "-D", "x=4"]).trim(), "9");
    assert_eq!(stdout(&["eval", "sqrt(x)", "-Dx=16"]).trim(), "4");
    assert_eq!(stdout(&["eval", "1 / 0"]).trim(), "inf");
    assert_eq!(stdout(&["eval", "0 / 0"]).trim(), "NaN");
}

#[test]
fn eval_names_an_unbound_variable() {
    let err = fails(&["eval", "x + y", "-D", "x=1"]);
    assert!(err.contains('y'), "{}", err);
    assert!(err.contains("-D"), "{}", err);
}

#[test]
fn diff_differentiates_and_simplifies() {
    let out = stdout(&["diff", "x", "x * x"]);
    assert!(out.contains("df/dx"), "{}", out);
    // Either form is correct; what matters is that no `d(` survives.
    assert!(!out.contains("d(x,"), "a derivative survived:\n{}", out);
    let out = stdout(&["diff", "x", "exp(sin(x * x))"]);
    assert!(out.contains("cos(x * x)"), "{}", out);
    assert!(!out.contains("d(x,"), "{}", out);
}

#[test]
fn diff_rejects_a_non_variable() {
    assert!(fails(&["diff", "1", "x"]).contains("variable"));
    assert!(fails(&["diff", "x"]).contains("usage"));
}

#[test]
fn check_compares_the_two_forms() {
    let out = stdout(&["check", "x * 1 + 0 * 1", "--samples", "200"]);
    assert!(out.contains("agree"), "{}", out);
}

#[test]
fn fuzz_reports_a_clean_run() {
    let out = stdout(&[
        "fuzz",
        "--rules",
        "safe",
        "--count",
        "60",
        "--samples",
        "40",
    ]);
    assert!(out.contains("clean"), "{}", out);
}

#[test]
fn vm_disassembles() {
    let out = stdout(&["vm", "a * b + 1", "--raw"]);
    assert!(out.contains("params"), "{}", out);
    assert!(out.contains("result in s"), "{}", out);
    // With bindings it also runs the program.
    let out = stdout(&["vm", "a * b + 1", "--raw", "-Da=3", "-Db=4"]);
    assert!(out.contains("13"), "{}", out);
}

#[test]
fn ast_shows_the_dag() {
    let out = stdout(&["ast", "let t = a + b in t * t"]);
    assert!(out.contains("shared nodes"), "{}", out);
    assert_eq!(
        stdout(&["ast", "2 * x + 1", "--sexp"]).trim(),
        "(+ (* 2 x) 1)"
    );
}

#[test]
fn egraph_dumps_and_emits_dot() {
    let out = stdout(&["egraph", "a * b", "--rules", "none"]);
    assert!(out.contains("classes"), "{}", out);
    let dot = stdout(&["egraph", "a * b", "--rules", "none", "--dot"]);
    assert!(dot.starts_with("digraph"), "{}", dot);
    assert!(dot.contains("cluster_"), "{}", dot);
}

#[test]
fn rules_lists_sets_and_members() {
    let out = stdout(&["rules"]);
    assert!(out.contains("fast-math"), "{}", out);
    let out = stdout(&["rules", "arith"]);
    assert!(out.contains("=>"), "{}", out);
    assert!(out.contains("total"), "{}", out);
    assert!(fails(&["rules", "nosuchset"]).contains("unknown"));
}

#[test]
fn bench_runs_the_suite() {
    let out = stdout(&["bench", "--quick"]);
    assert!(out.contains("horner"), "{}", out);
    assert!(out.contains("overall"), "{}", out);
}

#[test]
fn time_measures_all_three() {
    let out = stdout(&["time", "a * b + c", "--calls", "2000"]);
    assert!(out.contains("interpreted"), "{}", out);
    assert!(out.contains("compiled + optimized"), "{}", out);
    assert!(out.contains("ns/eval"), "{}", out);
}

#[test]
fn syntax_errors_point_at_the_problem() {
    let err = fails(&["opt", "1 + "]);
    assert!(err.contains("parse error"), "{}", err);
    assert!(err.contains('^'), "{}", err);
}

#[test]
fn unknown_commands_and_flags_are_rejected() {
    assert!(fails(&["nosuchcommand"]).contains("unknown command"));
    assert!(fails(&["opt", "x", "--rules", "nope"]).contains("unknown rule set"));
    assert!(fails(&["opt", "x", "--cost", "nope"]).contains("cost model"));
    assert!(fails(&["opt", "x", "--iters", "many"]).contains("not a number"));
}

#[test]
fn no_color_suppresses_escape_codes() {
    for args in [
        vec!["opt", "x * y + x * z", "--rules", "all"],
        vec!["rules"],
        vec!["bench", "--quick"],
    ] {
        let out = stdout(&args);
        assert!(
            !out.contains('\u{1b}'),
            "`saturn {}` emitted escape codes with NO_COLOR set",
            args.join(" ")
        );
    }
    // And `--color always` puts them back, so the detection is a choice
    // rather than a missing feature.
    let out = stdout(&[
        "opt",
        "x * y + x * z",
        "--rules",
        "all",
        "--color",
        "always",
    ]);
    assert!(out.contains('\u{1b}'), "--color always produced no colour");
}

#[test]
fn cost_models_can_disagree() {
    let smallest = stdout(&["opt", "(x ^ 2) ^ 3", "--rules", "all", "--cost", "size"]);
    let fastest = stdout(&["opt", "(x ^ 2) ^ 3", "--rules", "all", "--cost", "ops"]);
    assert!(smallest.contains("size"), "{}", smallest);
    assert!(fastest.contains("ops"), "{}", fastest);
}

#[test]
fn shared_printing_is_available() {
    let out = stdout(&["opt", "let t = a + b + c in t * t + t", "--shared"]);
    assert!(out.contains("let "), "{}", out);
}

#[test]
fn a_misspelled_flag_suggests_the_right_one() {
    // Without this, `--calls 2000` put `2000` into the expression being
    // optimized, which is not a failure a user can diagnose.
    assert!(fails(&["opt", "x", "--stat"]).contains("--stats"));
    assert!(fails(&["opt", "x", "--rule", "safe"]).contains("--rules"));
    assert!(fails(&["opt", "x", "--xyzzy"]).contains("saturn --help"));
    assert!(fails(&["opt", "x", "--rules"]).contains("needs a value"));
}

#[test]
fn why_proves_an_equality() {
    let out = stdout(&["why", "x * y + x * z", "x * (y + z)", "--rules", "all"]);
    assert!(out.contains("yes"), "{}", out);
    assert!(out.contains("distribute"), "{}", out);
    assert!(out.contains("?a = x"), "{}", out);
}

#[test]
fn why_unfolds_a_congruence_into_its_arguments() {
    let out = stdout(&[
        "why",
        "(a * b) * c + 1",
        "a * (b * c) + 1",
        "--rules",
        "all",
    ]);
    assert!(out.contains("congruence"), "{}", out);
    assert!(out.contains("argument"), "{}", out);
    assert!(out.contains("assoc-mul"), "{}", out);
}

#[test]
fn why_fails_when_it_cannot_prove_it() {
    let out = saturn(&["why", "x + 1", "x + 2", "--rules", "all"]);
    assert!(!out.status.success(), "an unprovable claim must not exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("do not prove these equal"), "{}", text);
    // Saturation finished, so this really is a negative answer rather than a
    // budget that ran out; the hint should say which.
    assert!(text.contains("no derivation exists"), "{}", text);
}

#[test]
fn why_needs_exactly_two_expressions() {
    assert!(fails(&["why", "x"]).contains("usage"));
    assert!(fails(&["why", "x", "y", "z"]).contains("usage"));
}

#[test]
fn opt_can_justify_its_own_result() {
    let out = stdout(&["opt", "u / w + v / w", "--rules", "all", "--why"]);
    assert!(out.contains("(u + v) / w"), "{}", out);
    assert!(
        out.contains("split-div") || out.contains("join-div"),
        "{}",
        out
    );
}

#[test]
fn assumptions_unlock_guarded_rules() {
    // Without a fact `w` could be zero, infinite or NaN, so nothing cancels.
    let plain = stdout(&["opt", "w / w * x"]);
    assert!(plain.contains("unchanged"), "{}", plain);

    let assumed = stdout(&["opt", "w / w * x", "--assume", "finite(w) && nonzero(w)"]);
    assert!(assumed.contains("cheaper"), "{}", assumed);
    assert!(assumed.contains("optimized x"), "{}", assumed);
}

#[test]
fn assumptions_accumulate_across_flags() {
    let out = stdout(&[
        "opt",
        "abs(x)",
        "--assume",
        "x >= 0",
        "--assume",
        "nonzero(x)",
    ]);
    assert!(out.contains("optimized x"), "{}", out);
}

#[test]
fn a_malformed_assumption_is_rejected() {
    let err = fails(&["opt", "x", "--assume", "x >"]);
    assert!(err.contains("parse error"), "{}", err);
    assert!(err.contains('^'), "{}", err);
    assert!(fails(&["opt", "x", "--assume", "x != 5"]).contains("against 0"));
}
