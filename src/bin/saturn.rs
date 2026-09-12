//! The `saturn` command line.

use saturn::analysis::MathAnalysis;
use saturn::check::Checker;
use saturn::codegen::{emit, Lang};
use saturn::egraph::EGraph;
use saturn::eval::{eval, parse_bindings, Env};
use saturn::extract::{dag_cost, tree_cost, AstDepth, AstSize, Extractor, OpCost};
use saturn::gen::{ExprStream, Grammar};
use saturn::lang::{Op, RecExpr};
use saturn::parser::parse;
use saturn::rewrite::Rewrite;
use saturn::rng::Rng;
use saturn::rules;
use saturn::runner::{BackoffScheduler, Runner, StopReason};
use saturn::sym::Sym;
use saturn::vm::Program;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Terminal styling
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Style {
    on: bool,
}

impl Style {
    fn detect(flag: Option<&str>) -> Style {
        let on = match flag {
            Some("always") => true,
            Some("never") => false,
            _ => std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal(),
        };
        Style { on }
    }
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{}m{}\x1b[0m", code, s)
        } else {
            s.to_string()
        }
    }
    fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.wrap("33", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.wrap("36", s)
    }
    fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// Levenshtein distance, for suggesting the flag someone meant.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

struct Args {
    command: String,
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(argv: Vec<String>) -> Result<Args, String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = argv.into_iter().peekable();
        // Every flag the CLI accepts. An unrecognized one is an error rather
        // than a positional argument: `--calls 2000` silently appending
        // `2000` to the expression being optimized is the kind of bug a user
        // has no way to diagnose.
        const VALUE_FLAGS: &[&str] = &[
            "rules",
            "cost",
            "iters",
            "nodes",
            "time",
            "samples",
            "seed",
            "tol",
            "color",
            "D",
            "define",
            "scheduler",
            "count",
            "depth",
            "max-failures",
            "set",
            "calls",
            "lang",
            "name",
        ];
        const BOOL_FLAGS: &[&str] = &[
            "help", "version", "stats", "shared", "sexp", "dot", "raw", "wild", "tame", "opt",
            "quick", "arith", "logic",
        ];
        let known = |name: &str| VALUE_FLAGS.contains(&name) || BOOL_FLAGS.contains(&name);
        let unknown = |name: &str| {
            let mut all: Vec<&str> = VALUE_FLAGS.iter().chain(BOOL_FLAGS).copied().collect();
            all.sort();
            match all
                .iter()
                .map(|k| (edit_distance(name, k), *k))
                // A suggestion is only useful when it is close; offering the
                // whole flag list is the same as offering nothing.
                .filter(|(d, k)| *d * 3 <= k.len().max(name.len()))
                .min()
            {
                Some((_, k)) => format!("unknown flag `--{}`; did you mean `--{}`?", name, k),
                None => format!("unknown flag `--{}`; run `saturn --help`", name),
            }
        };

