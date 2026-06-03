//! Throughput of the warp vs generator-lab's sequential generator, on the SAME
//! total work. v0 batches the gate scalar-per-lane, so this is expected to be
//! roughly on par (or a touch slower from the interleaving overhead) — the point
//! is a fair baseline to measure the packed kernel against later, and to confirm
//! the warp produces identical aggregate yield.
//!
//! Usage: cargo run --release -p generator-pack --example packbench -- [lanes=8] [attempts_per_lane=4000]

use generator_lab::generator::run_attempts;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
use generator_lab::warp::run_warp;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let total = lanes * per_lane;
    let base_seed = 1u64;

    println!("packbench: {lanes} lanes x {per_lane} attempts = {total} total attempts/mode\n");

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);

        // --- sequential baseline (generator-lab): the same total work, one seed
        // per lane so the produced puzzles are exactly the warp's union. ---
        let t0 = Instant::now();
        let mut seq = SeqStats { attempts: 0, successes: 0, total_givens: 0 };
        for l in 0..lanes {
            let mut rng = Rng::from_seed(base_seed + l as u64);
            let (s, _fp) = run_attempts(&mut rng, &spec, per_lane);
            seq.attempts += s.attempts;
            seq.successes += s.successes;
            seq.total_givens += s.total_givens;
        }
        let seq_dt = t0.elapsed();

        // --- warp ---
        let t1 = Instant::now();
        let res = run_warp(base_seed, &spec, lanes, per_lane);
        let warp_dt = t1.elapsed();

        let seq_us = seq_dt.as_secs_f64() * 1e6 / total as f64;
        let warp_us = warp_dt.as_secs_f64() * 1e6 / total as f64;

        println!("== {label} ==");
        println!(
            "  sequential : {:>8.3} us/att   yield {} / {}   avg givens {:.2}",
            seq_us,
            seq.successes,
            seq.attempts,
            seq.total_givens as f64 / seq.successes.max(1) as f64
        );
        println!(
            "  warp       : {:>8.3} us/att   yield {} / {}   avg givens {:.2}",
            warp_us,
            res.stats.successes,
            res.stats.attempts,
            res.stats.total_givens as f64 / res.stats.successes.max(1) as f64
        );
        println!("  speedup    : {:>8.3}x  (warp vs sequential)", seq_us / warp_us);
        assert_eq!(res.stats.successes, seq.successes, "yield mismatch warp vs sequential");
        println!();
    }
}

/// Tiny local tally (avoids leaking generator-lab's GenStats type here).
struct SeqStats {
    attempts: usize,
    successes: usize,
    total_givens: usize,
}
