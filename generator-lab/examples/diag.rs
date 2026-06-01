//! tdoku-style breakdown of the existence-prober's work: WHERE do the DFS nodes
//! and guesses go? Splits every `any_alt_solves` query by outcome (proving the
//! strip unique vs finding a 2nd solution) and by board sparsity, and reports how
//! concentrated the cost is (does a small tail of queries dominate?). Guides which
//! restructuring is worth doing instead of guessing.
//!
//! cargo run --release -p generator-lab --example diag --features count -- [--attempts N=8000] [--mode train|drill]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("rebuild with --features count");
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::bb::{AltStat, alt_stats, alt_stats_reset};
    use generator_lab::generator::run_attempts;
    use generator_lab::rng::Rng;
    use generator_lab::spec_for_mode;

    let mut attempts = 8000usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }
    let label = if mode == 0 { "train" } else { "drill" };

    alt_stats_reset();
    let spec = spec_for_mode(mode);
    let mut rng = Rng::from_seed(1);
    let _ = run_attempts(&mut rng, &spec, attempts);
    let stats: Vec<AltStat> = alt_stats().to_vec();

    let total_calls = stats.len() as f64;
    let total_nodes: u64 = stats.iter().map(|s| s.nodes as u64).sum();
    let total_guesses: u64 = stats.iter().map(|s| s.guesses as u64).sum();
    let per_att = |x: f64| x / attempts as f64;

    println!("== {label} ==  {attempts} attempts\n");
    println!(
        "alt-calls/att {:.1}   nodes/att {:.0}   guesses/att {:.0}   nodes/call {:.2}   guesses/call {:.2}",
        per_att(total_calls),
        per_att(total_nodes as f64),
        per_att(total_guesses as f64),
        total_nodes as f64 / total_calls,
        total_guesses as f64 / total_calls,
    );

    // Outcome split: proving uniqueness (no 2nd solution) vs finding a 2nd solution.
    for (name, want) in [("unique  (no alt)", false), ("nonunique (alt) ", true)] {
        let sub: Vec<&AltStat> = stats.iter().filter(|s| s.nonunique == want).collect();
        let n = sub.len() as f64;
        let nodes: u64 = sub.iter().map(|s| s.nodes as u64).sum();
        let guesses: u64 = sub.iter().map(|s| s.guesses as u64).sum();
        println!(
            "  {name}: calls {:>5.1}%  nodes {:>5.1}%  guesses {:>5.1}%   nodes/call {:>6.2}  guesses/call {:>6.2}",
            100.0 * n / total_calls,
            100.0 * nodes as f64 / total_nodes as f64,
            100.0 * guesses as f64 / total_guesses as f64,
            nodes as f64 / n.max(1.0),
            guesses as f64 / n.max(1.0),
        );
    }

    // Concentration: sort by nodes desc, see what fraction of nodes the top X% of
    // calls account for (is the cost a fat tail of hard queries?).
    let mut by_nodes: Vec<u32> = stats.iter().map(|s| s.nodes).collect();
    by_nodes.sort_unstable_by(|a, b| b.cmp(a));
    println!("\n  node concentration (top fraction of calls -> share of all nodes):");
    for frac in [0.01, 0.05, 0.10, 0.25, 0.50] {
        let k = ((by_nodes.len() as f64) * frac) as usize;
        let head: u64 = by_nodes[..k].iter().map(|&x| x as u64).sum();
        println!("    top {:>4.0}%  -> {:>5.1}% of nodes", frac * 100.0, 100.0 * head as f64 / total_nodes as f64);
    }

    // Sparsity buckets: nodes/guesses by how many cells were empty when probed.
    println!("\n  by board sparsity (empties at probe):");
    println!("    {:>9}  {:>8}  {:>10}  {:>10}  {:>11}", "empties", "calls", "nodes/call", "guess/call", "nodes-share");
    let buckets: [(u16, u16); 6] = [(0, 20), (20, 30), (30, 40), (40, 50), (50, 60), (60, 81)];
    for (lo, hi) in buckets {
        let sub: Vec<&AltStat> = stats.iter().filter(|s| s.empties >= lo && s.empties < hi).collect();
        if sub.is_empty() {
            continue;
        }
        let n = sub.len() as f64;
        let nodes: u64 = sub.iter().map(|s| s.nodes as u64).sum();
        let guesses: u64 = sub.iter().map(|s| s.guesses as u64).sum();
        println!(
            "    {:>3}-{:<3}    {:>8.1}%  {:>10.2}  {:>10.2}  {:>10.1}%",
            lo, hi,
            100.0 * n / total_calls,
            nodes as f64 / n,
            guesses as f64 / n,
            100.0 * nodes as f64 / total_nodes as f64,
        );
    }
}