        while let Some(a) = it.next() {
            if let Some(rest) = a.strip_prefix("--") {
                match rest.split_once('=') {
                    Some((k, v)) => {
                        if !known(k) {
                            return Err(unknown(k));
                        }
                        flags.push((k.to_string(), Some(v.to_string())));
                    }
                    None => {
                        if !known(rest) {
                            return Err(unknown(rest));
                        }
                        if VALUE_FLAGS.contains(&rest) {
                            let v = it
                                .next()
                                .ok_or_else(|| format!("`--{}` needs a value", rest))?;
                            flags.push((rest.to_string(), Some(v)));
                        } else {
                            flags.push((rest.to_string(), None));
                        }
                    }
                }
            } else if a.starts_with('-')
                && a.len() > 1
                && !a[1..].starts_with(|c: char| c.is_ascii_digit())
            {
                let short = &a[1..2];
                let glued = &a[2..];
                let name = match short {
                    "D" => "D",
                    "h" => "help",
                    "V" => "version",
                    "s" => "stats",
                    "q" => "quick",
                    other => return Err(format!("unknown flag `-{}`", other)),
                };
                if name == "D" {
                    let v = if glued.is_empty() {
                        it.next().ok_or("`-D` needs a `name=value`")?
                    } else {
                        glued.to_string()
                    };
                    flags.push(("D".into(), Some(v)));
                } else {
                    flags.push((name.into(), None));
                }
            } else {
                positional.push(a);
            }
        }
        let command = if positional.is_empty() {
            String::new()
        } else {
            positional.remove(0)
        };
        Ok(Args {
            command,
            positional,
            flags,
        })
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == name)
    }
    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }
    fn all(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(k, _)| k == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }
    fn num<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        match self.get(name) {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| format!("`--{} {}` is not a number", name, v)),
        }
    }
    fn expr_arg(&self) -> Result<String, String> {
        if self.positional.is_empty() {
            return Err("expected an expression".into());
        }
        Ok(self.positional.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Shared pipeline
// ---------------------------------------------------------------------------

enum Cost {
    Size,
    Depth,
    Ops,
}

impl Cost {
    fn parse(s: Option<&str>) -> Result<Cost, String> {
        Ok(match s.unwrap_or("ops") {
            "size" | "ast" => Cost::Size,
            "depth" => Cost::Depth,
            "ops" | "op" | "latency" => Cost::Ops,
            other => {
                return Err(format!(
                    "unknown cost model `{}`; expected size, depth, or ops",
                    other
                ))
            }
        })
    }
    fn name(&self) -> &'static str {
        match self {
            Cost::Size => "size",
            Cost::Depth => "depth",
            Cost::Ops => "ops",
        }
    }
    fn extract(&self, eg: &EGraph<MathAnalysis>, root: saturn::Id) -> (f64, RecExpr) {
        match self {
            Cost::Size => Extractor::new(eg, AstSize).find_best(root),
            Cost::Depth => Extractor::new(eg, AstDepth).find_best(root),
            Cost::Ops => Extractor::new(eg, OpCost).find_best(root),
        }
    }
    fn dag(&self, e: &RecExpr) -> f64 {
        match self {
            Cost::Size => dag_cost(e, &AstSize),
            Cost::Depth => tree_cost(e, &AstDepth),
            Cost::Ops => dag_cost(e, &OpCost),
        }
    }
}

struct Options {
    rules: Vec<Rewrite<MathAnalysis>>,
    rules_name: String,
    cost: Cost,
    iters: usize,
    nodes: usize,
    time: Duration,
    backoff: bool,
}

impl Options {
    fn from(args: &Args) -> Result<Options, String> {
        let name = args.get("rules").unwrap_or("default").to_string();
        let rules = rules::named(&name).ok_or_else(|| {
            let names: Vec<&str> = rules::set_names().iter().map(|(n, _)| *n).collect();
            format!(
                "unknown rule set `{}`; try one of: {}",
                name,
                names.join(", ")
            )
        })?;
        Ok(Options {
            rules,
            rules_name: name,
            cost: Cost::parse(args.get("cost"))?,
            iters: args.num("iters")?.unwrap_or(20),
            nodes: args.num("nodes")?.unwrap_or(20_000),
            time: Duration::from_secs_f64(args.num("time")?.unwrap_or(3.0)),
            backoff: args.get("scheduler").unwrap_or("backoff") != "simple",
        })
    }

    fn run(&self, expr: &RecExpr) -> Runner<MathAnalysis> {
        let mut r = Runner::default()
            .with_expr(expr)
            .with_iter_limit(self.iters)
            .with_node_limit(self.nodes)
            .with_time_limit(self.time);
        if self.backoff {
            r = r.with_scheduler(BackoffScheduler::default());
        }
        r.run(&self.rules)
    }

    /// Saturate and extract, returning the optimized expression and the run.
    fn optimize(&self, expr: &RecExpr) -> (RecExpr, f64, Runner<MathAnalysis>) {
        let runner = self.run(expr);
        let (cost, best) = self.cost.extract(&runner.egraph, runner.root());
        (best, cost, runner)
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn pct(before: f64, after: f64) -> String {
    if before <= 0.0 {
        return String::new();
    }
    let d = (after - before) / before * 100.0;
    if d.abs() < 0.05 {
        "unchanged".to_string()
    } else if d < 0.0 {
        format!("{:.0}% cheaper", -d)
    } else {
        format!("{:.0}% dearer", d)
    }
}

fn show_expr(st: &Style, label: &str, e: &RecExpr, shared: bool) {
    let text = if shared {
        e.pretty_shared()
    } else {
        e.pretty()
    };
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    println!("  {} {}", st.dim(&format!("{:<9}", label)), st.bold(first));
    for l in lines {
        println!("  {:<9} {}", "", st.bold(l));
    }
}

fn cmd_opt(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let opts = Options::from(args)?;
    let shared = args.has("shared");

    let (best, _, runner) = opts.optimize(&expr);

    let before = opts.cost.dag(&expr);
    let after = opts.cost.dag(&best);

    show_expr(st, "input", &expr, shared);
    println!(
        "  {} {} nodes, {} {}",
        st.dim("         "),
        expr.dag_size(),
        before,
        opts.cost.name(),
    );
    println!();
    show_expr(st, "optimized", &best, shared);
    let delta = pct(before, after);
    let colored = if after < before {
        st.green(&delta)
    } else if after > before {
        st.yellow(&delta)
    } else {
        st.dim(&delta)
    };
    println!(
        "  {} {} nodes, {} {}  {}",
        st.dim("         "),
        best.dag_size(),
        after,
        opts.cost.name(),
        colored
    );

    println!();
    let reason = runner
        .stop_reason
        .as_ref()
        .map(|r| r.to_string())
        .unwrap_or_default();
    let reason = if matches!(runner.stop_reason, Some(StopReason::Saturated)) {
        st.green(&reason)
    } else {
        st.yellow(&reason)
    };
    println!(
        "  {} {} classes, {} nodes, {} iterations, {:.1?} ({})",
        st.dim("e-graph  "),
        runner.egraph.number_of_classes(),
        runner.egraph.total_nodes(),
        runner.iterations.len(),
        runner.elapsed(),
        reason,
    );
    println!(
        "  {} {} rules from `{}`",
        st.dim("rules    "),
        opts.rules.len(),
        opts.rules_name
    );

    if args.has("stats") {
        println!();
        for line in runner.report().lines() {
            println!("  {}", st.dim(line));
        }
        println!();
        println!(
            "  {}",
            st.dim("iteration    classes    nodes   matches   time")
        );
        for it in &runner.iterations {
            println!(
                "  {:>9}  {:>9} {:>8}  {:>8}   {:.1?}",
                it.index,
                it.classes_after,
                it.nodes_after,
                it.total_matches,
                it.total_time()
            );
        }
    }
    Ok(())
}

fn cmd_eval(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let env: Env = parse_bindings(args.all("D").into_iter().chain(args.all("define")))?;

    let missing: Vec<Sym> = expr
        .vars()
        .into_iter()
        .filter(|v| !env.contains_key(v))
        .collect();
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
        return Err(format!(
            "unbound variable{}: {}\n  bind them with -D {}=1",
            if names.len() == 1 { "" } else { "s" },
            names.join(", "),
            names[0]
        ));
    }

    let expr = if args.has("opt") {
        let opts = Options::from(args)?;
        let (best, _, _) = opts.optimize(&expr);
        show_expr(st, "optimized", &best, false);
        best
    } else {
        expr
    };

    let v = eval(&expr, &env).map_err(|e| e.to_string())?;
    println!("{}", st.bold(&format_value(v)));
    Ok(())
}

fn format_value(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        }
    } else if v == 0.0 && v.is_sign_negative() {
        // `0` would hide the sign, and `-0.0` is a value this language can
        // tell apart from `0.0`.
        "-0".into()
    } else if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

fn cmd_diff(args: &Args, st: &Style) -> Result<(), String> {
    if args.positional.len() < 2 {
        return Err("usage: saturn diff <variable> <expression>".into());
    }
    let var = args.positional[0].clone();
    if !var
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
    {
        return Err(format!("`{}` is not a variable name", var));
    }
    let body = args.positional[1..].join(" ");
    // Parse the body on its own first so a syntax error points at the user's
    // text rather than at the wrapper this builds around it.
    let inner = parse(&body).map_err(|e| e.render())?;
    let wrapped = format!("d({}, {})", var, body);
    let expr = parse(&wrapped).map_err(|e| e.render())?;

    let mut opts = Options::from(args)?;
    if args.get("rules").is_none() {
        // Differentiation alone leaves the derivative unsimplified; the point
        // of doing it in an e-graph is that simplification happens at the same
        // time, so pull in the arithmetic rules unless told otherwise.
        opts.rules = rules::default_rules();
        opts.rules_name = "default".into();
    }
    let (best, _, runner) = opts.optimize(&expr);

    show_expr(st, "f", &inner, false);
    show_expr(st, &format!("df/d{}", var), &best, args.has("shared"));

    if best
        .reachable(best.root())
        .iter()
        .any(|&id| best.node(id).op == Op::Diff)
    {
        println!();
        println!(
            "  {}",
            st.yellow("the rules could not eliminate every d(...); the result is incomplete")
        );
    }
    if args.has("stats") {
        println!();
        for line in runner.report().lines() {
            println!("  {}", st.dim(line));
        }
    }
    Ok(())
}

fn cmd_ast(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    if args.has("sexp") {
        println!("{}", expr.to_sexp());
        return Ok(());
    }
    println!("  {}", st.dim("id   node                 refs"));
    let counts = expr.ref_counts(expr.root());
    for id in expr.reachable(expr.root()) {
        let n = expr.node(id);
        println!(
            "  {:<4} {:<20} {}",
            id.index(),
            format!("{:?}", n),
            counts[id.index()]
        );
    }
    println!();
    println!(
        "  {} {} shared nodes, {} in the expanded tree",
        st.dim("size"),
        expr.dag_size(),
        expr.tree_size()
    );
    Ok(())
}

fn cmd_egraph(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let opts = Options::from(args)?;
    let runner = opts.run(&expr);
    runner.egraph.check_invariants();
    if args.has("dot") {
        print!("{}", runner.egraph.to_dot());
        return Ok(());
    }
    print!("{}", runner.egraph.dump());
    println!();
    println!(
        "  {} root {}, {} classes, {} nodes",
        st.dim("summary"),
        runner.root(),
        runner.egraph.number_of_classes(),
        runner.egraph.total_nodes()
    );
    Ok(())
}

fn cmd_vm(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let expr = if args.has("raw") {
        expr
    } else {
        let opts = Options::from(args)?;
        opts.optimize(&expr).0
    };
    let prog = Program::compile(&expr).map_err(|e| e.to_string())?;
    print!("{}", prog.disassemble());
    let env: Env = parse_bindings(args.all("D").into_iter().chain(args.all("define")))?;
    if !env.is_empty() {
        match prog.eval_env(&env) {
            Ok(v) => println!("\n  {} {}", st.dim("result"), st.bold(&format_value(v))),
            Err(e) => println!("\n  {} {}", st.dim("result"), st.red(&e.to_string())),
        }
    }
    Ok(())
}

fn cmd_check(args: &Args, st: &Style) -> Result<bool, String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let opts = Options::from(args)?;
    let (best, _, _) = opts.optimize(&expr);

    let checker = Checker::new()
        .with_samples(args.num("samples")?.unwrap_or(5_000))
        .with_seed(args.num("seed")?.unwrap_or(0x5A7))
        .with_tolerance(args.num("tol")?.unwrap_or(1e-9))
        .with_wild(args.has("wild"));
    let report = checker.compare(&expr, &best);

    show_expr(st, "input", &expr, false);
    show_expr(st, "optimized", &best, false);
    println!();
    for line in report.render().lines() {
        println!("  {}", line);
    }
    if report.ok() {
        println!("  {}", st.green("the two agree on every sampled input"));
    } else {
        println!(
            "  {}",
            st.red("the optimized expression does not match the input")
        );
    }
    Ok(report.ok())
}

