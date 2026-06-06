//! Warp-only throughput + clean profiling target: runs NOTHING but the SIMT warp host
//! ([`generate::random_simt::run_warp`]) so `perf` attributes purely to the warp (the
//! packed prober `warp_pass` + the still-scalar baseline `FusedLogicSolver`), with no
//! sequential-generator contamination. Used to size the baseline gate's share of the
//! warp before/after vectorizing it.
//!
//! Usage: cargo run --release --example warpbench -- [lanes=8] [per_lane=8000] [reps=3]

use generator_lab::generate::random_simt::run_warp;
use generator_lab::spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);
    let reps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
    let total = lanes * per_lane;

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);
        run_warp(1, &spec, lanes, per_lane); // warmup
        let mut best = f64::MAX;
        for _ in 0..reps {
            let t = Instant::now();
            let res = run_warp(1, &spec, lanes, per_lane);
            let us = t.elapsed().as_secs_f64() * 1e6 / total as f64;
            best = best.min(us);
            std::hint::black_box(res.stats.successes);
        }
        println!("{label:<6} warp {best:>8.3} us/att   ({total} att)");
    }
}
