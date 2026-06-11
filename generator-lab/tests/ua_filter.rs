//! UA strip pre-filter acceptance: the filter is trajectory-identical (docs/UA-FILTER.md).
//!
//! The filter only ever fast-rejects gates the prober would revert, so the strip walk — and
//! hence the produced puzzles and the run tallies — must be bit-identical with the filter on
//! or off. This pins that for both engines (scalar `run_attempts`, SIMT `GateStream`) at
//! every UA tier, on train(HiddenQuad) and drill(HiddenQuad). A false positive would flip a
//! verdict and break a fingerprint here. (Cross-engine equality is `tests/equiv_warp_repr`.)

use generator_lab::fingerprint::grid_fp;
use generator_lab::generate::warp_host::{GateStream, Pumped};
use generator_lab::generate::{Stats, UaTier, run_attempts_ua};
use generator_lab::rng::Rng;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::NAKED_PAIR;

/// The strip walk goes fully to minimal regardless of the forced target (the target only
/// gates which stripped state is remembered as `best`), so the UA filter — which lives in
/// the spec-invariant uniqueness gate — sees the identical deep strip walk for any target.
/// We force NAKED_PAIR rather than the production HiddenQuad because it is common enough to
/// yield puzzles in a small budget (HiddenQuad, the rarest forceable single, yields ~none),
/// so the produced-puzzle comparison is actually exercised. `mode` 0 = train, 1 = drill.
fn spec(mode: u32) -> Spec {
    if mode == 0 { Spec::train(NAKED_PAIR) } else { Spec::drill(NAKED_PAIR) }
}

const ATTEMPTS: usize = 4000;
/// Seeds for the SIMT per-seed comparison (each yields ~one puzzle per ~10 attempts).
const SEEDS: u64 = 400;
const TIERS: [UaTier; 2] = [UaTier::Ua4, UaTier::Full];

/// Scalar: `run_attempts` is attempt-based on one RNG stream (a clean, fixed-work boundary),
/// and its fingerprint folds successes, not-forced and never-fired, so it is sensitive to
/// any trajectory change on its own. Returns `(tallies, fingerprint)`.
fn scalar(mode: u32, tier: UaTier) -> (Stats, u64) {
    let mut rng = Rng::from_seed(1);
    run_attempts_ua(&mut rng, &spec(mode), ATTEMPTS, tier)
}

/// SIMT: `GateStream` is a *seed-based* streaming engine (one puzzle per seed, lanes
/// interleaved). Capping on attempts-started is boundary-sensitive when per-attempt cost
/// changes, so we instead drain a *fixed* seed range fully and key the produced puzzles by
/// seed: the seed -> puzzle map is independent of lane interleaving and of the UA tier (the
/// per-seed trajectory is identical), which is exactly what this pins. Returns the
/// seed-sorted per-seed puzzle fingerprints plus the (then-deterministic) tallies.
fn simt(mode: u32, tier: UaTier) -> (Stats, Vec<(u64, u64)>) {
    let mut stream = GateStream::new_ua(1u64..=SEEDS, &spec(mode), true, tier);
    let mut puzzles: Vec<(u64, u64)> = Vec::new();
    loop {
        match stream.pump(64) {
            Pumped::Found(seed, p) => puzzles.push((seed, grid_fp(&p.puzzle.0))),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    puzzles.sort();
    (stream.stats(), puzzles)
}

#[test]
fn scalar_fingerprint_tier_invariant() {
    for mode in [0u32, 1u32] {
        let off = scalar(mode, UaTier::Off);
        assert!(off.0.successes > 0, "scalar mode {mode}: no yield in budget (test would be weak)");
        for tier in TIERS {
            assert_eq!(off, scalar(mode, tier), "scalar mode {mode} tier {tier:?}: trajectory drift");
        }
    }
}

#[test]
fn simt_fingerprint_tier_invariant() {
    for mode in [0u32, 1u32] {
        let off = simt(mode, UaTier::Off);
        assert_eq!(off.1.len(), SEEDS as usize, "SIMT mode {mode}: not every seed yielded");
        for tier in TIERS {
            assert_eq!(off, simt(mode, tier), "SIMT mode {mode} tier {tier:?}: trajectory drift");
        }
    }
}