fn cmd_rules(args: &Args, st: &Style) -> Result<(), String> {
    match args
        .get("set")
        .or_else(|| args.positional.first().map(|s| s.as_str()))
    {
        None => {
            println!("  {}", st.dim("rule sets"));
            for (name, desc) in rules::set_names() {
                let n = rules::named(name).map(|r| r.len()).unwrap_or(0);
                println!("  {:<16} {:>4}  {}", st.bold(name), n, st.dim(desc));
            }
            println!();
            println!("  {}", st.dim("list one with `saturn rules <name>`"));
        }
        Some(name) => {
            let set = rules::named(name).ok_or_else(|| format!("unknown rule set `{}`", name))?;
            for r in &set {
                println!("  {:<24} {}", st.cyan(&r.name), r.long_name());
            }
            println!();
            println!("  {} {} rules", st.dim("total"), set.len());
        }
    }
    Ok(())
}

/// Expressions that exercise different parts of the engine, used by `bench`
/// and by the README.
const SUITE: &[(&str, &str)] = &[
    ("identity", "x * (1 + 0) * 1 - 0"),
    ("factor", "a * x + a * y + a * z"),
    ("cancel", "(p + q) * (p + q) / (p + q)"),
    ("powers", "(x ^ 2) ^ 3"),
    ("exp-fuse", "exp(a) * exp(b) * exp(c)"),
    ("log-ratio", "ln(a) - ln(b)"),
    ("trig", "sin(t) * sin(t) + cos(t) * cos(t)"),
    ("horner", "a*x^3 + b*x^2 + c*x + d"),
    ("divide", "u / w + v / w"),
    ("deriv", "d(x, x^3 + 2*x^2 + x)"),
    ("deriv-chain", "d(x, exp(sin(x * x)))"),
    ("sqrt-square", "sqrt(u) * sqrt(u) + sqrt(v * v)"),
    ("boolean", "if(a < b, x, if(a < b, y, z))"),
    ("big", "(a + b + c) * (a + b + c) * (a + b + c)"),
];

