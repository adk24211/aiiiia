//! The interval domain must be *sound*: whatever the concrete operation
//! produces has to lie inside the abstract result.
//!
//! These tests build each interval as the hull of a set of concrete points, so
//! the points are known to be inside it by construction, and then check that
//! applying the transfer function over-approximates applying the real
//! operation to every pair of points.

use saturn::interval::Interval;
use saturn::rng::Rng;

/// Does `x` lie inside `i`?
fn contains(i: &Interval, x: f64) -> bool {
    if x.is_nan() {
        return i.nan;
    }
    // The bounds are outward-rounded, so a plain comparison is the right test.
    x >= i.lo && x <= i.hi
}

fn hull(points: &[f64]) -> Interval {
    points
        .iter()
        .map(|&x| Interval::point(x))
        .reduce(|a, b| a.join(b))
        .expect("hull of no points")
}

fn sample_sets(rng: &mut Rng, n: usize) -> Vec<Vec<f64>> {
    (0..n)
        .map(|_| {
            let k = 1 + rng.below(4);
            (0..k).map(|_| rng.float()).collect()
        })
        .collect()
}

macro_rules! check_unary {
    ($name:ident, $abstract:ident, $concrete:expr) => {
        #[test]
        fn $name() {
            let mut rng = Rng::seed(0xA11CE);
            for points in sample_sets(&mut rng, 400) {
                let i = hull(&points);
                let r = i.$abstract();
                for &x in &points {
                    let concrete: fn(f64) -> f64 = $concrete;
                    let y = concrete(x);
                    assert!(
                        contains(&r, y),
                        "{}({}) = {} escaped {} (from {})",
                        stringify!($abstract),
                        x,
                        y,
                        r,
                        i
                    );
                }
            }
        }
    };
}

macro_rules! check_binary {
    ($name:ident, $abstract:ident, $concrete:expr) => {
        #[test]
        fn $name() {
            let mut rng = Rng::seed(0xB0B);
            let sets = sample_sets(&mut rng, 120);
            for a_points in &sets {
                for b_points in sets.iter().take(20) {
                    let (ia, ib) = (hull(a_points), hull(b_points));
                    let r = ia.$abstract(ib);
                    for &x in a_points {
                        for &y in b_points {
                            let concrete: fn(f64, f64) -> f64 = $concrete;
                            let z = concrete(x, y);
                            assert!(
                                contains(&r, z),
                                "{} {} {} = {} escaped {} (from {} and {})",
                                x,
                                stringify!($abstract),
                                y,
                                z,
                                r,
                                ia,
                                ib
                            );
                        }
                    }
                }
            }
        }
    };
}

check_unary!(neg_is_sound, neg, |x| -x);
check_unary!(abs_is_sound, abs, f64::abs);
check_unary!(sqrt_is_sound, sqrt, f64::sqrt);
check_unary!(exp_is_sound, exp, f64::exp);
check_unary!(ln_is_sound, ln, f64::ln);
check_unary!(floor_is_sound, floor, f64::floor);
check_unary!(ceil_is_sound, ceil, f64::ceil);
check_unary!(sin_is_sound, bounded_trig, f64::sin);
check_unary!(cos_is_sound, bounded_trig, f64::cos);

check_binary!(add_is_sound, add, |a, b| a + b);
check_binary!(sub_is_sound, sub, |a, b| a - b);
check_binary!(mul_is_sound, mul, |a, b| a * b);
check_binary!(div_is_sound, div, |a, b| a / b);
check_binary!(min_is_sound, min, f64::min);
check_binary!(max_is_sound, max, f64::max);

#[test]
fn sign_is_sound() {
    let mut rng = Rng::seed(0xC0FFEE);
    for points in sample_sets(&mut rng, 400) {
        let i = hull(&points);
        let r = i.sign();
        for &x in &points {
            let s = if x.is_nan() {
                f64::NAN
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            };
            assert!(contains(&r, s), "sign({}) = {} escaped {}", x, s, r);
        }
    }
}

#[test]
fn pow_with_a_literal_exponent_is_sound() {
    let mut rng = Rng::seed(0xD1CE);
    for points in sample_sets(&mut rng, 300) {
        let base = hull(&points);
        for e in [-3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0, 0.5, 2.5] {
            let r = base.pow(Interval::point(e));
            for &x in &points {
                let y = x.powf(e);
                assert!(
                    contains(&r, y),
                    "{}^{} = {} escaped {} (base {})",
                    x,
                    e,
                    y,
                    r,
                    base
                );
            }
        }
    }
}

#[test]
fn meet_keeps_both_facts() {
    let a = Interval::new(-5.0, 10.0);
    let b = Interval::new(0.0, 3.0);
    let m = a.meet(b);
    assert_eq!(m.lo, 0.0);
    assert_eq!(m.hi, 3.0);
    assert!(!m.nan);
}

#[test]
fn meet_of_disagreeing_facts_stays_conservative() {
    // Two facts that cannot both hold mean some rule was unsound. The domain
    // must widen rather than report an impossible range that would then let
    // another rule fire on a false premise.
    let a = Interval::new(0.0, 1.0);
    let b = Interval::new(5.0, 6.0);
    let m = a.meet(b);
    assert!(m.lo <= 0.0 && m.hi >= 6.0, "meet collapsed to {}", m);
}

#[test]
fn predicates_match_their_meanings() {
    assert!(Interval::new(1.0, 2.0).is_positive());
    assert!(Interval::new(0.0, 2.0).is_nonneg());
    assert!(!Interval::new(0.0, 2.0).is_positive());
    assert!(Interval::new(-2.0, -1.0).is_negative());
    assert!(Interval::new(1.0, 2.0).is_finite_nonzero());
    assert!(!Interval::new(-1.0, 1.0).is_nonzero());
    assert!(!Interval::TOP.is_finite());
    assert!(!Interval::TOP.is_nonzero());
    assert!(!Interval::REAL.is_finite(), "infinities are not finite");
    assert_eq!(Interval::point(3.5).as_constant(), Some(3.5));
    assert_eq!(Interval::point(f64::INFINITY).as_constant(), None);
    assert_eq!(Interval::point(f64::NAN).as_constant(), None);

    // NaN poisons every predicate: an interval that may be NaN cannot be
    // used to discharge a side condition.
    let maybe_nan = Interval {
        lo: 1.0,
        hi: 2.0,
        nan: true,
    };
    assert!(!maybe_nan.is_positive());
    assert!(!maybe_nan.is_nonzero());
    assert!(!maybe_nan.is_finite());
}

#[test]
fn zero_times_infinity_admits_nan() {
    let zero = Interval::point(0.0);
    let inf = Interval::point(f64::INFINITY);
    assert!(
        zero.mul(inf).nan,
        "0 * inf is NaN and the domain must say so"
    );
    assert!(inf.mul(zero).nan);
}

#[test]
fn infinity_minus_infinity_admits_nan() {
    let inf = Interval::point(f64::INFINITY);
    assert!(inf.sub(inf).nan);
    assert!(inf.add(Interval::point(f64::NEG_INFINITY)).nan);
}

#[test]
fn division_by_a_straddling_interval_is_unbounded() {
    let num = Interval::new(1.0, 2.0);
    let den = Interval::new(-1.0, 1.0);
    let q = num.div(den);
    assert_eq!(q.lo, f64::NEG_INFINITY);
    assert_eq!(q.hi, f64::INFINITY);
}
