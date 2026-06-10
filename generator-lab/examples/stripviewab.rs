//! Interleaved A/B of the lane strip-state representation on the production SIMT
//! stream: variant A = dual-banded [`DualSolverState`] (the old lane state, column view
//! maintained on every clue clear/restore), variant B = single row-major view
//! ([`RowStrip`], the production candidate — the warp consumes only row bands, so the
//! column-view maintenance is pure waste there). Both variants run all production fast
//! paths. Same seeds, alternating order-swapped chunks in one binary (the drift-proof
//! protocol); both instantiations run the identical clue-survival algebra, so the
//! per-seed puzzle comparison printed at the end is the soundness guard, the time ratio
//! the verdict.
//!
//! Spec selection is combobench's: --force NAME[:COUNT] (repeatable), --toolbox full.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example stripviewab -- \
//!       --force NAME[:COUNT] [--force ...] [--toolbox train|full] \
//!       [--per-variant 40000] [--chunk 2000] [--seed 1]

use generator_lab::generate::StripView;
use generator_lab::generate::random_simt::{Pumped, PuzzleStream, RowStrip};
use generator_lab::repr::banded::DualSolverState;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{DIFFICULTY, NAMES, NUM, Tier, branch_of, tier_of};

/// Pump `s` until its cumulative attempt counter reaches `target`, recording each
/// found puzzle's `(seed, fp)`; returns the wall time spent in this chunk.
fn run_chunk<I: Iterator<Item = u64>, S: StripView>(
    s: &mut PuzzleStream<I, S>,
    target: usize,
    found: &mut Vec<(u64, u64)>,
) -> f64 {
    let t0 = std::time::Instant::now();
    while s.stats().attempts < target {
        match s.pump(1) {
            Pumped::Found(seed, p) => {
                found.push((seed, generator_lab::fingerprint::grid_fp(&p.puzzle.0)));
            }
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break,
        }
    }
    t0.elapsed().as_secs_f64()
}

fn main() {
    let mut forces: Vec<(usize, u16)> = Vec::new();
    let mut toolbox_full = false;
    let mut per_variant = 40_000usize;
    let mut chunk = 2_000usize;
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
            "--per-variant" => per_variant = it.next().and_then(|s| s.parse().ok()).unwrap_or(per_variant),
            "--chunk" => chunk = it.next().and_then(|s| s.parse().ok()).unwrap_or(chunk),
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
    println!(
        "stripviewab[{label}] toolbox={}: {per_variant}/variant in {chunk}-attempt chunks, seed {base_seed}",
        if toolbox_full { "full" } else { "train-union" },
    );

    let mut a = PuzzleStream::<_, DualSolverState>::new_in(base_seed.., &spec, true, true); // A: dual
    let mut b = PuzzleStream::<_, RowStrip>::new_in(base_seed.., &spec, true, true); // B: row-only
    let (mut found_a, mut found_b) = (Vec::new(), Vec::new());
    let (mut t_a, mut t_b) = (0.0f64, 0.0f64);
    let (mut att_a, mut att_b) = (0usize, 0usize);

    let chunks = per_variant.div_ceil(chunk);
    for c in 0..chunks {
        let target = ((c + 1) * chunk).min(per_variant);
        // Alternate order pairwise (A,B then B,A) so slow drift cancels; chunk 0 of
        // each variant is the warmup and is excluded from the timed totals.
        let measure = c > 0;
        let mut run = |which: u8| {
            let (dt, before, after) = if which == 0 {
                let before = a.stats().attempts;
                let dt = run_chunk(&mut a, target, &mut found_a);
                (dt, before, a.stats().attempts)
            } else {
                let before = b.stats().attempts;
                let dt = run_chunk(&mut b, target, &mut found_b);
                (dt, before, b.stats().attempts)
            };
            if measure {
                if which == 0 {
                    t_a += dt;
                    att_a += after - before;
                } else {
                    t_b += dt;
                    att_b += after - before;
                }
            }
        };
        if c % 2 == 0 {
            run(0);
            run(1);
        } else {
            run(1);
            run(0);
        }
    }

    // Soundness guard: both instantiations make identical gate decisions, so the
    // seed -> puzzle map must be IDENTICAL. The two variants overshoot the attempt
    // budget on different tick boundaries, so compare over the seeds BOTH completed.
    let map_a: std::collections::HashMap<u64, u64> = found_a.iter().copied().collect();
    let mut compared = 0usize;
    let mut mismatched = 0usize;
    for &(seed, fp) in &found_b {
        if let Some(&fa) = map_a.get(&seed) {
            compared += 1;
            mismatched += (fa != fp) as usize;
        }
    }

    let sa = a.stats();
    let sb = b.stats();
    let us_a = t_a * 1e6 / att_a.max(1) as f64;
    let us_b = t_b * 1e6 / att_b.max(1) as f64;
    println!("  A (dual): {us_a:>7.2} us/att   yield {} / {}", sa.successes, sa.attempts);
    println!("  B (row):  {us_b:>7.2} us/att   yield {} / {}", sb.successes, sb.attempts);
    println!(
        "  B/A: {:.4}  ({:+.2}%)   puzzles compared on {compared} common seeds: {}",
        us_b / us_a,
        100.0 * (us_b / us_a - 1.0),
        if mismatched == 0 { "IDENTICAL" } else { "MISMATCHED  <-- SOUNDNESS VIOLATION" },
    );
}
