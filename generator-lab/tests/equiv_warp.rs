//! Correctness anchor for the v0 warp: each lane runs a fixed seed, so lane `l`
//! must produce byte-identical results to generator-lab's *sequential* run from
//! seed `l` — same tallies, same fingerprint. If the warp ever reorders work in a
//! way that changes a lane's outcome, this catches it. The fingerprint folds
//! every produced puzzle, so equality means the actual puzzles match, not just
//! the counts. (The spec `verify` inside the warp independently guarantees every
//! emitted puzzle satisfies the spec; this test additionally pins faithfulness.)

use generator_lab::generator::run_attempts;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
use generator_lab::warp::run_warp;

/// For each `mode`, run a warp of several lanes and check every lane reproduces
/// the sequential generator's `(stats, fp)` for its seed.
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

#[test]
fn drill_warp_matches_sequential() {
    check_mode(1, 1, 8, 40);
}

/// A wider warp from a different seed base, to shake out lane-count / refill
/// scheduling assumptions (uneven attempt lengths across lanes).
#[test]
fn wide_warp_offset_seeds() {
    check_mode(0, 1000, 13, 25);
    check_mode(1, 1000, 13, 25);
}

/// The Success path: seed 4 yields a train puzzle at attempt ~18370, so a warp
/// based at seed 1 with 6 lanes (seeds 1..6, incl. 4) running past that yields at
/// least one success — the `verify`-accept + per-puzzle fingerprint fold that the
/// rarer (0-yield) seeds never exercise. Still must match the sequential run lane
/// for lane. Slow (~6 lanes x 20k attempts), hence the explicit opt-in by name.
#[test]
#[ignore = "slow (~20k attempts/lane to reach the success path); run with --ignored"]
fn warp_success_path_matches_sequential() {
    check_mode(0, 1, 6, 19000);
}
