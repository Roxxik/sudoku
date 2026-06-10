//! Correctness anchor for the `repr`-layer warp ([`generate::warp_host`]). The
//! [`GateStream`] runs each seed to its *first* success, racing W=8 seeds across the
//! warp; that produced puzzle must be byte-identical to the one scalar
//! [`generate`](generator_lab::generate::generate) yields from the same seed, regardless
//! of how the 8 slots interleave. Equality of the actual puzzle line (not just a count)
//! pins both faithfulness and that the packed prober's + baseline closure's verdicts match
//! the scalar `Search` prober / `FusedLogicSolver` the sequential path uses.
//!
//! Coverage note: because a seed is run to its first success, every *failing* attempt
//! along the way must also produce the scalar-identical verdict — otherwise the seed would
//! yield at a different attempt or a different puzzle. So a spec that needs several
//! attempts per seed (e.g. `train(NAKED_PAIR)`) transitively exercises the failure-path
//! verdicts too, not just the success path.

use generator_lab::generate::generate;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::NAKED_PAIR;

/// Drain a finite-seed [`GateStream`] to its terminal, collecting every `(seed, puzzle)`.
fn collect<I: Iterator<Item = u64>>(spec: &Spec, seeds: I) -> Vec<(u64, generator_lab::generate::GeneratedPuzzle)> {
    let mut stream = GateStream::new(seeds, spec);
    let mut produced = Vec::new();
    loop {
        match stream.pump(4096) {
            Pumped::Found(seed, p) => produced.push((seed, p)),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    produced
}

/// The stream's seed -> puzzle map must be a pure function of the seed: each seed's batched
/// puzzle equals the one scalar [`generate`] produces from the same seed, regardless of how
/// the W=8 warp interleaves the seeds.
///
/// Uses `train(NakedPair)` rather than the production HiddenQuad spec purely so the test is
/// fast: it yields in a handful of attempts per seed yet still drives the same fused fast
/// path (both singles + both LC allowed, a non-cheap forced kind). The batch has more seeds
/// than slots, so they genuinely interleave.
fn check_per_seed(spec: &Spec, base: u64, count: u64) {
    let mut produced = collect(spec, base..base + count);
    produced.sort_by_key(|(s, _)| *s); // streamed out of order

    // One puzzle per seed.
    assert_eq!(produced.len() as u64, count, "one puzzle per seed");
    for (i, (seed, _)) in produced.iter().enumerate() {
        assert_eq!(*seed, base + i as u64, "produced[{i}] seed (after sort)");
    }

    for (seed, p) in &produced {
        let mut rng = Rng::from_seed(*seed);
        let (scalar, _) = generate(&mut rng, spec, 5_000_000);
        let scalar = scalar.expect("scalar generate must yield for this seed");
        assert_eq!(
            p.puzzle.to_line(),
            scalar.puzzle.to_line(),
            "seed {seed}: batched puzzle != scalar generate"
        );
        assert_eq!(
            p.solution.to_line(),
            scalar.solution.to_line(),
            "seed {seed}: solution mismatch"
        );
    }
}

#[test]
fn stream_matches_scalar_per_seed() {
    check_per_seed(&Spec::train(NAKED_PAIR), 1, 20);
}

/// Non-contiguous and out-of-order seed iterators must work the same — the result is keyed
/// by seed, not by position.
#[test]
fn stream_non_contiguous_seeds() {
    let spec = Spec::train(NAKED_PAIR);
    let seeds = [97u64, 3, 51, 2, 88, 17];
    let mut produced = collect(&spec, seeds.into_iter());
    produced.sort_by_key(|(s, _)| *s);

    let mut sorted = seeds;
    sorted.sort();
    assert_eq!(produced.iter().map(|(s, _)| *s).collect::<Vec<_>>(), sorted);

    for (seed, p) in &produced {
        let mut rng = Rng::from_seed(*seed);
        let scalar = generate(&mut rng, &spec, 5_000_000).0.expect("yield");
        assert_eq!(p.puzzle.to_line(), scalar.puzzle.to_line(), "seed {seed}");
    }
}
