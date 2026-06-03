//! Diagnostic: what fraction of packed-prober probes the cheap naked-single pass
//! settles (solved / contradiction) vs leaves to the scalar branching fallback
//! (stuck). This sizes whether the hybrid packed prober can pay — a high stuck
//! fraction means the packed pass is mostly wasted work before the scalar DFS.
//!
//! Usage: cargo run --release -p generator-pack --example packdiag --features count -- [lanes=8] [per_lane=4000]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("packdiag needs the `count` feature: cargo run --release -p generator-pack --example packdiag --features count");
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::packed::{pstat_reset, pstat_snapshot};
    use generator_lab::spec_for_mode;
    use generator_lab::warp::run_warp;

    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4000);

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        pstat_reset();
        let spec = spec_for_mode(mode);
        let _ = run_warp(1, &spec, lanes, per_lane);
        let [solved, contra, stuck] = pstat_snapshot();
        let total = (solved + contra + stuck).max(1);
        println!("== {label} ==  {total} probes");
        println!(
            "  packed-settled {:.1}%  (solved {:.1}%  contra {:.1}%)   stuck->scalar {:.1}%",
            100.0 * (solved + contra) as f64 / total as f64,
            100.0 * solved as f64 / total as f64,
            100.0 * contra as f64 / total as f64,
            100.0 * stuck as f64 / total as f64,
        );
    }
}
