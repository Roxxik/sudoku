//! Correctness anchor for the `repr`-layer warp ([`generate::random_simt`]): each lane
//! runs a fixed seed, so lane `l` must produce byte-identical results to
//! [`generate::run_attempts`]'s *sequential* run from seed `l` — same tallies, same
//! fingerprint. If the warp ever reorders work in a way that changes a lane's outcome,
//! this catches it. The fingerprint folds every produced puzzle, so equality means the
//! actual puzzles match, not just the counts. (The spec `verify` inside the warp
//! independently guarantees every emitted puzzle satisfies the spec; this additionally
//! pins faithfulness — and that the new packed prober's verdicts match the scalar
//! `Search` prober the sequential path uses.)

use generator_lab::generate::random_simt::{
    find_puzzles, run_warp, run_warp_interleaved, run_warp_pingpong, run_warp_pipelined,
    run_warp_simt, run_warp_unified, run_warp_unified_lean,
};
use generator_lab::generate::{generate, run_attempts};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::NAKED_PAIR;
use generator_lab::spec_for_mode;

/// For each `mode`, run a warp of several lanes and check every lane reproduces the
/// sequential generator's `(stats, fp)` for its seed.
fn check_mode(mode: u32, base_seed: u64, lanes: usize, attempts_per_lane: usize) {
    let spec = spec_for_mode(mode);
    let res = run_warp(base_seed, &spec, lanes, attempts_per_lane);
    assert_eq!(res.per_lane.len(), lanes);

    for (l, &(stats, fp)) in res.per_lane.iter().enumerate() {
        let seed = base_seed + l as u64;
        let mut rng = Rng::from_seed(seed);
        let (ref_stats, ref_fp) = run_attempts(&mut rng, &spec, attempts_per_lane);

        assert_eq!(stats.attempts, ref_stats.attempts, "mode {mode} lane {l}: attempts");
        assert_eq!(stats.successes, ref_stats.successes, "mode {mode} lane {l}: successes");
        assert_eq!(stats.never_fired, ref_stats.never_fired, "mode {mode} lane {l}: never_fired");
        assert_eq!(stats.not_forced, ref_stats.not_forced, "mode {mode} lane {l}: not_forced");
        assert_eq!(stats.total_givens, ref_stats.total_givens, "mode {mode} lane {l}: total_givens");
        assert_eq!(fp, ref_fp, "mode {mode} lane {l}: fingerprint diverged (puzzles differ)");
    }
}

#[test]
fn train_warp_matches_sequential() {
    check_mode(0, 1, 8, 40);
}

/// The baseline-vectorizing two-warp hosts ([`run_warp_pipelined`] / [`run_warp_pingpong`])
/// defer + batch the baseline gate onto the packed solver instead of running it scalar
/// per lane. Since the packed solver is pinned byte-identical to the scalar one, every
/// logical lane's `(stats, fp)` must still match plain [`run_warp`] exactly — at a lane
/// count > 8 so the oversubscription/refill paths are exercised.
fn check_baseline_hosts(mode: u32, base_seed: u64, lanes: usize, attempts_per_lane: usize) {
    let spec = spec_for_mode(mode);
    let bar = run_warp(base_seed, &spec, lanes, attempts_per_lane);
    for (name, res) in [
        ("pipelined", run_warp_pipelined(base_seed, &spec, lanes, attempts_per_lane)),
        ("pingpong", run_warp_pingpong(base_seed, &spec, lanes, attempts_per_lane)),
        ("simt", run_warp_simt(base_seed, &spec, lanes, attempts_per_lane)),
        ("interleaved", run_warp_interleaved(base_seed, &spec, lanes, attempts_per_lane)),
        ("unified", run_warp_unified(base_seed, &spec, lanes, attempts_per_lane)),
        ("unified_lean", run_warp_unified_lean(base_seed, &spec, lanes, attempts_per_lane)),
    ] {
        assert_eq!(res.per_lane.len(), bar.per_lane.len(), "{name} mode {mode}: lane count");
        for (l, (a, b)) in res.per_lane.iter().zip(bar.per_lane.iter()).enumerate() {
            assert_eq!(a.0, b.0, "{name} mode {mode} lane {l}: stats diverged");
            assert_eq!(a.1, b.1, "{name} mode {mode} lane {l}: fingerprint diverged");
        }
    }
}

#[test]
fn baseline_hosts_match_run_warp() {
    check_baseline_hosts(0, 1, 24, 60);
    check_baseline_hosts(1, 1, 24, 60);
    // Offset seeds + a lane count not a multiple of the 8-wide warp.
    check_baseline_hosts(0, 500, 19, 40);
    check_baseline_hosts(1, 500, 19, 40);
}

#[test]
fn drill_warp_matches_sequential() {
    check_mode(1, 1, 8, 40);
}

/// A wider warp from a different seed base, to shake out lane-count / refill scheduling
/// assumptions (uneven attempt lengths across lanes).
#[test]
fn wide_warp_offset_seeds() {
    check_mode(0, 1000, 13, 25);
    check_mode(1, 1000, 13, 25);
}

/// The Success path: seed 4 yields a train puzzle at attempt ~18370, so a warp based at
/// seed 1 with 6 lanes (seeds 1..6, incl. 4) running past that yields at least one
/// success — the `verify`-accept + per-puzzle fingerprint fold that the rarer (0-yield)
/// seeds never exercise. Still must match the sequential run lane for lane. Slow (~6
/// lanes x 19k attempts), hence the explicit opt-in by name.
#[test]
#[ignore = "slow (~19k attempts/lane to reach the success path); run with --ignored"]
fn warp_success_path_matches_sequential() {
    check_mode(0, 1, 6, 19000);
}

/// [`find_puzzles`]'s seed -> puzzle map must be a pure function of the seed: each seed's
/// batched puzzle must equal the one scalar [`generate`] produces from the same seed,
/// regardless of how the W=8 warp interleaves the seeds. The per-attempt warp/scalar
/// verdict equivalence is already pinned above (`check_mode`); this pins the new
/// control flow — run a seed to its *first* success, then roll onto the next seed.
///
/// Uses `train(NakedPair)` rather than the production HiddenQuad spec purely so the test
/// is fast: it yields in a handful of attempts per seed yet still drives the same fused
/// fast path (both singles + both LC allowed, a non-cheap forced kind). The batch has
/// more seeds than slots, so they genuinely interleave.
fn check_find_puzzles(spec: &Spec, base: u64, count: u64) {
    let mut produced: Vec<(u64, _)> = Vec::new();
    let stats = find_puzzles(base..base + count, spec, |s, p| produced.push((s, p)));
    produced.sort_by_key(|(s, _)| *s); // streamed out of order

    // One puzzle per seed.
    assert_eq!(produced.len() as u64, count, "one puzzle per seed");
    assert_eq!(stats.successes as u64, count, "successes == seed count");
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
fn find_puzzles_matches_scalar_per_seed() {
    check_find_puzzles(&Spec::train(NAKED_PAIR), 1, 20);
}

/// Non-contiguous and out-of-order seed iterators must work the same — the result is
/// keyed by seed, not by position.
#[test]
fn find_puzzles_non_contiguous_seeds() {
    let spec = Spec::train(NAKED_PAIR);
    let seeds = [97u64, 3, 51, 2, 88, 17];
    let mut produced: Vec<(u64, _)> = Vec::new();
    find_puzzles(seeds, &spec, |s, p| produced.push((s, p)));
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
