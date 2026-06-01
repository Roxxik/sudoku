//! TEMPORARY: structural counts (scans / placements / clones per attempt) for
//! the instrumented probers. Engine-independent — tells us what to cut.

use solver_lab::counters::{reset, snapshot, snapshot_clone_split};
use solver_lab::harness::fingerprint_run;
use solver_lab::solvers::{UniqProber, banded, batch, bitboard_fb, bitboard_simd, light, lightbv};

fn run<P: UniqProber>(attempts: usize, seed: u64) {
    reset();
    let _ = fingerprint_run::<P>(attempts, seed);
    let (s, p, c) = snapshot();
    let (entry, builds) = snapshot_clone_split();
    let a = attempts as f64;
    let internal = c.saturating_sub(entry);
    println!(
        "{:<14} scans/att {:>9.1}  places/att {:>9.1}  clones/att {:>8.1}  places/clone {:>5.1}  scans/clone {:>5.2}",
        P::NAME,
        s as f64 / a,
        p as f64 / a,
        c as f64 / a,
        p as f64 / c as f64,
        s as f64 / c as f64,
    );
    if entry != 0 || builds != 0 {
        // Clone split (only the instrumented bitboard-simd reports these):
        // entry = per-query base clones, internal = solve_first branch clones,
        // builds = probe builds (the max clones the "skip last query" idea cuts).
        println!(
            "{:<14}   builds/att {:>7.1}  entry-clones/att {:>7.1}  internal-clones/att {:>7.1}  (skip-last ceiling: {:.1}%)",
            "",
            builds as f64 / a,
            entry as f64 / a,
            internal as f64 / a,
            100.0 * builds as f64 / c as f64,
        );
    }
}

fn main() {
    let attempts = 4000usize;
    let seed = 1u64;
    println!("structural counts, {attempts} attempts, seed {seed}\n");
    run::<batch::Probe>(attempts, seed);
    run::<light::Probe>(attempts, seed);
    run::<lightbv::Probe>(attempts, seed);
    run::<bitboard_simd::Probe>(attempts, seed);
    run::<bitboard_fb::Probe>(attempts, seed);
    run::<banded::Probe>(attempts, seed);
}
