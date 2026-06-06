//! THROWAWAY: report the new `Search<Bivalue>` strip's prober node counts
//! (solve_first/count invocations) and propagation work, to see how much search work
//! (and how many band passes) the new path does per puzzle.
//! Build: cargo run --release -p generator-lab --features count --example nodecount

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("nodecount needs the `count` feature: cargo run --release -p generator-lab --features count --example nodecount");
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{band_ctr_reset, band_ctr_snapshot, pctr_reset, pctr_snapshot};
    use generator_lab::fill::random_solution;
    use generator_lab::generate::strip_to_minimal;
    use generator_lab::probe::Search;
    use generator_lab::repr::banded::{Bands, RowMajor};
    use generator_lab::rng::Rng;
    use generator_lab::scan::Bivalue;

    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000);

    pctr_reset();
    band_ctr_reset();
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let sol = random_solution(&mut rng);
        let _ = strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(&mut rng, &sol);
    }
    let nodes = pctr_snapshot()[2];
    let prop = band_ctr_snapshot()[0];
    let pass = band_ctr_snapshot()[1];

    println!("n={n}  (Search<Bivalue> strip)");
    println!("nodes/puzzle       {:8.1}", nodes as f64 / n as f64);
    println!("propagate/puzzle   {:8.1}", prop as f64 / n as f64);
    println!("band-pass/puzzle   {:8.1}", pass as f64 / n as f64);
}
