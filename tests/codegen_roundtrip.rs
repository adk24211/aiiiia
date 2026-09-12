//! The emitted code is compiled and run, and its results compared bit for bit
//! against the reference interpreter.
//!
//! A code generator that merely *looks* right is worth very little: the ways
//! it goes wrong are an integer literal where a double was meant, a missing
//! parenthesis that changes precedence, a NaN rule the target spells
//! differently. All three survive inspection and none survives being run.
//!
//! Each toolchain is optional. When one is missing the test says so and
//! passes, rather than failing on a machine that simply does not have it.

use saturn::codegen::{emit, Lang};
use saturn::eval::{eval, Env};
use saturn::gen::{ExprStream, Grammar};
use saturn::lang::RecExpr;
use saturn::rng::Rng;
use saturn::sym::Sym;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const VARS: [&str; 3] = ["x", "y", "z"];
const EXPRS: usize = 120;
const ROWS: usize = 24;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("saturn-codegen-tests");
    std::fs::create_dir_all(&dir).expect("a writable temp directory");
    dir.join(name)
}

/// The expressions and the inputs every language is checked against.
fn corpus() -> (Vec<RecExpr>, Vec<[f64; 3]>) {
    let grammar = Grammar::default().with_logic().with_vars(&VARS);
    let exprs: Vec<RecExpr> = ExprStream::new(0xC0DE, grammar, 4).take(EXPRS).collect();
    let mut rng = Rng::seed(0x520F);
    let rows: Vec<[f64; 3]> = (0..ROWS)
        .map(|_| [rng.float(), rng.float(), rng.float()])
        .collect();
    (exprs, rows)
}

/// What the interpreter says, as raw bits, in the order the generated programs
/// print them.
fn expected(exprs: &[RecExpr], rows: &[[f64; 3]]) -> Vec<u64> {
    let mut out = Vec::with_capacity(exprs.len() * rows.len());
    for row in rows {
        for expr in exprs {
            let env: Env = VARS
                .iter()
                .zip(row)
                .map(|(name, v)| (Sym::new(name), *v))
                .collect();
            out.push(eval(expr, &env).expect("every variable is bound").to_bits());
        }
    }
    out
}

/// The emitted unit minus its preamble: everything from the function's own
/// signature onwards.
fn function_only<'a>(emitted: &'a str, signature: &str) -> &'a str {
    let at = emitted
        .find(signature)
        .unwrap_or_else(|| panic!("emitted code has no `{}`:\n{}", signature, emitted));
    &emitted[at..]
}

/// Every helper any expression in the corpus might need, once.
fn preludes(lang: Lang) -> String {
    // Emit a probe expression that uses all of them, then keep the preamble.
    let probe = saturn::parser::parse(
        "min(max(x, sign(y)), x ^ y) + x / y + sqrt(x) + ln(x) + exp(x) + sin(x) + cos(x) + tan(x) \
         + floor(x) + ceil(x) + abs(x) + atan2(x, y)",
    )
    .expect("valid probe");
    let text = emit(&probe, lang, "probe_unused");
    let end = text.find("probe_unused").expect("the probe function");
    let start = text[..end].rfind(match lang {
        Lang::C => "double ",
        Lang::Rust => "pub fn ",
        Lang::Python => "def ",
    });
    text[..start.expect("a function signature")].to_string()
}

fn lang_of(name: &str) -> Lang {
    match name {
        "C" => Lang::C,
        "Rust" => Lang::Rust,
        _ => Lang::Python,
    }
}

fn compare(language: &str, expected: &[u64], actual: &[u64], exprs: &[RecExpr], rows: &[[f64; 3]]) {
    assert_eq!(
        expected.len(),
        actual.len(),
        "{} produced {} results, expected {}",
        language,
        actual.len(),
        expected.len()
    );
    for (i, (want, got)) in expected.iter().zip(actual).enumerate() {
        let (w, g) = (f64::from_bits(*want), f64::from_bits(*got));
        if want == got || (w.is_nan() && g.is_nan()) {
            continue;
        }
        let expr = &exprs[i % exprs.len()];
        panic!(
            "{lang} disagrees with the interpreter\n  expression:  {expr}\n  inputs:      {inputs:?}\n  interpreter: {w:?}\n  {lang}: {g:?}\n  emitted:\n{code}",
            lang = language,
            expr = expr.pretty(),
            inputs = rows[i / exprs.len()],
            w = w,
            g = g,
            code = emit(expr, lang_of(language), "f"),
        );
    }
}

fn parse_bits(out: &str, language: &str) -> Vec<u64> {
    out.split_whitespace()
        .map(|t| {
            u64::from_str_radix(t, 16)
                .unwrap_or_else(|_| panic!("{} printed {:?}, expected hex bits", language, t))
        })
        .collect()
}

fn run(cmd: &mut Command, what: &str) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{} failed to start: {}", what, e));
    assert!(
        out.status.success(),
        "{} failed:\n{}\n{}",
        what,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("output should be utf-8")
}

