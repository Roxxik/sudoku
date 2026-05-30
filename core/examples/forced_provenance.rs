// Forced-instance provenance: is "the forced technique" a single, stable thing,
// or does it shift the moment you perturb the puzzle?
//
// This is the property the old HiddenSubsetConstructor needed and didn't have:
// it seeded a specific subset, but a *different* instance ended up forced. Here
// we measure the property directly on organically-generated forced puzzles
// (sourced via the modern make_puzzle_for_spec + Spec::train path, not the dead
// constructor), so we can decide between:
//
//   - provenance / instance-locked stripping (only viable if the forced
//     instance is stable under local moves), and
//   - reverse construction (build the stuck state from scratch — the only road
//     if the forced instance migrates freely).
//
// Two sections:
//   1. RARITY  — for a rare target (default hidden-quad), how many random-
//      restart attempts to find one forced puzzle under Spec::train? Find a few,
//      extrapolate. (Hidden-quad is the hardest case; this just sizes it.)
//   2. STUDY   — for a findable target (default hidden-triple): at the forced
//      bottleneck, how many distinct instances are available (multiplicity),
//      and when we strip one more given keeping the forcing constraint, is the
//      forced instance set the same / overlapping / disjoint (stability)?
//
// Usage:
//   cargo run --release --example forced_provenance -- \
//     [--study-tech hidden-triple] [--study-n 200] \
//     [--rare-tech hidden-quad] [--rare-n 3] [--rare-cap 500000] \
//     [--drill] [--skip-rare] [--skip-study]

use std::collections::BTreeSet;
use std::time::Instant;

use sudoku_core::board::UnitKind;
use sudoku_core::search::apply_mutation;
use sudoku_core::uniqueness::count_solutions;
use sudoku_core::{
    Board, Mutation, Probe, Rng, Spec, Step, TechniqueKind, all_techniques_filtered, apply_step,
    make_puzzle_for_spec_with_search, next_step_filtered, verify,
};

struct Args {
    study_tech: TechniqueKind,
    study_n: usize,
    rare_tech: TechniqueKind,
    rare_n: usize,
    rare_cap: usize,
    drill: bool,
    skip_rare: bool,
    skip_study: bool,
}

fn parse_tech(s: &str) -> TechniqueKind {
    sudoku_core::REGISTRY
        .iter()
        .find(|d| d.cli_name == s)
        .map(|d| d.kind)
        .unwrap_or_else(|| {
            eprintln!("unknown technique: {s}");
            std::process::exit(2);
        })
}

fn parse_args() -> Args {
    let mut a = Args {
        study_tech: TechniqueKind::HiddenTriple,
        study_n: 200,
        rare_tech: TechniqueKind::HiddenQuad,
        rare_n: 3,
        rare_cap: 500_000,
        drill: false,
        skip_rare: false,
        skip_study: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--study-tech" => a.study_tech = parse_tech(&it.next().unwrap()),
            "--study-n" => a.study_n = it.next().and_then(|s| s.parse().ok()).unwrap_or(200),
            "--rare-tech" => a.rare_tech = parse_tech(&it.next().unwrap()),
            "--rare-n" => a.rare_n = it.next().and_then(|s| s.parse().ok()).unwrap_or(3),
            "--rare-cap" => a.rare_cap = it.next().and_then(|s| s.parse().ok()).unwrap_or(500_000),
            "--drill" => a.drill = true,
            "--skip-rare" => a.skip_rare = true,
            "--skip-study" => a.skip_study = true,
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

fn spec_for(t: TechniqueKind, drill: bool) -> Spec {
    if drill { Spec::drill(t) } else { Spec::train(t) }
}

/// Identity of a subset instance: its house plus its sorted focus cells. Two
/// instances are "the same" iff these match. (Digits are determined by the
/// house+cells for our purposes.)
type InstanceKey = (UnitKind, u8, Vec<usize>);

fn instance_key(s: &Step) -> Option<InstanceKey> {
    let h = s.focus_house.as_ref()?;
    let mut cells = s.focus_cells.clone();
    cells.sort_unstable();
    Some((h.kind, h.index, cells.to_vec()))
}

/// The set of target instances forced at the puzzle's first bottleneck: walk
/// avoiding the target using in-scope techniques; at the first state where no
/// in-scope non-target step remains but the target is applicable, collect every
/// target instance available there. Empty if the avoid-walk solves or dead-ends
/// without the target (i.e. the target is not forced from this puzzle).
fn forced_instances(board: &Board, spec: &Spec, target: TechniqueKind) -> BTreeSet<InstanceKey> {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return BTreeSet::new();
        }
        if let Some(s) = next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            apply_step(&mut b, &s);
            continue;
        }
        // Stuck without target. Collect the target instances available here.
        let mut set = BTreeSet::new();
        for s in all_techniques_filtered(&b, |t| t == target) {
            if let Some(k) = instance_key(&s) {
                set.insert(k);
            }
        }
        return set;
    }
}

