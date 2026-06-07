//! "No-branch probe => baseline-trivial?" study. The uniqueness prober is *weaker* than
//! the baseline at propagation (singles only, no columns/LC), so a probe that resolves
//! without branching suggests the baseline would settle trivially too — and if it also
//! never fires a subset, HiddenQuad can't have fired, so req is not met deterministically
//! and the baseline solve could be SKIPPED for those gates. This measures, per unique gate
//! (the only ones the baseline runs on), the correlation between probe-branch and the
//! baseline outcome.
//!
//! Usage: cargo run --release --features count --example branchcorr -- [lanes=32] [per_lane=2000]

#[cfg(not(feature = "count"))]
fn main() {
    eprintln!("branchcorr requires --features count");
    std::process::exit(1);
}

#[cfg(feature = "count")]
fn main() {
    use generator_lab::generate::random_simt::{CorrGroup, branch_baseline_corr};
    use generator_lab::spec_for_mode;

    let mut args = std::env::args().skip(1);
    let lanes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(32);
    let per_lane: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);

    let pct = |x: u64, n: u64| if n == 0 { 0.0 } else { 100.0 * x as f64 / n as f64 };
    let row = |tag: &str, g: &CorrGroup, total: u64| {
        println!(
            "  {tag:<10} gates {:>9} ({:>5.1}% of unique)  solved {:>5.1}%  1-pass {:>5.1}%  no-subset {:>5.1}%  trivial {:>5.1}%  passes/gate {:.3}",
            g.gates,
            pct(g.gates, total),
            pct(g.solved, g.gates),
            pct(g.one_pass, g.gates),
            pct(g.gates - g.subset_fired, g.gates),
            pct(g.trivial, g.gates),
            g.passes as f64 / g.gates.max(1) as f64,
        );
    };

    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);
        let (nb, br) = branch_baseline_corr(1, &spec, lanes, per_lane);
        let total = nb.gates + br.gates;
        println!("== {label} ==  ({} unique gates / baseline calls)", total);
        row("no-branch", &nb, total);
        row("branched", &br, total);
        // The actionable number: if we SKIP baseline on no-branch gates, how many baseline
        // calls do we avoid, and how many would we get WRONG (non-trivial => skip unsafe)?
        let skip_frac = pct(nb.gates, total);
        let unsafe_skips = nb.gates - nb.trivial;
        println!(
            "  -> skipping baseline on no-branch gates avoids {:.1}% of baseline calls; {} ({:.3}%) would be mis-skipped (non-trivial)\n",
            skip_frac,
            unsafe_skips,
            pct(unsafe_skips, total),
        );
    }
}
