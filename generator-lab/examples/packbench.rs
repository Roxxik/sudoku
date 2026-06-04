//! End-to-end throughput: the warp (packed W=8 SIMT prober) vs generator-lab's
//! sequential scalar generator (lean `ProberBoard`), on the SAME total work and
//! producing the SAME puzzles (one seed per logical lane => the warp's union is
//! exactly the sequential run's; the yield assert pins it).
//!
//! `lanes` is the number of independent logical seed-streams, NOT a concurrency
//! knob: the streaming driver keeps exactly W=8 attempts in flight regardless, so
//! throughput is flat for `lanes >= 8` (the old macro-warp / FIFO-depth knob is
//! gone). The warp's parallelism comes from running 8 INDEPENDENT seed streams at
//! once, so this measures BATCH generation throughput (many puzzles), NOT
//! single-puzzle `find` latency — a single seed stream is inherently sequential
//! (attempts share one RNG stream; the uniqueness queries within an attempt are
//! sequential). For the faithful single-stream scalar number, use the `bench`
//! example.
//!
//! Usage: cargo run --release -p generator-lab --example packbench -- [lanes=8] [per_lane=4000]

use generator_lab::generator::run_attempts;
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;
use generator_lab::simt::host::run_warp;
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
