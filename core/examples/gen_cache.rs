// Generate forced puzzles and append them to the cache. This is a pure
// cache-filler: random-restart and/or the essence constructor, for a chosen
// target + mode (train/drill). All analysis lives in the separate
// `analyze_cache` example, which reads the cache back — so an expensive search
// (drill-quad is ~133s/puzzle via random) is paid once and reused forever.
//
// Usage:
//   cargo run --release --example gen_cache -- \
//     [--target hidden-quad] [--mode train|drill] \
//     [--method random|constructor|both|empty-unit|u9-seed] [--n 10] \
//     [--time-budget 120] [--seed 1] [--cache-path core/cache/forced.jsonl]
//
// Notes:
//   - random has NO per-call attempt cap: each call runs until it finds one
//     puzzle, so stats.attempts is that puzzle's true attempts-to-find. The
//     wall-clock budget is checked between finds.
//   - The constructor only supports hidden-triple / hidden-quad, and is a
//     TRAIN-only tool (it conflates in-scope with baseline; under drill it
//     produces ~0 forced and emits boards that fail verify). gen_cache verifies
//     every constructor output and only caches/ counts the ones that pass.

use std::time::Instant;

use sudoku_core::board::{BOX_UNITS, COL_UNITS, PEERS, ROW_UNITS, UnitKind};
use sudoku_core::uniqueness::count_solutions;
use sudoku_core::{
    Board, HiddenSubsetConstructor, REGISTRY, Rng, Spec, TechniqueKind, apply_step, construct_with,
    make_puzzle_for_spec_with_stats, next_step_filtered, random_full_grid, solve_filtered, verify,
};

struct Args {
    target: TechniqueKind,
    mode: String, // "train" | "drill"
    method: String, // "random" | "constructor" | "both" | "empty-unit" | "u9-seed"
    n: usize,
    time_budget: f64,
    seed: u64,
    cache_path: String,
    /// u9-seed unit-kind selection weights (row, col, box). Default uniform.
    /// To match a target house distribution T given measured per-kind yields y,
    /// set w_k = T_k / y_k (output kind freq ∝ w_k·y_k ∝ T_k).
    kind_weights: [f64; 3],
}

fn parse_tech(s: &str) -> TechniqueKind {
    REGISTRY
        .iter()
        .find(|d| d.cli_name == s)
        .map(|d| d.kind)
        .unwrap_or_else(|| {
            eprintln!("unknown technique: {s}");
            std::process::exit(2);
        })
}

