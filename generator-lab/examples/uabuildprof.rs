//! Isolated pooled UA-build profiler: fills a small solution pool once and rebuilds the
//! full 2-digit UA library over it many times, so `random_solution`'s fill amortizes out and
//! `perf annotate` sees `enumerate_2digit_packed` (+ its scalar tail) as the dominant symbol.
//!
//! Usage: cargo run --release -p generator-lab --example uabuildprof -- [boards=256] [repeats=4000] [seed=1]

use generator_lab::generate::ua_build_cost_pooled;

fn main() {
    let mut it = std::env::args().skip(1);
    let boards: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(256);
    let repeats: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let seed: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    let (nanos, uas) = ua_build_cost_pooled(seed, boards, repeats);
    let builds = (boards * repeats) as f64;
    println!(
        "Full build (pooled): {:.1} ns/board over {} builds, {:.1} UAs/board",
        nanos as f64 / builds,
        builds as u64,
        uas as f64 / builds
    );
}