/// Does `expr` still contain a derivative the rules could not eliminate?
fn has_diff(expr: &RecExpr) -> bool {
    expr.reachable(expr.root())
        .iter()
        .any(|&id| expr.node(id).op == Op::Diff)
}

fn cmd_bench(args: &Args, st: &Style) -> Result<(), String> {
    let mut opts = Options::from(args)?;
    if args.get("rules").is_none() {
        // The safe tier alone leaves most of the suite untouched by design;
        // the whole point of the table is what the rules can do together.
        opts.rules = rules::all_rules();
        opts.rules_name = "all".into();
    }
    // The suite exists to be run often, so it is bounded far more tightly
    // than a one-off invocation would be. Every entry below saturates well
    // inside these limits or is not improved by more room.
    if args.get("iters").is_none() {
        opts.iters = 12;
    }
    if args.get("nodes").is_none() {
        opts.nodes = 5_000;
    }
    if args.get("time").is_none() {
        opts.time = Duration::from_millis(if args.has("quick") { 250 } else { 1_000 });
    }
    println!(
        "  {} {} rules from `{}`",
        st.dim("using"),
        opts.rules.len(),
        opts.rules_name
    );
    println!();
    println!(
        "  {}",
        st.dim("name           nodes -> nodes     ops -> ops      classes    time")
    );
    let mut total_before = 0.0;
    let mut total_after = 0.0;
    for (name, src) in SUITE {
        let expr = parse(src).map_err(|e| e.render())?;
        let (best, _, runner) = opts.optimize(&expr);
        let after = dag_cost(&best, &OpCost);
        // A `d(...)` has no runtime cost to speak of -- it is priced out of
        // reach precisely so extraction never returns one -- so quoting a
        // number for it would only corrupt the total.
        let before = if has_diff(&expr) {
            None
        } else {
            Some(dag_cost(&expr, &OpCost))
        };
        if let Some(b) = before {
            total_before += b;
            total_after += after;
        }
        println!(
            "  {:<12} {:>5} -> {:<5} {:>8} -> {:<8.0} {:>7}   {:>7.1?}",
            name,
            expr.dag_size(),
            best.dag_size(),
            before
                .map(|b| format!("{:.0}", b))
                .unwrap_or_else(|| "-".into()),
            after,
            runner.egraph.number_of_classes(),
            runner.elapsed(),
        );
    }
    println!();
    println!(
        "  {} {} {}",
        st.dim("overall"),
        st.green(&pct(total_before, total_after)),
        st.dim("(derivatives excluded: they have no runtime cost to compare against)")
    );
    Ok(())
}

