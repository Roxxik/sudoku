//! Warp-only run (no sequential baseline) so a profiler sees just the packed
//! pipeline. Build with `--features profiling` to keep the packed kernels as
//! distinct frames.
//!
//! Usage: cargo run --release --features profiling -p generator-pack --example packrun -- [W=64] [per_lane=4000]

use generator_lab::spec_for_mode;
use generator_lab::simt::host::run_warp;

fn main() {
    let mut args = std::env::args().skip(1);
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(64);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let mut sink = 0u64;
    for mode in [0u32, 1u32] {
        let spec = spec_for_mode(mode);
        let res = run_warp(1, &spec, w, per_lane);
        sink = sink.wrapping_add(res.stats.attempts as u64 + res.stats.successes as u64);
    }
    println!("done: {sink}");
}
