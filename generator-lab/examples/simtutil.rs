//! Warp-utilization study for the pipelined SIMT host ([`run_warp_pipelined`]): for a
//! sweep of in-flight macro-lane counts L, reports each warp's average active-lane
//! occupancy — probe via the prober's `DSTAT[3]/(8*DSTAT[1])`, baseline via the solver's
//! `SSTAT[1]/(8*SSTAT[0])` — to size the minimal hot buffer (does 8+8 reach ~100%?).
//!
//! Usage: cargo run --release --features count --example simtutil -- [total=48000]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("simtutil requires --features count");
    std::process::exit(1);
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::generate::random_simt::{
        run_warp_pipelined, run_warp_unified, run_warp_unified_lean,
    };
    use generator_lab::generate::random_simt::WarpResult;
    use generator_lab::probe::simt::{dstat_reset, dstat_snapshot};
    use generator_lab::solve::{sstat_reset, sstat_snapshot, uwstat_reset, uwstat_snapshot};
    use generator_lab::spec::Spec;
    use generator_lab::subset_spec_for_mode;

    let total: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(48_000);

    let measure = |f: &dyn Fn() -> WarpResult| {
        dstat_reset();
        sstat_reset();
        let res = f();
        std::hint::black_box(res.stats.successes);
        let d = dstat_snapshot();
        let s = sstat_snapshot();
        let probe_util = d[3] as f64 / (8.0 * d[1] as f64);
        let base_util = s[1] as f64 / (8.0 * s[0] as f64);
        (100.0 * probe_util, 100.0 * base_util, d[1], s[0])
    };

    // The unified warp reports a SINGLE combined utilization (uwstat) — one warp, so its
    // occupancy is the whole story (no separate probe/baseline split) — plus its total
    // warp-pass count, directly comparable to opportunistic's probe + baseline passes summed.
    let measure_unified = |f: &dyn Fn() -> WarpResult| {
        uwstat_reset();
        let res = f();
        std::hint::black_box(res.stats.successes);
        let u = uwstat_snapshot();
        (100.0 * u[1] as f64 / (8.0 * u[0] as f64), u[0])
    };

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec: Spec = subset_spec_for_mode(mode);
        println!("== {label} ==  ({total} att)   probe-util / baseline-util / base-passes");
        for &lanes in &[8usize, 16, 32] {
            let per_lane = total / lanes;
            let (uu, up) = measure_unified(&|| run_warp_unified(1, &spec, lanes, per_lane));
            let (ul, lp) = measure_unified(&|| run_warp_unified_lean(1, &spec, lanes, per_lane));
            println!(
                "  L={lanes:<4} unified: warp {uu:>5.1}% ({up} passes)   unified_lean: warp {ul:>5.1}% ({lp} lean passes; columns recovered scalar)",
            );
        }
        let _ = &measure;
        let _ = run_warp_pipelined;
        println!();
    }
}