/// Measure how long one evaluation takes, averaged over many.
///
/// A cost model is a guess about the machine. This is the machine answering.
fn measure(run: &mut dyn FnMut(usize) -> f64, calls: usize) -> f64 {
    // Warm up, then take the best of several rounds: the minimum is the
    // measurement least polluted by whatever else the machine was doing.
    std::hint::black_box(run(calls / 4));
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        std::hint::black_box(run(calls));
        let ns = t.elapsed().as_secs_f64() * 1e9 / calls as f64;
        best = best.min(ns);
    }
    best
}

fn cmd_emit(args: &Args, _st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let lang_name = args.get("lang").unwrap_or("c");
    let lang = Lang::parse(lang_name).ok_or_else(|| {
        format!(
            "unknown language `{}`; expected c, rust, or python",
            lang_name
        )
    })?;
    let name = args.get("name").unwrap_or("f");
    let expr = if args.has("raw") {
        expr
    } else {
        Options::from(args)?.optimize(&expr).0
    };
    print!("{}", emit(&expr, lang, name));
    Ok(())
}

fn cmd_time(args: &Args, st: &Style) -> Result<(), String> {
    let src = args.expr_arg()?;
    let expr = parse(&src).map_err(|e| e.render())?;
    let opts = Options::from(args)?;
    let calls: usize = args.num("calls")?.unwrap_or(200_000);

    let (best, _, runner) = opts.optimize(&expr);
    let raw = Program::compile(&expr).map_err(|e| e.to_string())?;
    let fast = Program::compile(&best).map_err(|e| e.to_string())?;

    // One set of inputs, drawn once, so every variant sees identical work.
    let mut rng = Rng::seed(args.num("seed")?.unwrap_or(0x5A7));
    let rows: Vec<Vec<f64>> = (0..1024)
        .map(|_| raw.params().iter().map(|_| rng.tame_float()).collect())
        .collect();
    let reordered: Vec<Vec<f64>> = rows
        .iter()
        .map(|r| {
            let env: Env = raw
                .params()
                .iter()
                .copied()
                .zip(r.iter().copied())
                .collect();
            fast.params()
                .iter()
                .map(|p| env.get(p).copied().unwrap_or(0.0))
                .collect()
        })
        .collect();

    let interpreted = {
        let envs: Vec<Env> = rows
            .iter()
            .map(|r| {
                raw.params()
                    .iter()
                    .copied()
                    .zip(r.iter().copied())
                    .collect()
            })
            .collect();
        measure(
            &mut |n| {
                let mut acc = 0.0;
                for i in 0..n {
                    acc += eval(&expr, &envs[i % envs.len()]).unwrap_or(0.0);
                }
                acc
            },
            calls / 20,
        )
    };
    let compiled = measure(
        &mut |n| {
            let mut slots = Vec::new();
            let mut acc = 0.0;
            for i in 0..n {
                acc += raw.eval_with(&rows[i % rows.len()], &mut slots);
            }
            acc
        },
        calls,
    );
    let optimized = measure(
        &mut |n| {
            let mut slots = Vec::new();
            let mut acc = 0.0;
            for i in 0..n {
                acc += fast.eval_with(&reordered[i % reordered.len()], &mut slots);
            }
            acc
        },
        calls,
    );

    show_expr(st, "input", &expr, false);
    show_expr(st, "optimized", &best, false);
    println!();
    println!("  {}", st.dim("                        ns/eval   speedup"));
    let row = |label: &str, ns: f64, base: f64, colour: bool| {
        let speedup = if base > 0.0 && ns > 0.0 {
            format!("{:.1}x", base / ns)
        } else {
            "-".to_string()
        };
        println!(
            "  {:<24} {:>7.1}   {}",
            label,
            ns,
            if colour {
                st.green(&speedup)
            } else {
                st.dim(&speedup)
            }
        );
    };
    row("interpreted", interpreted, interpreted, false);
    row("compiled", compiled, interpreted, compiled < interpreted);
    row(
        "compiled + optimized",
        optimized,
        interpreted,
        optimized < compiled,
    );
    println!();
    println!(
        "  {} {} -> {} instructions, {} -> {} slots, {} rules from `{}`",
        st.dim("program"),
        raw.len(),
        fast.len(),
        raw.slots,
        fast.slots,
        opts.rules.len(),
        opts.rules_name,
    );
    let _ = runner;
    Ok(())
}