fn parse_args() -> Args {
    let mut out = Args {
        target: TechniqueKind::HiddenQuad,
        mode: "train".to_string(),
        method: "both".to_string(),
        n: 10,
        time_budget: 120.0,
        seed: 1,
        cache_path: "core/cache/forced.jsonl".to_string(),
        kind_weights: [1.0, 1.0, 1.0],
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => out.target = parse_tech(&it.next().unwrap_or_default()),
            "--mode" => out.mode = it.next().unwrap_or(out.mode),
            "--method" => out.method = it.next().unwrap_or(out.method),
            "--n" => out.n = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.n),
            "--time-budget" => out.time_budget = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.time_budget),
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            "--cache-path" => out.cache_path = it.next().unwrap_or(out.cache_path),
            "--kind-weights" => {
                if let Some(s) = it.next() {
                    let p: Vec<f64> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if p.len() == 3 {
                        out.kind_weights = [p[0], p[1], p[2]];
                    } else {
                        eprintln!("--kind-weights wants 3 comma-separated numbers: row,col,box");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    out
}

fn spec_for(target: TechniqueKind, mode: &str) -> Spec {
    match mode {
        "drill" => Spec::drill(target),
        "train" => Spec::train(target),
        other => {
            eprintln!("unknown mode: {other} (use train|drill)");
            std::process::exit(2);
        }
    }
}

/// Minimal append-only cache of found forced puzzles. One JSONL record per
/// puzzle: the 81-char puzzle + solution lines fully reconstruct the puzzle and
/// its state, plus how long it took to find. Append-only by design — re-runs
/// accumulate more samples (may contain duplicates; dedup on the puzzle line if
/// that matters to a consumer).
struct Recorder {
    out: Option<std::io::BufWriter<std::fs::File>>,
}

impl Recorder {
    fn new(path: &str) -> Self {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => Self { out: Some(std::io::BufWriter::new(f)) },
            Err(e) => {
                eprintln!("warning: could not open cache {path}: {e} (continuing without cache)");
                Self { out: None }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(&mut self, method: &str, mode: &str, target: &str, puzzle: &Board, solution: &Board, givens: usize, attempts: usize, ms: f64) {
        use std::io::Write;
        if let Some(w) = self.out.as_mut() {
            // Hand-rolled JSON: all fields are numbers or [0-9.]-only strings,
            // so no escaping needed.
            let _ = writeln!(
                w,
                "{{\"method\":\"{}\",\"mode\":\"{}\",\"target\":\"{}\",\"puzzle\":\"{}\",\"solution\":\"{}\",\"givens\":{},\"attempts\":{},\"ms\":{:.1}}}",
                method, mode, target, puzzle.to_line(), solution.to_line(), givens, attempts, ms,
            );
        }
    }

    fn flush(&mut self) {
        use std::io::Write;
        if let Some(w) = self.out.as_mut() {
            let _ = w.flush();
        }
    }
}

/// Strip a constructor output down to a verify-minimal puzzle: clear givens in
/// random order, keeping any clear that preserves uniqueness AND keeps the spec
/// satisfied (target still forced). Mirrors what the random generator does
/// implicitly, so the two populations agree on given-count.
fn minimize_forced(board: &Board, spec: &Spec, rng: &mut Rng) -> Board {
    let mut b = board.clone();
    let mut positions: Vec<usize> = (0..81).filter(|&i| !b.is_empty(i)).collect();
    rng.shuffle(&mut positions);
    for i in positions {
        if b.is_empty(i) {
            continue;
        }
        let mut cand = b.clone();
        cand.clear(i);
        if count_solutions(&cand, 2) != 1 {
            continue;
        }
        if verify(&cand, spec).is_ok() {
            b = cand;
        }
    }
    b
}

/// Random-restart generation, NO per-call attempt cap. For a memoryless random
/// generator a new attempt and a new seed are identical, so the cap bought
/// nothing but a place to check the time budget — and it muddied the accounting.
/// Here each call runs until it finds one puzzle (max_attempts set absurdly high),
/// so `stats.attempts` IS the attempts-to-find for that puzzle, and total attempts
/// / found is the honest mean (failed strips inside a call are already counted by
/// stats.attempts). The wall-clock budget is checked between finds; worst-case
/// overshoot is one find.
fn gen_random(spec: &Spec, target: TechniqueKind, n: usize, base_seed: u64, time_budget_s: f64, rec: &mut Recorder, mode: &str) -> (usize, usize, f64) {
    const NO_CAP: usize = usize::MAX;
    let mut found = 0;
    let mut attempts = 0;
    let t0 = Instant::now();
    let mut seed = base_seed;
    while found < n {
        if t0.elapsed().as_secs_f64() > time_budget_s {
            break;
        }
        let mut rng = Rng::from_seed(seed);
        seed += 1;
        let before_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let (out, stats) = make_puzzle_for_spec_with_stats(&mut rng, spec, NO_CAP);
        attempts += stats.attempts;
        if let Some(fr) = out {
            debug_assert!(verify(&fr.puzzle.puzzle, spec).is_ok());
            let now_ms = t0.elapsed().as_secs_f64() * 1000.0;
            // This call found exactly one puzzle: stats.attempts and the elapsed
            // delta ARE this puzzle's cost (including its own failed strips).
            rec.record("random", mode, target.cli_name(), &fr.puzzle.puzzle, &fr.puzzle.solution, fr.puzzle.givens, stats.attempts, now_ms - before_ms);
            found += 1;
            progress("random", found, n, stats.attempts, now_ms - before_ms, attempts, t0.elapsed().as_secs_f64());
        }
    }
    (found, attempts, t0.elapsed().as_secs_f64())
}

/// Essence-seeded constructor generation (hidden-triple / hidden-quad only).
/// Each output is minimized then verified; only verify-passing puzzles are
/// cached/counted (rejected ones are reported — they indicate the constructor's
/// in-scope-vs-baseline bug, which surfaces under drill).
fn gen_construct(spec: &Spec, size: usize, target: TechniqueKind, n: usize, base_seed: u64, time_budget_s: f64, rec: &mut Recorder, mode: &str) -> (usize, usize, f64) {
    let ctor = if size == 3 {
        HiddenSubsetConstructor::triple(spec.clone())
    } else {
        HiddenSubsetConstructor::quad(spec.clone())
    };
    let mut found = 0;
    let mut attempts = 0;
    let mut last_attempts = 0;
    let mut last_ms = 0.0;
    let mut rejected = 0;
    let t0 = Instant::now();
    let mut seed = base_seed;
    while found < n {
        if t0.elapsed().as_secs_f64() > time_budget_s {
            break;
        }
        let mut rng = Rng::from_seed(seed);
        seed += 1;
        match construct_with(&ctor, &mut rng, 5_000) {
            Some((board, a)) => {
                attempts += a;
                let board = minimize_forced(&board, spec, &mut rng);
                if verify(&board, spec).is_ok() {
                    // Recover the full solution by solving the minimized puzzle.
                    let solution = solve_filtered(board.clone(), |_| true).board;
                    let givens = (0..81).filter(|&i| !board.is_empty(i)).count();
                    let now_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    let this_attempts = attempts - last_attempts;
                    let this_ms = now_ms - last_ms;
                    rec.record("constructor", mode, target.cli_name(), &board, &solution, givens, this_attempts, this_ms);
                    last_attempts = attempts;
                    last_ms = now_ms;
                    found += 1;
                    progress("constructor", found, n, this_attempts, this_ms, attempts, t0.elapsed().as_secs_f64());
                } else {
                    rejected += 1;
                }
            }
            None => attempts += 5_000,
        }
    }
    if rejected > 0 {
        println!("  (constructor produced {} boards that FAILED verify - in-scope-vs-baseline bug)", rejected);
    }
    (found, attempts, t0.elapsed().as_secs_f64())
}

fn unit_cells(kind: UnitKind, index: usize) -> [usize; 9] {
    match kind {
        UnitKind::Row => ROW_UNITS[index],
        UnitKind::Col => COL_UNITS[index],
        UnitKind::Box => BOX_UNITS[index],
    }
}

/// True if the spec's baseline (Allowed | Forced) alone solves the board.
fn baseline_solves(board: &Board, spec: &Spec) -> bool {
    let mut b = board.clone();
    loop {
        if b.is_solved() {
            return true;
        }
        match next_step_filtered(&b, |t| spec.is_baseline(t)) {
            None => return false,
            Some(s) => apply_step(&mut b, &s),
        }
    }
}

/// Empty-unit-anchored constructor (exploits the duality theorem: a forced
/// hidden quad REQUIRES a fully-empty unit). Each attempt: take a full solution,
/// clear ONE entire unit (guaranteeing the u=9 prerequisite random puzzles
/// almost never have), then strip the remaining givens keeping uniqueness AND
/// baseline-solvability — exactly the random generator's strip, but anchored on
/// the structural prerequisite. Accept iff `verify` confirms the quad is forced.
///
/// Pre-emptying the unit is NECESSARY (u=9) but not sufficient (the unit must
/// stay empty until the stall, and host a hidden-quad structure) — so this is
/// still a search; the bet is that anchoring on u=9 lifts the hit rate far above
/// blind random's ~157k attempts/puzzle.
fn gen_empty_unit(spec: &Spec, target: TechniqueKind, n: usize, base_seed: u64, time_budget_s: f64, rec: &mut Recorder, mode: &str) -> (usize, usize, f64) {
    let units: Vec<(UnitKind, usize)> = (0..9)
        .map(|i| (UnitKind::Row, i))
        .chain((0..9).map(|i| (UnitKind::Col, i)))
        .chain((0..9).map(|i| (UnitKind::Box, i)))
        .collect();
    let mut found = 0;
    let mut attempts = 0;
    let mut last_attempts = 0;
    let mut last_ms = 0.0;
    let mut bad_uniqueness = 0; // clearing the unit left >1 completion
    let t0 = Instant::now();
    let mut seed = base_seed;
    while found < n {
        if t0.elapsed().as_secs_f64() > time_budget_s {
            break;
        }
        let mut rng = Rng::from_seed(seed);
        seed += 1;
        attempts += 1;
        let sol = random_full_grid(&mut rng);
        let (kind, idx) = units[rng.range(units.len())];
        let mut board = sol.clone();
        for &c in unit_cells(kind, idx).iter() {
            board.clear(c);
        }
        if count_solutions(&board, 2) != 1 {
            bad_uniqueness += 1;
            continue; // the cleared unit has multiple completions
        }
        // Strip the remaining (outside-unit) givens, keeping uniqueness AND
        // baseline-solvability — the unit stays empty (its cells are already).
        let mut positions: Vec<usize> = (0..81).filter(|&i| !board.is_empty(i)).collect();
        rng.shuffle(&mut positions);
        for p in positions {
            let mut cand = board.clone();
            cand.clear(p);
            if count_solutions(&cand, 2) != 1 {
                continue;
            }
            if !baseline_solves(&cand, spec) {
                continue;
            }
            board = cand;
        }
        if verify(&board, spec).is_ok() {
            let solution = solve_filtered(board.clone(), |_| true).board;
            let givens = (0..81).filter(|&i| !board.is_empty(i)).count();
            let now_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let this_attempts = attempts - last_attempts;
            let this_ms = now_ms - last_ms;
            rec.record("empty-unit", mode, target.cli_name(), &board, &solution, givens, this_attempts, this_ms);
            last_attempts = attempts;
            last_ms = now_ms;
            found += 1;
            progress("empty-unit", found, n, this_attempts, this_ms, attempts, t0.elapsed().as_secs_f64());
        }
    }
    if bad_uniqueness > 0 {
        println!("  (empty-unit: {} attempts discarded - cleared unit had >1 completion)", bad_uniqueness);
    }
    (found, attempts, t0.elapsed().as_secs_f64())
}

/// u=9 seeded constructor. Like empty-unit, but it also GUARANTEES a hidden-quad
/// structure inside the emptied unit, fixing the old constructor's fatal flaw
/// (it seeded into a u<=5 unit, which the duality theorem shows can never force a
/// quad). Per attempt: pick a unit U and a 4-of-9 partition C (quad cells) vs the
/// 5 complement cells; pin given peers OUTSIDE U that confine the 4 quad-digits D
/// to C (eliminate D from every complement cell); clear all of U; strip the
/// non-pinned outside givens keeping uniqueness + baseline-solvability. The pins
/// preserve confinement through stripping, so the hidden quad is structurally
/// present by construction — only its *forced-ness* is left to chance.
fn gen_u9_seed(spec: &Spec, target: TechniqueKind, n: usize, base_seed: u64, time_budget_s: f64, kind_weights: [f64; 3], rec: &mut Recorder, mode: &str) -> (usize, usize, f64) {
    // Weighted unit-kind selection: pick a kind by weight, then uniform within it.
    // Default [1,1,1] reproduces uniform-over-27. Set w_k = target_k / yield_k to
    // rebalance the (skewed) output distribution toward a target.
    let total_w = kind_weights.iter().sum::<f64>();
    let pick_unit = |rng: &mut Rng| -> (UnitKind, usize) {
        let u = (rng.range(1_000_000) as f64) / 1_000_000.0 * total_w;
        let kind = if u < kind_weights[0] {
            UnitKind::Row
        } else if u < kind_weights[0] + kind_weights[1] {
            UnitKind::Col
        } else {
            UnitKind::Box
        };
        (kind, rng.range(9))
    };
    let mut found = 0;
    let mut attempts = 0;
    let mut last_attempts = 0;
    let mut last_ms = 0.0;
    let mut unconfinable = 0; // partition had no outside pin for some (comp cell, digit)
    // Per-unit-kind attempt/success counts (Row=0, Col=1, Box=2) to diagnose the
    // house skew: is box-forced just a low-yield seed, not a measurement artifact?
    let mut kind_attempts = [0usize; 3];
    let mut kind_success = [0usize; 3];
    let kind_idx = |k: UnitKind| match k {
        UnitKind::Row => 0,
        UnitKind::Col => 1,
        UnitKind::Box => 2,
    };
    let t0 = Instant::now();
    let mut seed = base_seed;
    while found < n {
        if t0.elapsed().as_secs_f64() > time_budget_s {
            break;
        }
        let mut rng = Rng::from_seed(seed);
        seed += 1;
        attempts += 1;
        let sol = random_full_grid(&mut rng);
        let (kind, idx) = pick_unit(&mut rng);
        kind_attempts[kind_idx(kind)] += 1;
        let ucells = unit_cells(kind, idx);

        // Partition the unit's 9 cells: 4 quad cells C, 5 complement cells.
        let mut order: Vec<usize> = ucells.to_vec();
        rng.shuffle(&mut order);
        let c_cells = &order[0..4];
        let comp_cells = &order[4..9];
        let d: Vec<u8> = c_cells.iter().map(|&c| sol.cell(c)).collect();

        // Pin given peers outside U that confine D to C: for each complement cell
        // e and each quad-digit dk, keep a peer of e (outside U) that holds dk in
        // the solution, so dk is eliminated from e.
        let mut pins: Vec<usize> = Vec::new();
        let mut ok = true;
        'outer: for &e in comp_cells {
            for &dk in &d {
                let mut chosen: Option<usize> = None;
                for &p in &PEERS[e] {
                    if ucells.contains(&p) || sol.cell(p) != dk {
                        continue;
                    }
                    if pins.contains(&p) {
                        chosen = Some(p); // reuse an existing pin
                        break;
                    }
                    if chosen.is_none() {
                        chosen = Some(p);
                    }
                }
                match chosen {
                    Some(p) => {
                        if !pins.contains(&p) {
                            pins.push(p);
                        }
                    }
                    None => {
                        ok = false;
                        break 'outer;
                    }
                }
            }
        }
        if !ok {
            unconfinable += 1;
            continue;
        }

        // Clear the whole unit; strip non-pinned outside givens.
        let mut board = sol.clone();
        for &c in ucells.iter() {
            board.clear(c);
        }
        let mut strippable: Vec<usize> = (0..81)
            .filter(|&i| !board.is_empty(i) && !pins.contains(&i))
            .collect();
        rng.shuffle(&mut strippable);
        for p in strippable {
            let mut cand = board.clone();
            cand.clear(p);
            if count_solutions(&cand, 2) != 1 {
                continue;
            }
            if !baseline_solves(&cand, spec) {
                continue;
            }
            board = cand;
        }
        if verify(&board, spec).is_ok() {
            // Minimize toward the random distribution: strip ANY remaining given
            // (pins included) as long as the quad stays forced. Without this the
            // kept confinement pins bloat the given count (~28 vs random's ~24).
            let board = minimize_forced(&board, spec, &mut rng);
            let solution = solve_filtered(board.clone(), |_| true).board;
            let givens = (0..81).filter(|&i| !board.is_empty(i)).count();
            let now_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let this_attempts = attempts - last_attempts;
            let this_ms = now_ms - last_ms;
            rec.record("u9-seed", mode, target.cli_name(), &board, &solution, givens, this_attempts, this_ms);
            last_attempts = attempts;
            last_ms = now_ms;
            kind_success[kind_idx(kind)] += 1;
            found += 1;
            progress("u9-seed", found, n, this_attempts, this_ms, attempts, t0.elapsed().as_secs_f64());
        }
    }
    if unconfinable > 0 {
        println!("  (u9-seed: {} attempts had an unconfinable partition)", unconfinable);
    }
    println!(
        "  (u9-seed yield by seeded unit-kind: row {}/{}  col {}/{}  box {}/{})",
        kind_success[0], kind_attempts[0], kind_success[1], kind_attempts[1], kind_success[2], kind_attempts[2],
    );
    (found, attempts, t0.elapsed().as_secs_f64())
}

/// Running per-puzzle progress: printed each time a generator finds one, so a
/// long run shows live cost (this puzzle's attempts/ms) plus the running mean.
fn progress(label: &str, found: usize, wanted: usize, this_attempts: usize, this_ms: f64, total_attempts: usize, total_secs: f64) {
    println!(
        "    {:<12} [{}/{}] this: {:>9} attempts, {:>8.1} ms  |  avg: {:>9.1} attempts/puzzle, {:>8.1} ms/puzzle",
        label, found, wanted, this_attempts, this_ms, total_attempts as f64 / found as f64, total_secs * 1000.0 / found as f64,
    );
}

fn report(label: &str, found: usize, wanted: usize, attempts: usize, secs: f64) {
    if found == 0 {
        println!("  {:<14} 0/{} found in {:>7.2}s  ({} attempts spent, none reached forced)", label, wanted, secs, attempts);
        return;
    }
    println!(
        "  {:<14} {}/{} found in {:>7.2}s  ({:>9.1} attempts/puzzle, {:>8.1} ms/puzzle)",
        label, found, wanted, secs, attempts as f64 / found as f64, secs * 1000.0 / found as f64,
    );
}

fn main() {
    let args = parse_args();
    let spec = spec_for(args.target, &args.mode);
    let mut rec = Recorder::new(&args.cache_path);
    println!(
        "gen_cache: target={} mode={} method={} n={} seed={} (budget {:.0}s/phase) -> {}",
        args.target.cli_name(), args.mode, args.method, args.n, args.seed, args.time_budget, args.cache_path,
    );

    let do_random = args.method == "random" || args.method == "both";
    let do_construct = args.method == "constructor" || args.method == "both";
    let do_empty_unit = args.method == "empty-unit";
    let do_u9_seed = args.method == "u9-seed";

    if do_empty_unit {
        let (f, a, s) = gen_empty_unit(&spec, args.target, args.n, args.seed, args.time_budget, &mut rec, &args.mode);
        report("empty-unit", f, args.n, a, s);
    }
    if do_u9_seed {
        let (f, a, s) = gen_u9_seed(&spec, args.target, args.n, args.seed, args.time_budget, args.kind_weights, &mut rec, &args.mode);
        report("u9-seed", f, args.n, a, s);
    }
    if do_random {
        let (f, a, s) = gen_random(&spec, args.target, args.n, args.seed, args.time_budget, &mut rec, &args.mode);
        report("random", f, args.n, a, s);
    }
    if do_construct {
        let size = match args.target {
            TechniqueKind::HiddenTriple => 3,
            TechniqueKind::HiddenQuad => 4,
            _ => 0,
        };
        if size == 0 {
            println!("  (constructor unsupported for {} - skipping)", args.target.cli_name());
        } else {
            let (f, a, s) = gen_construct(&spec, size, args.target, args.n, args.seed, args.time_budget, &mut rec, &args.mode);
            report("constructor", f, args.n, a, s);
        }
    }

    rec.flush();
}
