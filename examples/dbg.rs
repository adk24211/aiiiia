use saturn::*;
fn main() {
    let rules: Vec<Rewrite<MathAnalysis>> = vec![
        rw!("assoc-add"; "(?a + ?b) + ?c" => "?a + (?b + ?c)"),
        rw!("assoc-mul"; "(?a * ?b) * ?c" => "?a * (?b * ?c)"),
        rw!("add-0"; "?a + 0" => "?a"),
        rw!("mul-1"; "?a * 1" => "?a"),
        rw!("mul-0"; "?a * 0" => "0"),
        rw!("distribute"; "?a * (?b + ?c)" => "?a * ?b + ?a * ?c"),
        rw!("factor"; "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
        rw!("sub-to-add"; "?a - ?b" => "?a + -1 * ?b"),
    ];
    let e = parse(&std::env::args().nth(1).unwrap_or("x * (1 + 0) * 1".into())).unwrap();
    let r = Runner::default().with_iter_limit(12).with_time_limit(std::time::Duration::from_secs(30)).with_expr(&e).run(&rules);
    for it in &r.iterations {
        println!("{:>3}  classes {:>6}  nodes {:>6}  matches {:>7}  unions {:>5}  {:?}  banned={:?}",
            it.index, it.classes_after, it.nodes_after, it.total_matches,
            it.applied.values().sum::<usize>(), it.total_time(), it.banned);
    }
    println!("{}", r.report());
}