fn cmd_fuzz(args: &Args, st: &Style) -> Result<bool, String> {
    let opts = Options::from(args)?;
    let count: usize = args.num("count")?.unwrap_or(1_000);
    let depth: usize = args.num("depth")?.unwrap_or(5);
    let seed: u64 = args.num("seed")?.unwrap_or(1);
    let tolerance: f64 = args.num("tol")?.unwrap_or(0.0);
    let grammar = if args.has("arith") {
        Grammar::arithmetic()
    } else {
        let g = Grammar::default();
        if args.has("logic") {
            g.with_logic()
        } else {
            g
        }
    };
    let checker = Checker::new()
        .with_samples(args.num("samples")?.unwrap_or(200))
        .with_seed(seed ^ 0x9E37_79B9_7F4A_7C15)
        .with_tolerance(tolerance)
        .with_wild(!args.has("tame"));

    println!(
        "  {} {} expressions, depth {}, {} rules from `{}`, tolerance {}",
        st.dim("fuzzing"),
        count,
        depth,
        opts.rules.len(),
        opts.rules_name,
        tolerance
    );
    let mut failures = 0;
    for (i, expr) in ExprStream::new(seed, grammar, depth)
        .take(count)
        .enumerate()
    {
        let (best, _, _) = opts.optimize(&expr);
        let report = checker.compare(&expr, &best);
        if !report.ok() {
            failures += 1;
            println!();
            println!("  {} expression {}", st.red("MISMATCH"), i);
            show_expr(st, "input", &expr, false);
            show_expr(st, "optimized", &best, false);
            for line in report.render().lines() {
                println!("  {}", line);
            }
            if failures >= args.num("max-failures")?.unwrap_or(5usize) {
                break;
            }
        }
    }
    println!();
    if failures == 0 {
        println!(
            "  {} every optimized expression agreed with its input",
            st.green("clean")
        );
    } else {
        println!(
            "  {} {} expression{} changed meaning",
            st.red("unsound"),
            failures,
            if failures == 1 { "" } else { "s" }
        );
        println!(
            "  {}",
            st.dim("replay one with the same --seed; the rule that fired is the bug")
        );
    }
    Ok(failures == 0)
}

