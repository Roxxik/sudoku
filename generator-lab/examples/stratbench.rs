//! Strategy bench: MRV vs Bivalue [`BranchStrategy`] on the fill (banded rep), now
//! that the strategy is a swappable type parameter. Bivalue was the *prober's* win
//! (post-propagation the board is stuck, so a bivalue branch holds the factor at 2).
//! This checks whether that transfers to the FILL — the hypothesis is no: early in a
//! from-empty fill no cell is bivalue, so Bivalue degenerates toward naive
//! lowest-index branching while MRV takes the most-constrained cell.
//!
//! Each strategy is timed over `iters` grids from the same seed (it produces its own
//! valid grids; the two streams diverge, so there is no cross-fingerprint here — the
//! `bivalue_strategy_swaps_in` test already pins correctness).
//!
//! Usage: cargo run --release -p generator-lab --example stratbench -- [--iters N=100000] [--seed S=1]

use std::time::Instant;

use generator_lab::fill::random_solution_with;
use generator_lab::rng::Rng;
use generator_lab::scan::{Bivalue, Mrv};

fn main() {
    let mut iters = 100_000usize;
    let mut seed = 1u64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--seed" => seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(seed),
            _ => {}
        }
    }

    let mrv = time::<Mrv>(iters, seed);
    let biv = time::<Bivalue>(iters, seed);

    println!("strategy bench: {iters} grids, seed {seed}\n");
    println!("  Mrv      {:>9.1} ns/grid", mrv);
    println!("  Bivalue  {:>9.1} ns/grid   ({:.2}x vs Mrv)", biv, biv / mrv);
}

fn time<S: generator_lab::scan::BranchStrategy>(iters: usize, seed: u64) -> f64 {
    let mut rng = Rng::from_seed(seed);
    // Fold a few cells into a sink so the fill can't be optimized away.
    let mut sink = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        let s = random_solution_with::<S>(&mut rng);
        let c = s.cells();
        sink ^= c[0].map_or(0, |d| d.get()) as u64
            ^ ((c[40].map_or(0, |d| d.get()) as u64) << 8)
            ^ ((c[80].map_or(0, |d| d.get()) as u64) << 16);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / iters as f64;
    std::hint::black_box(sink);
    ns
}
