//! Fill-only microbench: time `random_solution` (the full-grid MRV fill) in isolation,
//! with NO strip / prober / baseline around it, so `perf annotate` attributes cleanly to
//! the sieve / branch / place inside `Fill::fill`.
//!
//! A fingerprint folds every produced grid so any code change that alters the search is
//! caught (the grids must stay byte-identical for a given seed).
//!
//!   cargo build --release -p generator-lab --example fillbench
//!   perf record -g --call-graph dwarf target/release/examples/fillbench 200000
//!   perf annotate -i perf.data --stdio 'Fill<...>::fill' | head -120
use generator_lab::fill::random_solution;
use generator_lab::fingerprint::{FNV_OFFSET, fnv_fold_cells};
use generator_lab::repr::CELLS;
use generator_lab::rng::Rng;
use std::time::Instant;

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    // warmup
    let mut fp = FNV_OFFSET;
    for seed in 0..(n / 10).max(1) {
        let g = random_solution(&mut Rng::from_seed(seed));
        let cells: [u8; CELLS] = core::array::from_fn(|i| g.0.get(i).map_or(0, |d| d.get()));
        fnv_fold_cells(&mut fp, &cells);
    }
    let t = Instant::now();
    let mut fp = FNV_OFFSET;
    for seed in 0..n {
        let g = random_solution(&mut Rng::from_seed(seed));
        let cells: [u8; CELLS] = core::array::from_fn(|i| g.0.get(i).map_or(0, |d| d.get()));
        fnv_fold_cells(&mut fp, &cells);
    }
    let el = t.elapsed();
    println!(
        "fill: {n} grids  {:.3} us/grid  total {:.2} ms  fp={fp:#018x}",
        el.as_secs_f64() * 1e6 / n as f64,
        el.as_secs_f64() * 1e3
    );
}
