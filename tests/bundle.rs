//! Several expressions optimized together, and the code emitted for them.

use saturn::analysis::MathAnalysis;
use saturn::bundle::Bundle;
use saturn::check::Checker;
use saturn::codegen::{emit_bundle, Lang};
use saturn::extract::{AstSize, OpCost};
use saturn::rules;
use saturn::runner::Runner;
use std::process::Command;
use std::time::Duration;

fn optimize(src: &str, set: &str) -> Bundle {
    let bundle = Bundle::parse(src).unwrap_or_else(|e| panic!("{}", e));
    let runner = Runner::<MathAnalysis>::default()
        .with_iter_limit(10)
        .with_node_limit(10_000)
        .with_time_limit(Duration::from_secs(3));
    saturn::optimize_bundle(runner, &bundle, &rules::named(set).unwrap(), OpCost).0
}

const ROTATION: &str = "nx = x * cos(t) - y * sin(t)\nny = x * sin(t) + y * cos(t)";

#[test]
fn outputs_share_their_common_work() {
    let b = optimize(ROTATION, "safe");
    assert!(
        b.cost(&OpCost) < b.cost_apart(&OpCost),
        "sharing bought nothing: {} together, {} apart",
        b.cost(&OpCost),
        b.cost_apart(&OpCost)
    );
    // Two transcendental calls, not four.
    let trig = b
        .reachable()
        .iter()
        .filter(|&&id| matches!(b.expr.node(id).op, saturn::Op::Sin | saturn::Op::Cos))
        .count();
    assert_eq!(trig, 2, "{}", emit_bundle(&b, Lang::C, "f"));
}

#[test]
fn every_output_still_computes_what_it_did() {
    let source = "p = u / w + v / w\nq = (u + v) * (u + v)\nr = exp(u) * exp(v)";
    let before = Bundle::parse(source).unwrap();
    let after = optimize(source, "all");
    assert_eq!(before.len(), after.len());

    let checker = Checker::new()
        .with_samples(2_000)
        .with_seed(0xB0D1)
        .with_tolerance(1e-9)
        .with_wild(true)
        .with_finite_only(true);
    for ((name, lhs), (_, rhs)) in before.parts().iter().zip(after.parts()) {
        let report = checker.compare(lhs, &rhs);
        assert!(
            report.ok(),
            "output `{}` changed\n  {}\n  {}\n{}",
            name,
            lhs.pretty(),
            rhs.pretty(),
            report.render()
        );
    }
}

#[test]
fn optimizing_together_is_never_worse_than_the_input() {
    for source in [
        ROTATION,
        "a = x + 1\nb = x + 1",
        "p = sqrt(x)\nq = sqrt(x) * sqrt(x)",
        "only = a * b",
    ] {
        let before = Bundle::parse(source).unwrap();
        let after = optimize(source, "all");
        assert!(
            after.cost(&OpCost) <= before.cost(&OpCost) + 1e-9,
            "`{}` got worse: {} -> {}",
            source,
            before.cost(&OpCost),
            after.cost(&OpCost)
        );
    }
}

#[test]
fn identical_outputs_collapse_to_one_computation() {
    let b = optimize("a = x * y + 1\nb = 1 + y * x", "safe");
    assert_eq!(b.len(), 2);
    // Both outputs are the same term, so they name the same node.
    assert_eq!(b.outputs[0].1, b.outputs[1].1);
    assert_eq!(b.cost(&AstSize) * 2.0, b.cost_apart(&AstSize));
}

#[test]
fn a_single_output_emits_a_plain_function() {
    let b = Bundle::parse("a * b + 1").unwrap();
    let c = emit_bundle(&b, Lang::C, "f");
    assert!(c.contains("double f(double a, double b)"), "{}", c);
    assert!(c.contains("return"), "{}", c);
    let rust = emit_bundle(&b, Lang::Rust, "f");
    assert!(rust.contains("-> f64"), "{}", rust);
}

#[test]
fn several_outputs_emit_one_function_per_language() {
    let b = optimize(ROTATION, "safe");

    let c = emit_bundle(&b, Lang::C, "rotate");
    assert!(c.contains("void rotate("), "{}", c);
    assert!(c.contains("double *nx"), "{}", c);
    assert!(c.contains("*ny ="), "{}", c);
    assert_eq!(c.matches("cos(").count(), 1, "{}", c);

    let rust = emit_bundle(&b, Lang::Rust, "rotate");
    assert!(rust.contains("-> (f64, f64)"), "{}", rust);
    assert!(rust.contains("/// Returns (nx, ny)."), "{}", rust);

    let py = emit_bundle(&b, Lang::Python, "rotate");
    assert!(py.contains("return ("), "{}", py);
    assert!(py.contains("Returns (nx, ny)"), "{}", py);
}

#[test]
fn the_emitted_c_compiles_and_agrees() {
    let compiler = if have("cc") {
        "cc"
    } else if have("gcc") {
        "gcc"
    } else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let b = optimize(ROTATION, "safe");
    let dir = std::env::temp_dir().join("saturn-bundle-test");
    std::fs::create_dir_all(&dir).expect("a writable temp directory");
    let (c_path, bin) = (dir.join("rot.c"), dir.join("rot"));

    let mut src = emit_bundle(&b, Lang::C, "rotate");
    src.push_str(
        "\n#include <stdio.h>\nint main(void) {\n\
         for (int i = 0; i < 5; i++) {\n\
         double t = i * 0.7, x = i + 1.0, y = i - 2.0, nx, ny;\n\
         rotate(t, x, y, &nx, &ny);\n\
         printf(\"%.17g %.17g\\n\", nx, ny);\n\
         }\n return 0;\n}\n",
    );
    std::fs::write(&c_path, &src).expect("writing the generated C");
    let out = Command::new(compiler)
        .args(["-O1", "-o"])
        .arg(&bin)
        .arg(&c_path)
        .arg("-lm")
        .output()
        .expect("the compiler should run");
    assert!(
        out.status.success(),
        "the emitted C did not compile:\n{}\n{}",
        src,
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("the program should run");
    assert!(run.status.success());

    let printed: Vec<f64> = String::from_utf8_lossy(&run.stdout)
        .split_whitespace()
        .map(|t| t.parse().expect("a number"))
        .collect();
    assert_eq!(printed.len(), 10);
    for (i, pair) in printed.chunks(2).enumerate() {
        let (t, x, y) = (i as f64 * 0.7, i as f64 + 1.0, i as f64 - 2.0);
        let (want_nx, want_ny) = (x * t.cos() - y * t.sin(), x * t.sin() + y * t.cos());
        assert!(
            (pair[0] - want_nx).abs() < 1e-12,
            "nx: {} vs {}",
            pair[0],
            want_nx
        );
        assert!(
            (pair[1] - want_ny).abs() < 1e-12,
            "ny: {} vs {}",
            pair[1],
            want_ny
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
