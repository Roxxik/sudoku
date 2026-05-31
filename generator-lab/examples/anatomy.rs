//! Baseline anatomy: how often is each technique SCANNED per attempt (and per
//! baseline call)? scan-count × scan-size ≈ cost, so this says what inside
//! `baseline` to optimize. Needs `--features count`.
//!
//! cargo run --release -p generator-lab --example anatomy --features count -- [--attempts N=4000]

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{CTR_NAMES, ctr_reset, ctr_snapshot};
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
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(1);
        let _ = run_attempts(&mut rng, &spec, attempts);
        let c = ctr_snapshot();
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
        println!();
    }
}

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("rebuild with --features count");
}
