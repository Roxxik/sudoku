//! A/B for vectorizing the **baseline gate** in the SIMT generator. Compares the current
//! warp ([`run_warp`], packed prober + scalar per-lane baseline) against the two decoupled
//! two-warp prototypes that also vectorize the baseline onto the packed
//! [`PackedSolver`](generator_lab::solve::simt::PackedSolver):
//!   - [`run_warp_pipelined`] (A1, nested flush: continuous prober, batched solver flush),
//!   - [`run_warp_pingpong`]  (A2, alternating full prober/solver passes),
//! swept over the in-flight logical-lane count (oversubscription) since both need lanes >> 8
//! to keep both warps fed. Same total work everywhere.
//!
//! Correctness: each prototype is cross-checked against `run_warp` at a matched lane count
//! — per-lane `(stats, fp)` must be byte-identical (the packed baseline solver is pinned to
//! the scalar one, so deferring/batching it changes nothing).
//!
//! Usage: cargo run --release --example simtbaselinebench -- [total=96000] [reps=3]

use generator_lab::generate::random_simt::{
    WarpResult, run_warp, run_warp_pingpong, run_warp_pipelined,
};
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use std::time::Instant;

fn check_match(name: &str, spec: &Spec, lanes: usize, per_lane: usize) {
    let bar = run_warp(1, spec, lanes, per_lane);
    let mine = match name {
        "pipelined" => run_warp_pipelined(1, spec, lanes, per_lane),
        _ => run_warp_pingpong(1, spec, lanes, per_lane),
    };
    assert_eq!(mine.per_lane.len(), bar.per_lane.len(), "{name}: lane count");
    for (l, (a, b)) in mine.per_lane.iter().zip(bar.per_lane.iter()).enumerate() {
        assert_eq!(a.0, b.0, "{name}: lane {l} stats diverged");
        assert_eq!(a.1, b.1, "{name}: lane {l} fp diverged");
    }
}

fn time<F: Fn() -> WarpResult>(total: usize, reps: usize, f: F) -> f64 {
    f(); // warmup
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        let r = f();
        best = best.min(t.elapsed().as_secs_f64() * 1e6 / total as f64);
        std::hint::black_box(r.stats.successes);
    }
    best
}

fn main() {
    let mut args = std::env::args().skip(1);
    let total: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(96_000);
    let reps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);

        // Correctness: prototypes match run_warp at a matched (small) lane count.
        check_match("pipelined", &spec, 16, 2000);
        check_match("pingpong", &spec, 16, 2000);

        let bar = time(total, reps, || run_warp(1, &spec, 8, total / 8));
        println!("== {label} ==   (total {total} att, {reps} reps, best of)");
        println!("  run_warp (scalar baseline, 8 lanes) : {bar:>8.3} us/att   1.00x");

        for &lanes in &[32usize, 64, 128, 256, 512] {
            let per_lane = total / lanes;
            let pl = time(total, reps, || run_warp_pipelined(1, &spec, lanes, per_lane));
            let pp = time(total, reps, || run_warp_pingpong(1, &spec, lanes, per_lane));
            println!(
                "  L={lanes:<4} pipelined {pl:>8.3} us/att {:>6.2}x   pingpong {pp:>8.3} us/att {:>6.2}x",
                bar / pl,
                bar / pp
            );
        }
        println!();
    }
}