fn run_rarity(a: &Args) {
    let spec = spec_for(a.rare_tech, a.drill);
    let mode = if a.drill { "drill" } else { "train" };
    println!("=== RARITY: {} ({}) ===", a.rare_tech.cli_name(), mode);
    println!(
        "finding {} forced puzzle(s) via random-restart make_puzzle_for_spec, cap {} attempts each",
        a.rare_n, a.rare_cap
    );

    let mut rng = Rng::from_seed(1);
    let mut attempts_each: Vec<usize> = Vec::new();
    let t0 = Instant::now();
    for i in 0..a.rare_n {
        let (found, stats) = make_puzzle_for_spec_with_search(
            &mut rng,
            &spec,
            a.rare_cap,
            0,
            None,
            Probe::default(),
        );
        match found {
            Some(_) => {
                println!("  #{}: found after {} attempts", i + 1, stats.attempts);
                attempts_each.push(stats.attempts);
            }
            None => {
                println!(
                    "  #{}: NOT FOUND within {} attempts (elapsed {:.1}s) — stopping rarity probe",
                    i + 1,
                    a.rare_cap,
                    t0.elapsed().as_secs_f64()
                );
                break;
            }
        }
    }
    if !attempts_each.is_empty() {
        let total: usize = attempts_each.iter().sum();
        let mean = total as f64 / attempts_each.len() as f64;
        println!(
            "  mean attempts/forced-puzzle: {:.0}  (elapsed {:.1}s, {:.0} attempts/s)",
            mean,
            t0.elapsed().as_secs_f64(),
            total as f64 / t0.elapsed().as_secs_f64(),
        );
        println!(
            "  extrapolation: ~{:.0} attempts to build a batch of 100",
            mean * 100.0
        );
    }
    println!();
}

fn run_study(a: &Args) {
    let spec = spec_for(a.study_tech, a.drill);
    let target = a.study_tech;
    let mode = if a.drill { "drill" } else { "train" };
    println!("=== STUDY: {} ({}) ===", target.cli_name(), mode);
    println!("sourcing {} forced puzzles, then measuring multiplicity + stability", a.study_n);

    let mut rng = Rng::from_seed(1);

    // Multiplicity histogram: distinct forced instances at the bottleneck.
    let mut mult_hist: Vec<usize> = vec![0; 8];
    // Stability tallies, summed over all forced neighbors of all puzzles.
    let mut neigh_total = 0usize; // forced neighbors examined
    let mut neigh_equal = 0usize; // same forced-instance set as the parent
    let mut neigh_overlap = 0usize; // shares >=1 instance, not equal
    let mut neigh_disjoint = 0usize; // no shared instance — migrated
    // Per-puzzle: did ANY forced neighbor keep the exact same instance set?
    let mut puzzles_with_stable_neighbor = 0usize;
    let mut puzzles_with_any_forced_neighbor = 0usize;

    let mut built = 0usize;
    let t0 = Instant::now();
    let cap = 200_000usize;
    while built < a.study_n {
        let (found, _stats) =
            make_puzzle_for_spec_with_search(&mut rng, &spec, cap, 0, None, Probe::default());
        let Some(fr) = found else {
            println!("  (could not source another puzzle within {cap} attempts — stopping at {built})");
            break;
        };
        let puzzle = fr.puzzle.puzzle.clone();
        let solution = fr.puzzle.solution.clone();
        built += 1;

        let parent_set = forced_instances(&puzzle, &spec, target);
        let m = parent_set.len().min(mult_hist.len() - 1);
        mult_hist[m] += 1;

        // Perturb: strip one more given (re-pinning uniqueness), keep neighbors
        // that still verify (still forced-target). Classify each vs the parent.
        let givens: Vec<usize> = (0..81).filter(|&i| !puzzle.is_empty(i)).collect();
        let mut any_forced = false;
        let mut any_stable = false;
        for g in givens {
            let neighbor = apply_mutation(&puzzle, &solution, Mutation::StripRepin(g));
            if count_solutions(&neighbor, 2) != 1 {
                continue;
            }
            if verify(&neighbor, &spec).is_err() {
                continue; // no longer forced-target — not a "keeps the constraint" neighbor
            }
            any_forced = true;
            neigh_total += 1;
            let nset = forced_instances(&neighbor, &spec, target);
            let shares = parent_set.intersection(&nset).next().is_some();
            if nset == parent_set {
                neigh_equal += 1;
                any_stable = true;
            } else if shares {
                neigh_overlap += 1;
            } else {
                neigh_disjoint += 1;
            }
        }
        if any_forced {
            puzzles_with_any_forced_neighbor += 1;
        }
        if any_stable {
            puzzles_with_stable_neighbor += 1;
        }
    }

    if built == 0 {
        println!("  (built 0 puzzles)");
        return;
    }
    println!("  built {} puzzles in {:.1}s", built, t0.elapsed().as_secs_f64());
    println!();
    println!("  multiplicity at bottleneck (distinct forced instances):");
    for (k, &c) in mult_hist.iter().enumerate() {
        if c > 0 {
            let label = if k == mult_hist.len() - 1 {
                format!("{}+", k)
            } else {
                k.to_string()
            };
            println!("    {:>2} instance(s): {:>4} ({:>5.1}%)", label, c, 100.0 * c as f64 / built as f64);
        }
    }
    println!();
    println!("  stability under strip-one-more (keeping the forcing constraint):");
    println!("    puzzles with >=1 forced neighbor   : {:>4} ({:>5.1}%)", puzzles_with_any_forced_neighbor, 100.0 * puzzles_with_any_forced_neighbor as f64 / built as f64);
    if puzzles_with_any_forced_neighbor > 0 {
        println!("    ...of those, >=1 SAME-instance-set : {:>4} ({:>5.1}%)", puzzles_with_stable_neighbor, 100.0 * puzzles_with_stable_neighbor as f64 / puzzles_with_any_forced_neighbor as f64);
    }
    if neigh_total > 0 {
        let pc = |x: usize| 100.0 * x as f64 / neigh_total as f64;
        println!("    forced neighbors examined          : {}", neigh_total);
        println!("      same instance set (stable)       : {:>5.1}%", pc(neigh_equal));
        println!("      overlapping (shares >=1)         : {:>5.1}%", pc(neigh_overlap));
        println!("      disjoint (fully migrated)        : {:>5.1}%", pc(neigh_disjoint));
    }
    println!();
}

fn main() {
    let a = parse_args();
    if !a.skip_rare {
        run_rarity(&a);
    }
    if !a.skip_study {
        run_study(&a);
    }
}
