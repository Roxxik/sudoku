//! Isolated packed-prober throughput + profiling target: NOTHING but the W=8 SIMT
//! uniqueness prober ([`probe::simt::PackedProber`]). It harvests a faithful corpus of
//! probes from a real generator run ([`generate::random_simt::collect_probes`]) once, then
//! replays that corpus through [`PackedProber::resolve`] in a timed loop — no strip walk,
//! no scalar baseline gate, no grid fill in the measured region. So `perf` attributes
//! cleanly to the prober kernel (`warp_pass`/`smear_v`/`run_stream`/`branch_cell`/frame
//! snapshot+restore) instead of folding in the host-side gate resolution that `warpprof`
//! and `simtbench` still pay.
//!
//! Build + profile:
//!   cargo build --release -p generator-lab --features profiling --example proberbench
//!   perf record -g --call-graph dwarf target/release/examples/proberbench
//!   perf report --stdio | head -40
//!
//! Usage: cargo run --release -p generator-lab --example proberbench -- [corpus_att=8000] [reps=20] [mode=0]
//! (corpus_att = attempts whose gates seed the corpus; the corpus is ~50 probes/att.)

use generator_lab::generate::random_simt::{collect_probes, run_warp};
use generator_lab::probe::simt::PackedProber;
use generator_lab::subset_spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_att: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);
    let reps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let spec = subset_spec_for_mode(mode);

    // Harvest the corpus once (8 lanes share the work). Touch run_warp for the dstat
    // probe count parity check below; the corpus itself comes from collect_probes.
    let lanes = 8;
    let per_lane = corpus_att / lanes;
    let probes = collect_probes(1, &spec, lanes, per_lane);
    println!(
        "proberbench mode={mode}: corpus {} probes (from {} att), {reps} reps",
        probes.len(),
        per_lane * lanes
    );
    let _ = run_warp; // kept importable for ad-hoc A/B; not in the timed region

    let mut prober = PackedProber::new();
    let mut out = vec![false; probes.len()];

    // Warmup.
    prober.resolve(&probes, &mut out);
    let nonunique: usize = out.iter().filter(|&&b| b).count();

    let t = Instant::now();
    for _ in 0..reps {
        prober.resolve(&probes, &mut out);
    }
    let dt = t.elapsed();

    let total = reps as f64 * probes.len() as f64;
    let ns = dt.as_secs_f64() * 1e9 / total;
    std::hint::black_box(&out);
    println!(
        "  {:>7.2} ns/probe   ({:.3} M probes/s)   nonunique {}/{} ({:.1}%)",
        ns,
        1e3 / ns,
        nonunique,
        probes.len(),
        100.0 * nonunique as f64 / probes.len() as f64
    );
}
