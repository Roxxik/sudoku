//! Unified-warp ONLY throughput + profiling target: runs just
//! [`generate::random_simt::run_warp_unified`] (probe + baseline on one warp), so `perf`
//! attributes cleanly to the unified kernel/host without the other variants polluting the
//! samples (`simtbaselinebench` runs every prototype). The unified warp is full at
//! active-set 8, so its remaining cost is the per-pass kernel + the scalar residue
//! (strip-walk / advance_to_gate / branch_cell / subset_step) — this is the target to find
//! the next lever.
//!
//! Build + profile (the `profiling` feature marks warp_pass(_full)/branch_cell
//! `inline(never)` for clean symbols):
//!   cargo build --release -p generator-lab --features profiling --example unifiedprof
//!   perf record -g --call-graph dwarf target/release/examples/unifiedprof 8 12000 0
//!   perf report --stdio | head -60
//!
//! Usage: cargo run --release -p generator-lab --example unifiedprof -- [lanes=8] [per_lane=12000] [mode=0]

use generator_lab::generate::random_simt::run_warp_unified;
use generator_lab::subset_spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12_000);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let total = lanes * per_lane;
    let spec = subset_spec_for_mode(mode);

    let t = Instant::now();
    let res = run_warp_unified(1, &spec, lanes, per_lane);
    let dt = t.elapsed();

    let us = dt.as_secs_f64() * 1e6 / total as f64;
    let label = if mode == 0 { "train" } else { "drill" };
    println!(
        "unifiedprof {label}: {lanes} lanes x {per_lane} = {total} att   {us:>8.3} us/att   yield {} / {}",
        res.stats.successes, res.stats.attempts
    );
}
