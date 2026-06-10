//! Hit-rate + soundness tally for the hidden-single re-force fast-path EXPERIMENT
//! (`StripState::reforced`): walk faithful scalar strip attempts and count, at every
//! `alts != 0` gate, how often the stripped cell's original digit is still a hidden
//! single in its row/column/box — each such gate could skip BOTH the uniqueness probe
//! and the baseline solve, like the `alts == 0` fast path. The violation counters
//! cross-check the soundness claims against the real gate verdicts and must be zero.
//!
//! Spec selection is combobench's: --force NAME[:COUNT] (repeatable) with the
//! train-union toolbox, or --toolbox full.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example reforcestat -- \
//!       --force NAME[:COUNT] [--force ...] [--toolbox train|full] \
//!       [--attempts 2000] [--seed 1]

use generator_lab::generate::reforce_stat;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{DIFFICULTY, NAMES, NUM, Tier, branch_of, tier_of};

fn main() {
    let mut forces: Vec<(usize, u16)> = Vec::new();
    let mut toolbox_full = false;
    let mut attempts = 2000usize;
    let mut base_seed = 1u64;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => {
                let spec = it.next().unwrap_or_default();
                let (name, count) = match spec.split_once(':') {
                    Some((n, c)) => (n.to_string(), c.parse().unwrap_or(1u16)),
                    None => (spec, 1u16),
                };
                let idx = NAMES.iter().position(|&n| n == name).unwrap_or_else(|| {
                    eprintln!("unknown kind {name:?}; known: {}", NAMES.join(", "));
                    std::process::exit(2);
                });
                forces.push((idx, count));
            }
            "--toolbox" => toolbox_full = matches!(it.next().as_deref(), Some("full")),
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
            _ => {}
        }
    }

    if forces.is_empty() {
        eprintln!("need at least one --force NAME[:COUNT]");
        std::process::exit(2);
    }

    // Toolbox: full ladder, or the union of each forced target's train-scope
    // (combobench's builder, replicating Spec::train's allow rule per target).
    let mut spec = Spec::explicit();
    if toolbox_full {
        for idx in 0..NUM {
            spec = spec.allow(idx);
        }
    } else {
        let mut allowed = [false; NUM];
        for &(target, _) in &forces {
            let target_tier = tier_of(target);
            let target_branch = branch_of(target);
            let target_diff = DIFFICULTY[target];
            for idx in 0..NUM {
                let tt = tier_of(idx);
                if tt > target_tier {
                    continue;
                }
                let ok = if tt <= Tier::Intermediate {
                    true
                } else {
                    branch_of(idx) == target_branch && DIFFICULTY[idx] <= target_diff
                };
                allowed[idx] |= ok;
            }
        }
        for (idx, &a) in allowed.iter().enumerate() {
            if a {
                spec = spec.allow(idx);
            }
        }
    }
    for &(idx, count) in &forces {
        spec = spec.force(idx, count);
    }

    let label = forces
        .iter()
        .map(|&(idx, c)| if c == 1 { NAMES[idx].to_string() } else { format!("{}x{c}", NAMES[idx]) })
        .collect::<Vec<_>>()
        .join(" + ");
    println!("reforcestat[{label}] toolbox={}: {attempts} attempts, seed {base_seed}", if toolbox_full { "full" } else { "train-union" });

    let t0 = std::time::Instant::now();
    let s = reforce_stat(base_seed, &spec, attempts);
    let dt = t0.elapsed();

    let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
    println!(
        "  per attempt: {:.1} trivial keeps, {:.1} gates ({:.1} accepted / {:.1} nonunique / {:.1} unsolvable)",
        s.trivial as f64 / s.attempts as f64,
        s.gates as f64 / s.attempts as f64,
        s.accepted as f64 / s.attempts as f64,
        s.rejected_nonunique as f64 / s.attempts as f64,
        s.rejected_unsolvable as f64 / s.attempts as f64,
    );
    println!(
        "  hits: {} = {:.1}% of gates, {:.1}% of accepted (row {:.1}% / col {:.1}% / box {:.1}% of gates)",
        s.hits,
        pct(s.hits, s.gates),
        pct(s.hits, s.accepted),
        pct(s.hit_row, s.gates),
        pct(s.hit_col, s.gates),
        pct(s.hit_box, s.gates),
    );
    println!(
        "  violations (must be 0): nonunique {} unsolvable {} req-drift {}",
        s.hit_nonunique, s.hit_unsolvable, s.hit_req_drift,
    );
    println!("  walk: {:.2}s ({:.1} us/att scalar)", dt.as_secs_f64(), dt.as_secs_f64() * 1e6 / s.attempts as f64);
}
