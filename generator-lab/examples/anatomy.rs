//! Baseline anatomy: how often is each technique SCANNED per attempt (and per
//! baseline call)? scan-count × scan-size ≈ cost, so this says what inside
//! `baseline` to optimize. Needs `--features count`.
//!
//! cargo run --release -p generator-lab --example anatomy --features count -- [--attempts N=4000]

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{
        CTR_NAMES, PCTR_NAMES, ctr_reset, ctr_snapshot, pctr_reset, pctr_snapshot,
    };
    use generator_lab::generator::run_attempts;
    use generator_lab::rng::Rng;
    use generator_lab::spec_for_mode;

    let attempts = std::env::args()
        .skip_while(|a| a != "--attempts")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000usize);

    println!("baseline anatomy: {attempts} attempts/mode, seed 1\n");
    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        ctr_reset();
        pctr_reset();
        generator_lab::bb::band_ctr_reset();
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(1);
        let _ = run_attempts(&mut rng, &spec, attempts);
        let c = ctr_snapshot();
        let pc = pctr_snapshot();
        let per_att = |x: u64| x as f64 / attempts as f64;
        let bcalls = c[0].max(1);
        println!("== {label} ==  ({:.1} baseline-calls/attempt)", per_att(c[0]));
        for i in 0..8 {
            println!(
                "  {:<16} {:>9.1}/att   {:>6.2}/baseline-call",
                CTR_NAMES[i],
                per_att(c[i]),
                c[i] as f64 / bcalls as f64,
            );
        }
        let acalls = pc[0].max(1);
        println!("  -- prober ({:.1} alt-calls/attempt) --", per_att(pc[0]));
        for i in 0..8 {
            println!(
                "  {:<20} {:>9.1}/att   {:>6.2}/alt-call",
                PCTR_NAMES[i],
                per_att(pc[i]),
                pc[i] as f64 / acalls as f64,
            );
        }
        let bc = generator_lab::bb::band_ctr_snapshot();
        use generator_lab::bb::BAND_CTR_NAMES;
        let pcalls = bc[0].max(1);
        let passes = bc[1].max(1);
        println!("  -- band_update ({:.1} propagate-calls/attempt) --", per_att(bc[0]));
        for i in 0..4 {
            println!("  {:<16} {:>10.1}/att   {:>6.2}/propagate", BAND_CTR_NAMES[i], per_att(bc[i]), bc[i] as f64 / pcalls as f64);
        }
        println!("  band-passes/propagate {:.2}   bd-scans/pass {:.1}   PRODUCTIVE {:.1}%  (waste = rescans dirty-tracking could skip)",
            bc[1] as f64 / pcalls as f64, bc[2] as f64 / passes as f64, 100.0 * bc[3] as f64 / bc[2].max(1) as f64);
        println!();
    }
}

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("rebuild with --features count");
}
