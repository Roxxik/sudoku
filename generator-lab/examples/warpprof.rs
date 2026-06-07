//! Warp-ONLY throughput + profiling target: runs just the packed SIMT prober warp
//! ([`generate::random_simt::run_warp`]), with no sequential baseline alongside it, so
//! `perf` attributes cleanly to the warp/prober (`simtbench` runs the scalar generator
//! too and pollutes the samples).
//!
//! Build + profile:
//!   cargo build --release -p generator-lab --features profiling --example warpprof
//!   perf record -g --call-graph dwarf target/release/examples/warpprof 8 8000
//!   perf report --stdio | head -60
//!
//! Usage: cargo run --release -p generator-lab --example warpprof -- [lanes=8] [per_lane=8000] [mode=0]

use generator_lab::generate::random_simt::run_warp;
use generator_lab::subset_spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let total = lanes * per_lane;
    let spec = subset_spec_for_mode(mode);

    let t = Instant::now();
    let res = run_warp(1, &spec, lanes, per_lane);
    let dt = t.elapsed();

    let us = dt.as_secs_f64() * 1e6 / total as f64;
    println!(
        "warpprof mode={mode}: {lanes} lanes x {per_lane} = {total} att   {:>8.3} us/att   yield {} / {}",
        us, res.stats.successes, res.stats.attempts
    );

    #[cfg(feature = "count")]
    {
        let d = generator_lab::probe::simt::dstat_snapshot();
        let (probes, ticks, branches, active_sum) = (d[0], d[1], d[2], d[3]);
        let util = active_sum as f64 / (generator_lab::probe::simt::LANES as u64 * ticks) as f64;
        println!(
            "  probes {probes}   ticks {ticks}   branch_nodes {branches}",
        );
        println!(
            "  utilization {:.3}   passes/probe {:.2}   branches/probe {:.3}",
            util,
            ticks as f64 / probes as f64,
            branches as f64 / probes as f64,
        );
        let p = generator_lab::probe::simt::pstat_snapshot();
        println!("  pstat [solved-no-branch, contra-no-branch, branched] = {:?}", p);
    }
}
