// Seed fidelity: is the hidden triple that ends up FORCED in a constructed
// puzzle actually the one the constructor seeded — or a different triple that
// emerged during stripping?
//
// For each constructed puzzle we know the seeded geometry (unit + subset
// cells). We walk the avoid-target path to its first bottleneck (the state
// where every in-scope non-target step is exhausted and only a hidden triple
// can progress) and inspect which hidden triple(s) are forced there.
//
// Usage: cargo run --release --example seed_fidelity -- [--n N=300]

use std::collections::BTreeSet;

use sudoku_core::{
    Board, HiddenSubsetConstructor, HouseRef, Rng, SeedGeometry, Spec, Step, TechniqueKind,
    all_techniques_filtered, apply_step, next_step_filtered, random_full_grid,
};
use sudoku_core::board::UnitKind;

struct Cfg {
    n: usize,
    narrow: bool,
}

fn parse_cfg() -> Cfg {
    let mut cfg = Cfg { n: 300, narrow: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--n" => cfg.n = it.next().and_then(|s| s.parse().ok()).unwrap_or(300),
            "--narrow" => cfg.narrow = true,
            _ => {}
        }
    }
    cfg
}

/// Walk avoiding the target until no in-scope non-target step remains. Returns
/// that bottleneck board iff the target is applicable there (the "forced"
/// state). None if the walk solves or dead-ends without the target.
fn first_bottleneck(board: &Board, spec: &Spec, target: TechniqueKind) -> Option<Board> {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return None;
        }
        if let Some(s) = next_step_filtered(&b, |t| spec.is_in_scope(t) && t != target) {
            apply_step(&mut b, &s);
        } else if next_step_filtered(&b, |t| t == target).is_some() {
            return Some(b);
        } else {
            return None;
        }
    }
}

fn house_matches(h: Option<&HouseRef>, g: &SeedGeometry) -> bool {
    match h {
        Some(h) => h.kind == g.unit_kind && h.index as usize == g.unit_index,
        None => false,
    }
}

fn cells_match(step: &Step, g: &SeedGeometry) -> bool {
    let a: BTreeSet<usize> = step.focus_cells.iter().copied().collect();
    let b: BTreeSet<usize> = g.subset_cells.iter().copied().collect();
    a == b
}

fn kind_str(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Row => "row",
        UnitKind::Col => "col",
        UnitKind::Box => "box",
    }
}

fn main() {
    let cfg = parse_cfg();
    let n = cfg.n;
    let target = TechniqueKind::HiddenTriple;
    let spec = if cfg.narrow {
        Spec::allow_up_to(TechniqueKind::HiddenSingle).require(target, 1)
    } else {
        Spec::train(target)
    };
    println!("spec: {}", if cfg.narrow { "narrow (singles + hidden-triple)" } else { "train (broad, up to hidden-triple)" });
    let ctor = HiddenSubsetConstructor::triple(spec.clone());

    let mut seed_present_on_board = 0; // seeded triple detectable on the raw constructed board
    let mut seed_exact = 0; // forced state hosts the seeded triple, same cells
    let mut seed_unit_diff = 0; // forced state hosts a triple in the seeded unit, different cells
    let mut other_only = 0; // forced triples all in other units
    let mut no_bottleneck = 0; // avoid-walk never hit a forced-triple state (shouldn't happen)
    let mut sole_forced_unit = 0; // exactly one unit hosts a forced triple at the bottleneck
    let mut subset_cells_survive = 0; // all seeded subset cells still empty at the bottleneck

    let attempt_budget: u64 = 200_000;
    let mut seed: u64 = 1;
    let mut built = 0;
    let mut attempts = 0u64;
    while built < n && attempts < attempt_budget {
        let mut rng = Rng::from_seed(seed);
        seed += 1;
        attempts += 1;
        let solution = random_full_grid(&mut rng);
        let Some((board, g)) = ctor.try_extend_traced(&solution, &mut rng) else {
            continue;
        };
        built += 1;

        // Is the seeded triple even present on the constructed board?
        let on_board = all_techniques_filtered(&board, |t| t == target)
            .iter()
            .any(|s| house_matches(s.focus_house.as_ref(), &g));
        if on_board {
            seed_present_on_board += 1;
        }

        let Some(bn) = first_bottleneck(&board, &spec, target) else {
            no_bottleneck += 1;
            continue;
        };

        let forced: Vec<Step> = all_techniques_filtered(&bn, |t| t == target);
        let units: BTreeSet<(UnitKind, u8)> = forced
            .iter()
            .filter_map(|s| s.focus_house.as_ref().map(|h| (h.kind, h.index)))
            .collect();
        if units.len() == 1 {
            sole_forced_unit += 1;
        }
        if g.subset_cells.iter().all(|&c| bn.is_empty(c)) {
            subset_cells_survive += 1;
        }

        let in_seed_unit: Vec<&Step> = forced
            .iter()
            .filter(|s| house_matches(s.focus_house.as_ref(), &g))
            .collect();
        if in_seed_unit.iter().any(|s| cells_match(s, &g)) {
            seed_exact += 1;
        } else if !in_seed_unit.is_empty() {
            seed_unit_diff += 1;
        } else {
            other_only += 1;
        }
    }

    let pc = |k: usize| 100.0 * k as f64 / built as f64;
    println!("seed fidelity over {} constructed hidden-triple puzzles", built);
    println!("  build cost: {} attempts ({:.1} attempts/puzzle)", attempts, attempts as f64 / built as f64);
    println!();
    println!("  seeded triple present on constructed board : {:>4} ({:>5.1}%)", seed_present_on_board, pc(seed_present_on_board));
    println!("  seeded subset cells survive to bottleneck  : {:>4} ({:>5.1}%)", subset_cells_survive, pc(subset_cells_survive));
    println!();
    println!("  forced state vs seed:");
    println!("    seeded triple is the forced one (exact)  : {:>4} ({:>5.1}%)", seed_exact, pc(seed_exact));
    println!("    forced in seeded unit, different cells   : {:>4} ({:>5.1}%)", seed_unit_diff, pc(seed_unit_diff));
    println!("    forced only in other unit(s)             : {:>4} ({:>5.1}%)", other_only, pc(other_only));
    println!("    no forced-triple bottleneck found        : {:>4} ({:>5.1}%)", no_bottleneck, pc(no_bottleneck));
    println!();
    println!("  bottleneck forced by a single unit         : {:>4} ({:>5.1}%)", sole_forced_unit, pc(sole_forced_unit));

    if built == 0 {
        println!("(0 built within {} attempts — gate too tight)", attempt_budget);
        return;
    }

    // One worked example, for the eyeball test.
    let mut rng = Rng::from_seed(424242);
    for _ in 0..200_000 {
        let solution = random_full_grid(&mut rng);
        if let Some((board, g)) = ctor.try_extend_traced(&solution, &mut rng) {
            println!();
            println!("example: seeded {} {} cells {:?} q={}", kind_str(g.unit_kind), g.unit_index + 1, g.subset_cells, g.q);
            if let Some(bn) = first_bottleneck(&board, &spec, target) {
                for s in all_techniques_filtered(&bn, |t| t == target) {
                    let h = s.focus_house.as_ref().map(|h| format!("{} {}", kind_str(h.kind), h.index + 1)).unwrap_or_default();
                    println!("  forced triple @ {} cells {:?}", h, s.focus_cells);
                }
            }
            break;
        }
    }
}
