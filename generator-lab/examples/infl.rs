//! Node-inflation diagnostic: the packed prober propagates with naked singles
//! ONLY, while the scalar prober uses naked + hidden singles + locked candidates.
//! Less propagation => more branch nodes per probe. This measures that inflation
//! apples-to-apples on the SAME workload:
//!
//!   - scalar (vendored `bb` prober, via the scalar generator): per-probe
//!     solve_first nodes, sieve-waves, branch-points  [bb PCTR counters],
//!   - packed (the warp's PackedProber): per-probe branch-nodes and per-lane
//!     propagation work, plus lane utilization  [packed DSTAT counters].
//!
//! Inflation = packed-per-probe / scalar-per-probe. If branches inflate ~Nx, the
//! packed propagation has to be >Nx cheaper per unit to come out ahead.
//!
//! Usage: cargo run --release -p generator-pack --example infl --features count -- [W=16] [per_lane=2000]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("infl needs the `count` feature: cargo run --release -p generator-pack --example infl --features count");
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{pctr_reset, pctr_snapshot};
    use generator_lab::generate::run_attempts;
    use generator_lab::simt::prober::{dstat_reset, dstat_snapshot};
    use generator_lab::rng::Rng;
    use generator_lab::spec_for_mode;
    use generator_lab::simt::host::run_warp;

    let mut args = std::env::args().skip(1);
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let base = 1u64;

    println!("infl: W={w} x {per_lane} attempts = {} total/mode (seeds {base}..{})\n", w * per_lane, base + w as u64);

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);

        // --- scalar prober work, same seeds/attempts as the warp ---
        pctr_reset();
        for l in 0..w {
            let mut rng = Rng::from_seed(base + l as u64);
            let _ = run_attempts(&mut rng, &spec, per_lane);
        }
        let p = pctr_snapshot();
        let (s_probes, s_nodes, s_waves, s_branch) = (p[0], p[2], p[3], p[5]);

        // --- packed prober work (the warp), identical workload ---
        dstat_reset();
        let _ = run_warp(base, &spec, w, per_lane);
        let d = dstat_snapshot();
        let (k_probes, k_waves, k_branch, k_lanework) = (d[0], d[1], d[2], d[3]);

        let per = |num: u64, den: u64| num as f64 / den.max(1) as f64;
        println!("== {label} ==");
        println!("  probes:            scalar {s_probes}   packed {k_probes}   (should match)");
        println!(
            "  scalar/probe:      nodes {:.2}   sieve-waves {:.2}   branch-points {:.3}",
            per(s_nodes, s_probes), per(s_waves, s_probes), per(s_branch, s_probes)
        );
        println!(
            "  packed/probe:      warp-passes {:.2}   branches {:.3}",
            per(k_waves, k_probes), per(k_branch, k_probes)
        );
        println!(
            "  INFLATION:         branches {:.2}x   prop-passes(packed vs scalar waves) {:.2}x",
            per(k_branch, k_probes) / per(s_branch, s_probes).max(1e-9),
            per(k_waves, k_probes) / per(s_waves, s_probes).max(1e-9),
        );
        // `k_lanework` = sum of active lanes over all warp passes; with refill the
        // warp runs to empty, so utilization is active-lanes / (8 * passes).
        println!(
            "  warp utilization:  {:.1}%  (avg active {:.2} lanes / 8 over {k_waves} passes)",
            100.0 * per(k_lanework, 8 * k_waves),
            per(k_lanework, k_waves),
        );
        println!();
    }
}
