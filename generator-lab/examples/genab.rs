//! Full-generator A/B: the old `bb` strip-generate pipeline (`generator::run_attempts`)
//! vs the new-repr one (`generate::run_attempts`).
//!
//! Correctness is checked on a *yielding* target (train(NakedTriple)) where the
//! produced-puzzle fingerprint actually bites — identical Stats + fp means the two
//! engines strip to the same puzzles. Performance is then reported on the production
//! train/drill(HiddenQuad) targets (fixed-work us/attempt, new/old ratio).
//!
//! Run: cargo run --release -p generator-lab --example genab -- [--attempts N=6000] [--seed S=1]

use std::time::Instant;

use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use generator_lab::technique_kinds::NAKED_TRIPLE;
use generator_lab::{generate, generator};

fn main() {
    let mut attempts = 6000usize;
    let mut seed = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            _ => {}
        }
    }

    // ---- correctness on a yielding target: same seed -> same puzzles ----
    let probe = Spec::train(NAKED_TRIPLE);
    let (so, fo) = generator::run_attempts(&mut Rng::from_seed(seed), &probe, attempts);
    let (sn, fnp) = generate::run_attempts(&mut Rng::from_seed(seed), &probe, attempts);
    assert_eq!(fo, fnp, "fingerprint diverged on train(NakedTriple)");
    assert_eq!(
        (so.successes, so.not_forced, so.never_fired, so.total_givens),
        (sn.successes, sn.not_forced, sn.never_fired, sn.total_givens),
        "stats diverged on train(NakedTriple)"
    );
    println!(
        "genab: {attempts} attempts, seed {seed}\n\
         correctness: train(NakedTriple) {} puzzles, fp {fo:#018x} (old == new)\n",
        so.successes
    );

    // ---- performance on the production targets ----
    println!("{:<8} {:>12} {:>12} {:>10}", "mode", "old us/att", "new us/att", "new/old");
    for (name, spec) in [("train", spec_for_mode(0)), ("drill", spec_for_mode(1))] {
        let t = Instant::now();
        let _ = generator::run_attempts(&mut Rng::from_seed(seed), &spec, attempts);
        let old_us = t.elapsed().as_secs_f64() * 1e6 / attempts as f64;

        let t = Instant::now();
        let _ = generate::run_attempts(&mut Rng::from_seed(seed), &spec, attempts);
        let new_us = t.elapsed().as_secs_f64() * 1e6 / attempts as f64;

        println!("{name:<8} {old_us:>12.1} {new_us:>12.1} {:>9.2}x", new_us / old_us);
    }
}
