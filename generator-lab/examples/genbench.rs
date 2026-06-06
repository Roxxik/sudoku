//! Uniqueness-only generation bench: strip random solutions to minimal uniquely-
//! solvable puzzles, NO baseline/spec gate, comparing the new prober engines against
//! each other.
//!
//! Every variant strips the same solutions in the same order (same seed -> same
//! fill RNG -> same grid + same shuffle), so they must all produce byte-identical
//! puzzles — the run asserts every variant's fingerprint matches the bar's, which is a
//! correctness cross-check of the whole new stack as much as a perf measurement.
//!
//! Variants: the new scan/sieve `Search` with three over-cap policies for the rare
//! all-`≥D` node — `Mrv` (recompute a full-depth sieve) at depths 2/3/4/5/9,
//! `LooseMrv` (give up, branch on whatever) at 2/3/4/5, `MrvRecount` (recount just
//! the stuck cells, no full recompute) at 2/3/4/5 — plus `Bivalue` at depth 3 (its
//! natural cap) and the composable `Singles`. `Mrv`/`MrvRecount` pick the identical
//! branch cell, so that pair is a clean recompute-vs-recount A/B.
//!
//! Run: `cargo run --release --example genbench -- [seeds]` (default 200).

use std::time::{Duration, Instant};

use generator_lab::fill::random_solution;
use generator_lab::generate::strip_to_minimal;
use generator_lab::grid::CELLS;
use generator_lab::probe::{Prober, Search, Singles};
use generator_lab::repr::SearchState;
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::rng::Rng;
use generator_lab::scan::{Bivalue, LooseMrv, Mrv, MrvRecount};
use generator_lab::util::{FNV_OFFSET, fnv_fold_cells};

/// The production banded packing for the new probers.
type M = Bands<RowMajor>;

/// One variant's result over `n` seeds.
struct Run {
    elapsed: Duration,
    fp: u64,
    givens: usize,
}

/// Strip with a new-stack prober `P` on the banded packing.
fn run_new<P: Prober<SearchState<M>>>(n: u64) -> Run {
    let mut fp = FNV_OFFSET;
    let mut givens = 0usize;
    let t = Instant::now();
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let solution = random_solution(&mut rng);
        let puzzle = strip_to_minimal::<M, P>(&mut rng, &solution);
        let cells: [u8; CELLS] = core::array::from_fn(|i| puzzle.get(i).map_or(0, |d| d.get()));
        fnv_fold_cells(&mut fp, &cells);
        givens += cells.iter().filter(|&&d| d != 0).count();
    }
    Run { elapsed: t.elapsed(), fp, givens }
}

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let variants: Vec<(&str, Run)> = vec![
        ("Search<Mrv<2>>", run_new::<Search<Mrv<2>>>(n)),
        ("Search<Mrv<3>>", run_new::<Search<Mrv<3>>>(n)),
        ("Search<Mrv<4>>", run_new::<Search<Mrv<4>>>(n)),
        ("Search<Mrv<5>>", run_new::<Search<Mrv<5>>>(n)),
        ("Search<Mrv<6>>", run_new::<Search<Mrv<6>>>(n)),
        ("Search<Mrv<7>>", run_new::<Search<Mrv<7>>>(n)),
        ("Search<LooseMrv<3>>", run_new::<Search<LooseMrv<3>>>(n)),
        ("Search<LooseMrv<4>>", run_new::<Search<LooseMrv<4>>>(n)),
        ("Search<LooseMrv<5>>", run_new::<Search<LooseMrv<5>>>(n)),
        ("Search<LooseMrv<6>>", run_new::<Search<LooseMrv<6>>>(n)),
        ("Search<LooseMrv<7>>", run_new::<Search<LooseMrv<7>>>(n)),
        ("Search<MrvRecount<3>>", run_new::<Search<MrvRecount<3>>>(n)),
        ("Search<MrvRecount<4>>", run_new::<Search<MrvRecount<4>>>(n)),
        ("Search<MrvRecount<5>>", run_new::<Search<MrvRecount<5>>>(n)),
        ("Search<MrvRecount<6>>", run_new::<Search<MrvRecount<6>>>(n)),
        ("Search<MrvRecount<7>>", run_new::<Search<MrvRecount<7>>>(n)),
        ("Search<Bivalue>", run_new::<Search<Bivalue>>(n)),
        ("Singles", run_new::<Singles>(n)),
    ];

    // Correctness: every variant must produce identical puzzles.
    let bar = &variants[0];
    for (name, run) in &variants {
        assert_eq!(run.fp, bar.1.fp, "{name} produced different puzzles than {}", bar.0);
    }

    let bar_us = bar.1.elapsed.as_secs_f64() * 1e6 / n as f64;
    println!(
        "{n} seeds, all fingerprints match ({:#018x}), avg givens {:.1}\n",
        bar.1.fp,
        bar.1.givens as f64 / n as f64,
    );
    println!("{:<24} {:>12} {:>12} {:>10}", "variant", "us/puzzle", "puzzles/s", "vs bar");
    for (name, run) in &variants {
        let us = run.elapsed.as_secs_f64() * 1e6 / n as f64;
        let per_s = n as f64 / run.elapsed.as_secs_f64();
        println!("{name:<24} {us:>12.1} {per_s:>12.0} {:>9.2}x", us / bar_us);
    }
}
