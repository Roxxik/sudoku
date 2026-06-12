//! Correctness anchor for the `repr`-layer warp ([`generate::warp_host`]). One seed = one
//! attempt: the [`GateStream`] runs each seed through a single strip walk, racing W=8 seeds
//! across the warp, and most seeds yield nothing. For every seed the warp's outcome must be
//! byte-identical to the one the scalar [`attempt`](generator_lab::generate::attempt)
//! produces from the same seed — both *whether* it yields and, when it does, the exact
//! puzzle + solution — regardless of how the 8 slots interleave. Equality of the produced
//! `seed -> puzzle` map (not just a count) pins both faithfulness and that the packed
//! prober's + baseline closure's verdicts match the scalar `Search` prober /
//! `FusedLogicSolver` the sequential path uses.
//!
//! Coverage note: a partial-yield spec (`train(NAKED_PAIR)`) is used on purpose — across a
//! range of seeds it exercises the failing attempts (`NeverFired` / `NotForced`) as well as
//! the successes, since the warp and the scalar path must agree the seed yields *nothing*
//! just as exactly as they agree on a produced puzzle.

use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{AttemptResult, attempt};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::NAKED_PAIR;

/// The scalar `seed -> (puzzle, solution)` map over `seeds`: one attempt per seed, keeping
/// only the seeds whose single strip walk succeeds. Sorted by seed for a stable compare.
fn scalar_map<I: Iterator<Item = u64>>(spec: &Spec, seeds: I) -> Vec<(u64, String, String)> {
    let mut out = Vec::new();
    for seed in seeds {
        let mut rng = Rng::from_seed(seed);
        if let AttemptResult::Success(p) = attempt(&mut rng, spec) {
            out.push((seed, p.puzzle.to_line(), p.solution.to_line()));
        }
    }
    out.sort_by_key(|(s, _, _)| *s);
    out
}

/// The warp's `seed -> (puzzle, solution)` map over `seeds`: drain the [`GateStream`]
/// (a finite seed feed terminates once every lane is idle). Sorted by seed.
fn warp_map<I: Iterator<Item = u64>>(spec: &Spec, seeds: I) -> Vec<(u64, String, String)> {
    let mut stream = GateStream::new(seeds, spec);
    let mut out = Vec::new();
    loop {
        match stream.pump(4096) {
            Pumped::Found(seed, p) => out.push((seed, p.puzzle.to_line(), p.solution.to_line())),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    out.sort_by_key(|(s, _, _)| *s);
    out
}

/// The warp's per-seed map must equal the scalar one over the same seeds, regardless of how
/// the W=8 warp interleaves them.
///
/// Uses `train(NakedPair)` rather than the production HiddenQuad spec partly so the test is
/// fast and partly so the yield is partial — many seeds produce nothing, a few produce a
/// puzzle, and the warp must reproduce both verdicts. It still drives the same fused fast
/// path (both singles + both LC allowed, a non-cheap forced kind). The batch has many more
/// seeds than slots, so they genuinely interleave.
fn check_per_seed(spec: &Spec, base: u64, count: u64) {
    let warp = warp_map(spec, base..base + count);
    let scalar = scalar_map(spec, base..base + count);

    // Partial yield is the whole point: confirm the range is neither all-miss nor all-hit,
    // so both the success and the failure verdicts are actually under test.
    assert!(!scalar.is_empty(), "spec yielded no puzzles over {count} seeds — widen the range");
    assert!(
        (scalar.len() as u64) < count,
        "spec yielded on every seed — pick a rarer spec so misses are exercised too"
    );

    assert_eq!(warp, scalar, "warp seed -> puzzle map != scalar attempt map");
}

#[test]
fn stream_matches_scalar_per_seed() {
    check_per_seed(&Spec::train(NAKED_PAIR), 1, 200);
}

/// Non-contiguous and out-of-order seed iterators must work the same — the result is keyed
/// by seed, not by position.
#[test]
fn stream_non_contiguous_seeds() {
    let spec = Spec::train(NAKED_PAIR);
    let seeds = [97u64, 3, 51, 2, 88, 17, 5, 41, 12, 73, 8, 60];
    let warp = warp_map(&spec, seeds.into_iter());
    let scalar = scalar_map(&spec, seeds.into_iter());
    assert_eq!(warp, scalar, "warp seed -> puzzle map != scalar attempt map (non-contiguous)");
}
