//! Interleaved A/B of the Engine factoring: variant A = the merged host
//! ([`random_simt_merged::PuzzleStream`], slot lifecycle hardcoded in `tick`),
//! variant B = the factored host ([`warp_host::GateStream`], same lifecycle behind
//! the monomorphized [`Engine`] seam). Same lane coroutines, same kernels, same
//! seeds, alternating order-swapped chunks in one binary; the per-seed puzzle
//! comparison is the soundness guard, the time ratio the verdict (expected: exact
//! parity — the dispatch is static).
//!
//! Spec selection is combobench's: --force NAME[:COUNT] (repeatable), --toolbox
//! train|full. No --force defaults to train(HiddenQuad).
//!
//! Usage:
//!   cargo run --release -p generator-lab --example jobfactorab -- \
//!       [--force NAME[:COUNT] ...] [--toolbox train|full] \
//!       [--per-variant 40000] [--chunk 2000] [--seed 1]

use generator_lab::generate::random_simt::Pumped;
use generator_lab::generate::random_simt_merged::PuzzleStream as MergedStream;
use generator_lab::generate::warp_host::GateStream;
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{DIFFICULTY, HIDDEN_QUAD, NAMES, NUM, Tier, branch_of, tier_of};

/// Pump the merged stream until its cumulative attempt counter reaches `target`,
/// recording each found puzzle's `(seed, fp)`; returns the wall time.
fn run_chunk_merged<I: Iterator<Item = u64>>(
    s: &mut MergedStream<I>,
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

/// Same, for the factored stream.
fn run_chunk_factored<I: Iterator<Item = u64>>(
    s: &mut GateStream<I>,
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
            "--per-variant" => {
                per_variant = it.next().and_then(|s| s.parse().ok()).unwrap_or(per_variant)
            }
            "--chunk" => chunk = it.next().and_then(|s| s.parse().ok()).unwrap_or(chunk),
            "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
            _ => {}
        }
    }

    // --only a|b: run ONE variant for the full budget and exit — the single-variant
    // mode `perf stat` needs for clean instruction-count attribution (counts are
    // work-proportional and thermal-immune, unlike the interleaved wall-clock).
    let mut only: Option<u8> = None;
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--only") {
        only = match args.get(i + 1).map(String::as_str) {
            Some("a") => Some(0),
            Some("b") => Some(1),
            _ => None,
        };
    }

    if forces.is_empty() {
        forces.push((HIDDEN_QUAD, 1)); // default: the standard Subset bench target
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
        "jobfactorab[{label}] toolbox={}: {per_variant}/variant in {chunk}-attempt chunks, seed {base_seed}",
        if toolbox_full { "full" } else { "train-union" },
    );

    if let Some(which) = only {
        let mut found = Vec::new();
        let dt = if which == 0 {
            let mut s = MergedStream::new(base_seed.., &spec);
            run_chunk_merged(&mut s, per_variant, &mut found)
        } else {
            let mut s = GateStream::new(base_seed.., &spec);
            run_chunk_factored(&mut s, per_variant, &mut found)
        };
        println!(
            "  only {}: {:.2} us/att   found {}",
            if which == 0 { "A (merged)" } else { "B (factored)" },
            dt * 1e6 / per_variant as f64,
            found.len(),
        );
        return;
    }

    let mut a = MergedStream::new(base_seed.., &spec); // A: merged (hardcoded tick)
    let mut b = GateStream::new(base_seed.., &spec); // B: factored (Engine seam)
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
                let dt = run_chunk_merged(&mut a, target, &mut found_a);
                (dt, before, a.stats().attempts)
            } else {
                let before = b.stats().attempts;
                let dt = run_chunk_factored(&mut b, target, &mut found_b);
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

    // Soundness guard: both hosts walk identical gate sequences, so the
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
    println!("  A (merged):   {us_a:>7.2} us/att   yield {} / {}", sa.successes, sa.attempts);
    println!("  B (factored): {us_b:>7.2} us/att   yield {} / {}", sb.successes, sb.attempts);
    println!(
        "  B/A: {:.4}  ({:+.2}%)   puzzles compared on {compared} common seeds: {}",
        us_b / us_a,
        100.0 * (us_b / us_a - 1.0),
        if mismatched == 0 { "IDENTICAL" } else { "MISMATCHED  <-- SOUNDNESS VIOLATION" },
    );
}
