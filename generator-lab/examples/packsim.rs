//! Packing-efficiency predictor: before building any SIMT/SIMD/GPU "pack N
//! attempts" prober, measure whether the per-prober-call cost is uniform enough
//! for lockstep to pay. Runs attempts under `--features count`, harvests the
//! per-call `ALT_STATS` (nodes = existence-DFS node count, 1 = branchless
//! propagate-to-resolution, >1 = it branched), and reports:
//!
//!   - the cost distribution (percentiles, branchless share),
//!   - SIMT efficiency vs lane width N under two bracketing schedulers:
//!       * NO-REFILL  (fixed warps run to the slowest lane): the pessimistic
//!         ceiling of a naive packer with no work-stealing. eff = mean / E[max_N].
//!       * PERFECT-REFILL (work-stealing fills a lane the instant it finishes):
//!         eff = min(1, total / (N * max_call)). The gap between the two is the
//!         value of building a refill scheduler.
//!
//! Cost proxy is per-call DFS `nodes`. It ignores within-call propagation-wave
//! length (which a real packer also locksteps), so treat absolute numbers as a
//! shape indicator, not a guarantee.
//!
//! Usage: cargo run --release -p generator-lab --example packsim --features count -- [--attempts N=8000]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("packsim needs the `count` feature: cargo run --release -p generator-lab --example packsim --features count");
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{alt_stats, alt_stats_reset};
    use generator_lab::generator::run_attempts;
    use generator_lab::rng::Rng;
    use generator_lab::spec_for_mode;

    let attempts = std::env::args()
        .skip_while(|a| a != "--attempts")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8000usize);

    println!("packsim: {attempts} attempts/mode, seed 1 (cost proxy = per-call DFS nodes)\n");

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        alt_stats_reset();
        let spec = spec_for_mode(mode);
        let mut rng = Rng::from_seed(1);
        let _ = run_attempts(&mut rng, &spec, attempts);

        let stats = alt_stats();
        let n = stats.len();
        if n == 0 {
            println!("== {label} ==  no prober calls\n");
            continue;
        }
        let mut nodes: Vec<f64> = stats.iter().map(|s| s.nodes as f64).collect();
        let total: f64 = nodes.iter().sum();
        let mean = total / n as f64;
        let branchless = stats.iter().filter(|s| s.nodes <= 1).count();
        let branchless_share = branchless as f64 / n as f64;
        let nonunique = stats.iter().filter(|s| s.nonunique).count();
        // node-work share carried by the branching (nodes>1) minority.
        let branch_work: f64 = stats.iter().filter(|s| s.nodes > 1).map(|s| s.nodes as f64).sum();

        nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| nodes[((p * (n as f64 - 1.0)).round() as usize).min(n - 1)];
        let max = nodes[n - 1];

        println!("== {label} ==");
        println!(
            "  prober calls {n}  ({:.1}/attempt),  nonunique {:.2}%,  branchless(nodes<=1) {:.1}%  carrying {:.1}% of node-work",
            n as f64 / attempts as f64,
            100.0 * nonunique as f64 / n as f64,
            100.0 * branchless_share,
            100.0 * (total - branch_work) / total,
        );
        println!(
            "  nodes/call: mean {:.2}  p50 {:.0}  p90 {:.0}  p99 {:.0}  p99.9 {:.0}  max {:.0}",
            mean, pct(0.50), pct(0.90), pct(0.99), pct(0.999), max
        );

        // E[max of N] for sampling N calls with replacement, from the sorted CDF:
        //   P(max == v_i) = (i/n)^N - ((i-1)/n)^N.
        let e_max_n = |big_n: u32| -> f64 {
            let nn = n as f64;
            let mut e = 0.0;
            for (idx, &v) in nodes.iter().enumerate() {
                let i = (idx + 1) as f64;
                let p = (i / nn).powi(big_n as i32) - ((i - 1.0) / nn).powi(big_n as i32);
                e += v * p;
            }
            e
        };

        println!("  lane-width   no-refill eff (speedup)   perfect-refill eff (speedup)");
        for &big_n in &[4u32, 8, 16, 32, 64, 256, 1024] {
            let emax = e_max_n(big_n);
            let eff_nr = mean / emax;
            let eff_pr = (total / (big_n as f64 * max)).min(1.0);
            println!(
                "    N={:<5}   {:>5.1}%  ({:>5.2}x)            {:>5.1}%  ({:>6.1}x)",
                big_n,
                100.0 * eff_nr,
                big_n as f64 * eff_nr,
                100.0 * eff_pr,
                big_n as f64 * eff_pr,
            );
        }
        println!();
    }
}
