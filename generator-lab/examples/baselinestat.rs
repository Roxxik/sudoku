//! Workload study for the **baseline gate** — the scalar [`solve::FusedLogicSolver`]
//! the SIMT codepath still runs per lane. Drives the production `train`/`drill`
//! generator (the scalar [`generate::run_attempts`], byte-identical to the warp's
//! per-lane work) under `feature = "count"` and dumps the `FSTAT` breakdown, so the
//! SIMT-baseline-solver design is chosen from real numbers rather than a guess:
//!
//!   - how the work splits between the **vectorizable cheap closure** (singles + LC)
//!     and the **hard subset ladder** (naked/hidden pair..quad);
//!   - the cheap-closure depth (propagate passes per solve);
//!   - how rare a subset step actually is, and the HiddenQuad firing rate.
//!
//! Usage: cargo run --release --features count --example baselinestat -- [attempts=20000] [seed=1] [mode=0|1|both]
//! (mode 0 = train(HiddenQuad), 1 = drill(HiddenQuad). The cheap path is the same
//!  engine in both; drill's baseline is singles-only so it never reaches subsets.)

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("baselinestat requires --features count");
    std::process::exit(1);
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::generate::run_attempts;
    use generator_lab::rng::Rng;
    use generator_lab::solve::{fstat_reset, fstat_snapshot};
    use generator_lab::spec::kinds::NAMES;
    use generator_lab::subset_spec_for_mode;

    let mut args = std::env::args().skip(1);
    let attempts: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mode_arg = args.next().unwrap_or_else(|| "0".into());

    let modes: &[u32] = match mode_arg.as_str() {
        "both" => &[0, 1],
        "1" => &[1],
        _ => &[0],
    };

    for &mode in modes {
        let spec = subset_spec_for_mode(mode);
        let name = if mode == 0 { "train(HiddenQuad)" } else { "drill(HiddenQuad)" };
        fstat_reset();
        let mut rng = Rng::from_seed(seed);
        let (stats, _fp) = run_attempts(&mut rng, &spec, attempts);
        let f = fstat_snapshot();

        let calls = f[0].max(1) as f64;
        println!("=== baseline gate: {name}, {attempts} attempts (seed {seed}) ===");
        println!(
            "  attempts {}  successes {}  givens {}",
            stats.attempts, stats.successes, stats.total_givens
        );
        println!("  baseline solve calls         {:>12}", f[0]);
        println!("  calls/attempt                {:>15.2}", f[0] as f64 / attempts as f64);
        println!(
            "  propagate passes (closure)   {:>12}   {:>6.2}/call   (cheap-closure depth)",
            f[1], f[1] as f64 / calls
        );
        println!(
            "  calls reaching a subset step {:>12}   {:>6.3}%   <-- vectorize-vs-scalar lever",
            f[3], 100.0 * f[3] as f64 / calls
        );
        println!(
            "  total subset steps fired     {:>12}   {:>6.4}/call",
            f[2], f[2] as f64 / calls
        );
        println!(
            "  step_subsets invocations     {:>12}   {:>6.4}/call   (incl. unsolvable-stall None)",
            f[6], f[6] as f64 / calls
        );
        println!(
            "  calls solved                 {:>12}   {:>6.2}%",
            f[4], 100.0 * f[4] as f64 / calls
        );
        println!(
            "  calls firing HiddenQuad      {:>12}   {:>6.4}%",
            f[5], 100.0 * f[5] as f64 / calls
        );
        println!(
            "  drain entries {}  sieve recomputes {}  ({:.2} sieves/drain-entry, incl terminal pass)",
            f[8], f[7], f[7] as f64 / f[8].max(1) as f64
        );
        println!("  per-kind firing totals (cheap kinds 0..4 are fired-or-not, <=1/call):");
        for k in 0..10 {
            if f[10 + k] > 0 {
                println!("    {:<16} {:>12}", NAMES[k], f[10 + k]);
            }
        }
        println!();
    }
}
