//! Sweep COMBINED-requirement specs through the unified W=8 SIMT warp to find a
//! generator codepath worth optimizing: one that is both slow per attempt AND
//! rare (many attempts per puzzle), so the warp's average time to produce one
//! puzzle is large (> 1s is the threshold of interest).
//!
//! A single `--target` (the `simtbench`/`findpar` interface) only ever Forces one
//! kind. This example builds an EXPLICIT spec that Forces several kinds at once
//! (and any kind with count > 1), e.g. "require a W-Wing AND a naked triple" or
//! "require two X-Wings and a jellyfish". Combining requirements keeps the
//! per-attempt solver cost high (the baseline ladder can't fast-path a Forced
//! kind) while making the puzzle much rarer, which is exactly the slow+rare
//! codepath we want to surface.
//!
//! Work is FIXED (`run_warp_unified`, lanes x per_lane attempts) so a truly-rare combo
//! is capped instead of running forever; us/att is yield-independent so it measures
//! cleanly even at zero yield, and the yield (successes/attempts) gives the
//! attempts/puzzle needed to project the average s/puzzle.
//!
//! Usage:
//!   cargo run --release -p generator-lab --example combobench -- \
//!       --force NAME[:COUNT] [--force NAME[:COUNT] ...] \
//!       [--toolbox train|full] [--lanes 8] [--per-lane 100000] [--seed 1]
//!
//! NAME is any kind from `spec::kinds::NAMES` (e.g. `w-wing`, `naked-triple`,
//! `jellyfish`). COUNT defaults to 1. `--toolbox train` (default) allows the
//! union of each forced target's train-scope (Trunk + simpler-or-equal peers in
//! that target's branch); `--toolbox full` allows the entire 16-kind ladder
//! (more substitutes => rarer).

use generator_lab::generate::random_simt::{Pumped, PuzzleStream};
use generator_lab::spec::Spec;
use generator_lab::spec::kinds::{DIFFICULTY, NAMES, NUM, Tier, branch_of, tier_of};

fn main() {
    let mut forces: Vec<(usize, u16)> = Vec::new();
    let mut toolbox_full = false;
    let mut lanes = 8usize;
    let mut per_lane = 100_000usize;
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
            "--lanes" => lanes = it.next().and_then(|s| s.parse().ok()).unwrap_or(lanes),
            "--per-lane" => per_lane = it.next().and_then(|s| s.parse().ok()).unwrap_or(per_lane),
            "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
            _ => {}
        }
    }

    if forces.is_empty() {
        eprintln!("need at least one --force NAME[:COUNT]");
        std::process::exit(2);
    }

    // Build the allowed toolbox: either the full ladder, or the union of each
    // forced target's train-scope (Trunk up to the target's tier + simpler-or-
    // equal same-branch peers). Replicates Spec::train's allow rule per target.
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
    // Force the requested kinds (overrides Allowed for those slots).
    for &(idx, count) in &forces {
        spec = spec.force(idx, count);
    }

    let label = forces
        .iter()
        .map(|&(idx, c)| if c == 1 { NAMES[idx].to_string() } else { format!("{}x{c}", NAMES[idx]) })
        .collect::<Vec<_>>()
        .join(" + ");
    let toolbox = if toolbox_full { "full" } else { "train-union" };
    let total = lanes * per_lane;
    println!("combobench[{label}] toolbox={toolbox}: {lanes} lanes x {per_lane} = {total} attempts");

    #[cfg(feature = "count")]
    {
        generator_lab::repr::banded::psg_reset();
        generator_lab::solve::uwstat_reset();
    }
    // Fixed work, capped from the OUTSIDE: pump the production stream until `total`
    // attempts have started (the counter climbs on every attempt incl. retries, so this
    // terminates even for a combo that never yields). Fold an order-independent fingerprint
    // over the produced puzzles so two builds match iff they produce the same puzzle set.
    let t0 = std::time::Instant::now();
    let mut stream = PuzzleStream::new(base_seed.., &spec);
    let mut combo_fp = 0u64; // XOR-fold of per-puzzle fps: order-independent
    // Pump ONE tick at a time so the outside cap overshoots `total` by at most a tick's
    // worth of attempts; a tick (a full 8-wide warp pass) dwarfs the per-call cost.
    while stream.stats().attempts < total {
        match stream.pump(1) {
            Pumped::Found(_, p) => combo_fp ^= generator_lab::fingerprint::grid_fp(&p.puzzle.0),
            Pumped::StepCountReached => {}
            Pumped::NoMorePuzzles => break, // unbounded seeds: unreachable
        }
    }
    let dt = t0.elapsed();
    let stats = stream.stats();
    #[cfg(feature = "count")]
    {
        let h = generator_lab::repr::banded::psg_snapshot();
        let total: u64 = h.iter().sum();
        let pct: Vec<String> =
            h.iter().map(|&c| format!("{:.1}%", 100.0 * c as f64 / total.max(1) as f64)).collect();
        println!("  place_single_group group-size histogram (cells): {h:?}");
        println!("    share: {}", pct.join(" "));
        // Honest unified-warp lane utilization for THIS workload: avg active lanes per
        // pass / LANES (active = slot bound to live work). uw = [passes, active-lane-sum].
        let uw = generator_lab::solve::uwstat_snapshot();
        let lanes_const = generator_lab::probe::simt::LANES;
        let util = uw[1] as f64 / (lanes_const as f64 * uw[0].max(1) as f64);
        println!(
            "  warp LANES={lanes_const}  passes {}  util {:.3} ({:.2}/{lanes_const} lanes)  passes/att {:.2}",
            uw[0],
            util,
            uw[1] as f64 / uw[0].max(1) as f64,
            uw[0] as f64 / (lanes * per_lane) as f64,
        );
    }

    // Two builds match iff they produce the same set of puzzles.
    println!("  fp: {combo_fp:#018x}");

    let s = &stats;
    let us_per_att = dt.as_secs_f64() * 1e6 / s.attempts.max(1) as f64;
    let att_per_puz = s.attempts as f64 / s.successes.max(1) as f64;
    // Average wall time for the W=8 warp to produce one puzzle.
    let s_per_puzzle = us_per_att * att_per_puz / 1e6;

    println!(
        "  {us_per_att:>7.2} us/att   yield {} / {} ({:.1} att/puzzle){}",
        s.successes,
        s.attempts,
        att_per_puz,
        if s.successes == 0 { "  [NONE in budget]" } else { "" },
    );
    if s.successes == 0 {
        println!("  s/puzzle: > {:.3}s  (no puzzle in {total} attempts; lower bound)", us_per_att * total as f64 / 1e6);
    } else {
        println!(
            "  s/puzzle: {s_per_puzzle:.3}s  (avg, W=8 warp){}",
            if s_per_puzzle > 1.0 { "  <-- > 1s target" } else { "" },
        );
    }
}
