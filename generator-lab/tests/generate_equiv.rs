//! End-to-end equivalence: the new-repr generator (`generate::run_attempts`, on the
//! layered prober/solver stack) must produce byte-identical puzzles to the old `bb`
//! generator (`generator::run_attempts`) for every seed and spec.
//!
//! The fingerprint folds *produced puzzles only* (the engine-equivalence contract:
//! "same seed -> same puzzles"), so it is only meaningful on a spec that actually
//! yields. HiddenQuad almost never does — `find`/`findpar` cover it specifically — so
//! the byte-level check runs on **NakedTriple / HiddenTriple**: frequent enough that
//! the fp bites, hard enough to exercise the subset ladder and the harder baseline
//! gate (not just the singles fast path). A couple of cheaper targets add breadth, and
//! the production HiddenQuad specs get a `Stats`-only smoke check (the NotForced /
//! NeverFired split is a real signal even with no successes).
//!
//! This composes the per-gate equivalences proven elsewhere (genbench: new prober ==
//! bb byte-identical; bb_equiv/solvebench: fused baseline == bb verdict; logic_equiv:
//! LogicSolver == fused/scalar) through the actual attempt loop, best tracking, and
//! verify — the glue this port adds.

use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::technique_kinds::{
    HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, LC_POINTING, NAKED_TRIPLE,
};
use generator_lab::{generate, generator};

/// Old `generator::Stats` and new `generate::Stats` are distinct types; compare field
/// by field.
fn assert_stats_eq(old: &generator::Stats, new: &generate::Stats, label: &str) {
    assert_eq!(old.attempts, new.attempts, "{label}: attempts");
    assert_eq!(old.successes, new.successes, "{label}: successes");
    assert_eq!(old.never_fired, new.never_fired, "{label}: never_fired");
    assert_eq!(old.not_forced, new.not_forced, "{label}: not_forced");
    assert_eq!(old.total_givens, new.total_givens, "{label}: total_givens");
}

/// Run both generators over `n` attempts for `spec`; assert identical `Stats` and
/// identical produced-puzzle fingerprint. `min_succ` guards that the fp actually bit
/// (a target that never yields would pass the fp check vacuously).
fn check(label: &str, spec: &Spec, n: usize, seed: u64, min_succ: usize) {
    let (old_stats, old_fp) = generator::run_attempts(&mut Rng::from_seed(seed), spec, n);
    let (new_stats, new_fp) = generate::run_attempts(&mut Rng::from_seed(seed), spec, n);
    assert_stats_eq(&old_stats, &new_stats, label);
    assert_eq!(old_fp, new_fp, "{label}: produced-puzzle fingerprint diverged");
    assert!(
        new_stats.successes >= min_succ,
        "{label}: only {} successes (< {min_succ}) — fp check would be vacuous",
        new_stats.successes
    );
}

/// The primary byte-level check: triple targets yield real puzzles AND exercise the
/// subset ladder + harder baseline gate, so the produced-puzzle fingerprint genuinely
/// constrains both engines.
#[test]
fn new_generator_matches_bb_on_triples() {
    check("train(NakedTriple)", &Spec::train(NAKED_TRIPLE), 4000, 1, 5);
    check("train(HiddenTriple)", &Spec::train(HIDDEN_TRIPLE), 4000, 1, 5);
    check("train(NakedTriple) seed7", &Spec::train(NAKED_TRIPLE), 4000, 7, 5);
}

/// Cheaper targets for breadth: they cover the singles / locked-candidates gate shapes
/// and yield in bulk, so the fingerprint folds thousands of puzzles.
#[test]
fn new_generator_matches_bb_on_cheap_targets() {
    check("train(HiddenSingle)", &Spec::train(HIDDEN_SINGLE), 3000, 1, 100);
    check("train(LcPointing)", &Spec::train(LC_POINTING), 3000, 1, 100);
}

/// The production specs almost never yield, so this is a `Stats`-only smoke check: the
/// NotForced / NeverFired split must still match (same best/verify decisions). Byte-
/// level HiddenQuad equivalence is `find`/`findpar`'s job, not the fingerprint's.
#[test]
fn new_generator_matches_bb_stats_on_hidden_quad() {
    for (label, spec) in [
        ("train(HiddenQuad)", Spec::train(HIDDEN_QUAD)),
        ("drill(HiddenQuad)", Spec::drill(HIDDEN_QUAD)),
    ] {
        let (old_stats, _) = generator::run_attempts(&mut Rng::from_seed(1), &spec, 3000);
        let (new_stats, _) = generate::run_attempts(&mut Rng::from_seed(1), &spec, 3000);
        assert_stats_eq(&old_stats, &new_stats, label);
    }
}