/// How each generated program names its inputs, given the bit pattern.
fn call_args(expr: &RecExpr, row_index: usize, lang: Lang) -> String {
    expr.vars()
        .iter()
        .map(|v| match lang {
            Lang::Python => format!(
                "row{}[{}]",
                row_index,
                VARS.iter().position(|n| *n == v.as_str()).unwrap()
            ),
            _ => format!("r{}_{}", row_index, v),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn emitted_c_matches_the_interpreter() {
    if !have("cc") && !have("gcc") {
        eprintln!("skipping: no C compiler");
        return;
    }
    let compiler = if have("cc") { "cc" } else { "gcc" };
    let (exprs, rows) = corpus();

    let mut src = String::from("#include <math.h>\n#include <stdio.h>\n#include <string.h>\n\n");
    src.push_str(
        "static double from_bits(unsigned long long b) { double d; memcpy(&d, &b, 8); return d; }\n\
         static unsigned long long to_bits(double d) { unsigned long long b; memcpy(&b, &d, 8); return b; }\n\n",
    );
    src.push_str(preludes(Lang::C).trim_start_matches("#include <math.h>\n\n"));
    for (i, e) in exprs.iter().enumerate() {
        // Each emitted unit is standalone, so it repeats the include and any
        // helpers it needs. Keep only the function itself and give the file
        // one copy of each helper up front.
        let f = emit(e, Lang::C, &format!("f{}", i));
        src.push_str(function_only(&f, &format!("double f{}(", i)));
        src.push('\n');
    }
    src.push_str("int main(void) {\n");
    for (r, row) in rows.iter().enumerate() {
        for (v, value) in VARS.iter().zip(row) {
            let _ = writeln!(
                src,
                "    const double r{}_{} = from_bits(0x{:016x}ULL);",
                r,
                v,
                value.to_bits()
            );
        }
        for (i, e) in exprs.iter().enumerate() {
            let _ = writeln!(
                src,
                "    printf(\"%016llx\\n\", to_bits(f{}({})));",
                i,
                call_args(e, r, Lang::C)
            );
        }
    }
    src.push_str("    return 0;\n}\n");

    let c_path = scratch("roundtrip.c");
    let bin = scratch("roundtrip_c");
    std::fs::write(&c_path, &src).expect("writing the generated C");
    run(
        Command::new(compiler)
            .arg("-O1")
            .arg("-o")
            .arg(&bin)
            .arg(&c_path)
            .arg("-lm"),
        "compiling the generated C",
    );
    let out = run(&mut Command::new(&bin), "running the generated C");
    compare(
        "C",
        &expected(&exprs, &rows),
        &parse_bits(&out, "C"),
        &exprs,
        &rows,
    );
}

#[test]
fn emitted_rust_matches_the_interpreter() {
    if !have("rustc") {
        eprintln!("skipping: no rustc");
        return;
    }
    let (exprs, rows) = corpus();

    let mut src = String::from("#![allow(unused_parens, dead_code, clippy::all)]\n");
    src.push_str(&preludes(Lang::Rust));
    for (i, e) in exprs.iter().enumerate() {
        let f = emit(e, Lang::Rust, &format!("f{}", i));
        src.push_str(function_only(&f, &format!("pub fn f{}(", i)));
        src.push('\n');
    }
    src.push_str("fn main() {\n");
    for (r, row) in rows.iter().enumerate() {
        for (v, value) in VARS.iter().zip(row) {
            let _ = writeln!(
                src,
                "    let r{}_{} = f64::from_bits(0x{:016x}u64);",
                r,
                v,
                value.to_bits()
            );
        }
        for (i, e) in exprs.iter().enumerate() {
            let _ = writeln!(
                src,
                "    println!(\"{{:016x}}\", f{}({}).to_bits());",
                i,
                call_args(e, r, Lang::Rust)
            );
        }
    }
    src.push_str("}\n");

    let rs_path = scratch("roundtrip.rs");
    let bin = scratch("roundtrip_rs");
    std::fs::write(&rs_path, &src).expect("writing the generated Rust");
    run(
        Command::new("rustc")
            .arg("-O")
            .arg("-A")
            .arg("warnings")
            .arg("-o")
            .arg(&bin)
            .arg(&rs_path),
        "compiling the generated Rust",
    );
    let out = run(&mut Command::new(&bin), "running the generated Rust");
    compare(
        "Rust",
        &expected(&exprs, &rows),
        &parse_bits(&out, "Rust"),
        &exprs,
        &rows,
    );
}

#[test]
fn emitted_python_matches_the_interpreter() {
    if !have("python3") {
        eprintln!("skipping: no python3");
        return;
    }
    let (exprs, rows) = corpus();

    let mut src = String::from("import math\nimport struct\n\n\n");
    src.push_str(preludes(Lang::Python).trim_start_matches("import math\n\n\n"));
    for (i, e) in exprs.iter().enumerate() {
        let f = emit(e, Lang::Python, &format!("f{}", i));
        src.push_str(function_only(&f, &format!("def f{}(", i)));
        src.push('\n');
    }
    src.push_str(
        "\ndef bits(v):\n    return '%016x' % struct.unpack('<Q', struct.pack('<d', v))[0]\n\n\n",
    );
    for (r, row) in rows.iter().enumerate() {
        let values: Vec<String> = row
            .iter()
            .map(|v| {
                format!(
                    "struct.unpack('<d', struct.pack('<Q', 0x{:016x}))[0]",
                    v.to_bits()
                )
            })
            .collect();
        let _ = writeln!(src, "row{} = [{}]", r, values.join(", "));
    }
    for (r, _) in rows.iter().enumerate() {
        for (i, e) in exprs.iter().enumerate() {
            let _ = writeln!(
                src,
                "print(bits(f{}({})))",
                i,
                call_args(e, r, Lang::Python)
            );
        }
    }

    let py_path = scratch("roundtrip.py");
    std::fs::write(&py_path, &src).expect("writing the generated Python");
    let out = run(
        Command::new("python3").arg(&py_path),
        "running the generated Python",
    );
    compare(
        "Python",
        &expected(&exprs, &rows),
        &parse_bits(&out, "Python"),
        &exprs,
        &rows,
    );
}

#[test]
fn the_scratch_directory_is_cleaned_up() {
    // Not a behaviour test: it keeps the other three from leaving artefacts
    // behind on a machine that runs the suite often.
    let dir: &Path = &std::env::temp_dir().join("saturn-codegen-tests");
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
}