fn cmd_repl(st: &Style) -> Result<(), String> {
    println!("{}", st.dim("saturn — enter an expression, or `:help`"));
    let mut env = Env::new();
    let mut opts_rules = rules::default_rules();
    let mut rules_name = "default".to_string();
    let stdin = std::io::stdin();
    loop {
        print!("{} ", st.cyan("λ"));
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            println!();
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix(':') {
            let mut it = rest.split_whitespace();
            match it.next().unwrap_or("") {
                "q" | "quit" | "exit" => return Ok(()),
                "help" => {
                    println!("  :let x = 1.5      bind a variable");
                    println!("  :env              show bindings");
                    println!("  :rules <name>     switch rule set");
                    println!("  :quit");
                }
                "env" => {
                    let mut ks: Vec<(&Sym, &f64)> = env.iter().collect();
                    ks.sort_by_key(|(s, _)| s.as_str());
                    for (k, v) in ks {
                        println!("  {} = {}", k, format_value(*v));
                    }
                }
                "let" => match parse_bindings([rest.trim_start_matches("let").trim()]) {
                    Ok(e) => env.extend(e),
                    Err(e) => println!("  {}", st.red(&e)),
                },
                "rules" => match it.next() {
                    Some(n) => match rules::named(n) {
                        Some(r) => {
                            opts_rules = r;
                            rules_name = n.to_string();
                            println!("  {} {} rules", st.dim(&rules_name), opts_rules.len());
                        }
                        None => println!("  {}", st.red("unknown rule set")),
                    },
                    None => println!("  {} ({} rules)", rules_name, opts_rules.len()),
                },
                other => println!("  {}", st.red(&format!("unknown command `:{}`", other))),
            }
            continue;
        }
        match parse(line) {
            Err(e) => println!("{}", st.red(&e.render())),
            Ok(expr) => {
                let runner = Runner::default()
                    .with_expr(&expr)
                    .with_iter_limit(20)
                    .with_time_limit(Duration::from_secs(3))
                    .with_scheduler(BackoffScheduler::default())
                    .run(&opts_rules);
                let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
                println!("  {}", st.bold(&best.pretty()));
                if best.vars().iter().all(|v| env.contains_key(v)) && !best.vars().is_empty() {
                    if let Ok(v) = eval(&best, &env) {
                        println!("  {} {}", st.dim("="), st.green(&format_value(v)));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------

const HELP: &str = "\
saturn — equality saturation for a small numeric language

USAGE
  saturn <command> [expression] [options]

COMMANDS
  opt <expr>            saturate and extract the cheapest equivalent expression
  eval <expr>           evaluate, with -D x=1.5 bindings
  diff <var> <expr>     differentiate symbolically, simplifying as it goes
  check <expr>          compare the optimized form against the original numerically
  emit <expr>           print the optimized expression as C, Rust, or Python
  vm <expr>             compile to bytecode and disassemble
  ast <expr>            show the parsed expression DAG
  egraph <expr>         dump the saturated e-graph (--dot for Graphviz)
  rules [set]           list the rule sets, or the rules in one
  time <expr>           measure interpreted, compiled, and optimized evaluation
  bench                 run the built-in suite and report the savings
  fuzz                  generate random expressions and check the rules are sound
  repl                  interactive

OPTIONS
  --rules <set>         safe | default | diff | arith | transcendental | logic |
                        fast-math | all | none          [default: default]
  --cost <model>        size | depth | ops              [default: ops]
  --iters <n>           saturation iteration limit      [default: 20]
  --nodes <n>           e-graph node limit              [default: 20000]
  --time <secs>         wall-clock limit                [default: 3]
  --scheduler <s>       backoff | simple                [default: backoff]
  -D name=value         bind a variable (repeatable)
  --shared              print shared subterms as `let` bindings
  --stats, -s           show per-iteration and per-rule statistics
  --samples <n>         inputs to try in `check`        [default: 5000]
  --seed <n>            random seed for `check`         [default: 1447]
  --tol <x>             relative tolerance for `check`  [default: 1e-9]
  --wild                let `check` use infinities and huge magnitudes
  --lang <l>            c | rust | python, for `emit`      [default: c]
  --name <id>           name of the emitted function        [default: f]
  --raw                 in `vm` and `emit`, skip optimizing first
  --count <n>           expressions to generate in `fuzz`  [default: 1000]
  --depth <n>           generated expression depth          [default: 5]
  --arith, --logic      restrict or widen the fuzz grammar
  --tame                keep fuzz inputs away from infinities and subnormals
  --color <when>        always | never | auto           [default: auto]
  -h, --help            this text
  -V, --version

EXAMPLES
  saturn opt 'x * y + x * z'
  saturn diff x 'exp(sin(x * x))'
  saturn opt '(a + b) / (a + b)' --rules fast-math --stats
  saturn check 'a*x^3 + b*x^2 + c*x + d' --samples 20000
  saturn vm 'u / w + v / w'
  saturn fuzz --rules safe --count 5000
  saturn emit 'a*x^3 + b*x^2 + c*x + d' --rules all --lang rust --name poly
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match Args::parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("saturn: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let st = Style::detect(args.get("color"));

    if args.has("version") {
        println!("saturn {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.has("help") || args.command.is_empty() || args.command == "help" {
        print!("{}", HELP);
        return if args.command.is_empty() && !args.has("help") {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    let result = match args.command.as_str() {
        "opt" | "optimize" | "simplify" => cmd_opt(&args, &st).map(|_| true),
        "eval" | "e" => cmd_eval(&args, &st).map(|_| true),
        "diff" | "d" | "derive" => cmd_diff(&args, &st).map(|_| true),
        "ast" | "parse" => cmd_ast(&args, &st).map(|_| true),
        "egraph" | "eg" => cmd_egraph(&args, &st).map(|_| true),
        "vm" | "compile" => cmd_vm(&args, &st).map(|_| true),
        "check" => cmd_check(&args, &st),
        "rules" => cmd_rules(&args, &st).map(|_| true),
        "bench" => cmd_bench(&args, &st).map(|_| true),
        "fuzz" => cmd_fuzz(&args, &st),
        "time" => cmd_time(&args, &st).map(|_| true),
        "emit" | "codegen" => cmd_emit(&args, &st).map(|_| true),
        "repl" => cmd_repl(&st).map(|_| true),
        other => Err(format!("unknown command `{}`; run `saturn --help`", other)),
    };

    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{} {}", st.red("saturn:"), e);
            ExitCode::FAILURE
        }
    }
}
