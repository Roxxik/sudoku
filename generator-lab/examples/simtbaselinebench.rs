//! A/B for vectorizing the **baseline gate** in the SIMT generator. Compares the current
//! warp ([`run_warp`], packed prober + scalar per-lane baseline) against the prototypes that
//! also vectorize the baseline:
//!   - **two-warp, coupled** — a [`PackedProber`] feeds a [`PackedSolver`]:
//!     [`run_warp_pipelined`] (A1, continuous prober + batched solver flush) and
//!     [`run_warp_interleaved`] (one pass each per outer step). Both need lanes >> 8
//!     (oversubscription) to keep the baseline warp fed — `simtutil` shows baseline util
//!     48%@16 -> 86%@64.
//!   - **one warp, unified** — [`run_warp_unified`]: probe AND baseline lanes share a single
//!     [`UnifiedWarp`](generator_lab::solve::simt::UnifiedWarp), a slot flipping probe->
//!     baseline in place. ~100% util at active set = 8 (no oversubscription) — the win.
//! Swept over the in-flight logical-lane count; same total work everywhere.
//!
//! Correctness: each prototype is cross-checked against `run_warp` at a matched lane count
//! — per-lane `(stats, fp)` must be byte-identical (the packed baseline solver is pinned to
//! the scalar one, so deferring/batching it changes nothing).
//!
//! Usage: cargo run --release --example simtbaselinebench -- [total=96000] [reps=3]

use generator_lab::generate::random_simt::{
    WarpResult, run_warp, run_warp_interleaved, run_warp_pingpong, run_warp_pipelined,
    run_warp_simt, run_warp_unified, run_warp_unified_lean,
};
use generator_lab::spec::Spec;
use generator_lab::spec_for_mode;
use std::time::Instant;

fn check_match(name: &str, spec: &Spec, lanes: usize, per_lane: usize) {
    let bar = run_warp(1, spec, lanes, per_lane);
    let mine = match name {
        "pipelined" => run_warp_pipelined(1, spec, lanes, per_lane),
        "simt" => run_warp_simt(1, spec, lanes, per_lane),
        "interleaved" => run_warp_interleaved(1, spec, lanes, per_lane),
        "unified" => run_warp_unified(1, spec, lanes, per_lane),
        "unified_lean" => run_warp_unified_lean(1, spec, lanes, per_lane),
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
        check_match("interleaved", &spec, 16, 2000);
        check_match("unified", &spec, 16, 2000);
        check_match("unified_lean", &spec, 16, 2000);

        let bar = time(total, reps, || run_warp(1, &spec, 8, total / 8));
        println!("== {label} ==   (total {total} att, {reps} reps, best of)");
        println!("  run_warp (scalar baseline, 8 lanes) : {bar:>8.3} us/att   1.00x");

        // The unified warp keeps its active set at 8 intrinsically (8 in flight regardless
        // of the macro-lane count), so its natural config is L=8; the larger L only change
        // total work per macro-lane, not occupancy. Reported across the same sweep for A/B.
        for &lanes in &[8usize, 16, 24, 32, 64] {
            let per_lane = total / lanes;
            let pl = if lanes >= 16 { time(total, reps, || run_warp_pipelined(1, &spec, lanes, per_lane)) } else { f64::NAN };
            let un = time(total, reps, || run_warp_unified(1, &spec, lanes, per_lane));
            let ul = time(total, reps, || run_warp_unified_lean(1, &spec, lanes, per_lane));
            println!(
                "  L={lanes:<4} opportunistic {pl:>8.3} {:>5.2}x    unified {un:>8.3} {:>5.2}x    unified_lean {ul:>8.3} {:>5.2}x",
                bar / pl,
                bar / un,
                bar / ul,
            );
        }
        println!();
    }
}
