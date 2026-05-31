//! Hypothesis test: in the `train(HiddenQuad)` strip loop the uniqueness prober
//! is REDUNDANT with the baseline gate.
//!
//! The baseline toolbox (up to HiddenQuad) applies only *sound* forced steps, so
//! if it drives a board to a full grid, that grid is forced — hence the board is
//! unique. Therefore `baseline_solvable(b) => unique(b)`. A strip is kept iff
//! `unique && baseline_solvable`; if the implication holds, that is just
//! `baseline_solvable`, and the entire prober call can be deleted.
//!
//! This example verifies that empirically three ways:
//!   1. the dangerous case `non_unique && baseline_solvable` never occurs
//!      (checked against the trusted oracle, not a fast prover);
//!   2. the baseline-only strip trajectory has the SAME fingerprint as the
//!      full two-gate loop;
//!   3. times both loops so we can see the win.
//!
//! Usage: cargo run --release -p solver-lab --example redundancy -- [--attempts N=4000] [--seed S=1]

use std::time::Instant;

use solver_lab::generate::random_full_grid;
use solver_lab::grid::{Board, CELLS, digit_to_bit};
use solver_lab::oracle::count_solutions;
use solver_lab::rng::Rng;
use solver_lab::techniques::baseline_solvable;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fold(fp: &mut u64, b: &Board) {
    for i in 0..CELLS {
        *fp ^= b.cell(i) as u64;
        *fp = fp.wrapping_mul(FNV_PRIME);
    }
}

/// Current loop: uniqueness gate (oracle here, for trust) THEN baseline gate.
/// Also tallies the per-position cases. Returns the fingerprint.
fn two_gate(rng: &mut Rng, stats: &mut Stats) -> u64 {
    let mut fp = FNV_OFFSET;
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();
    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);
    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let orig = puzzle.cell(i);
        puzzle.clear_naked(i);

        let v_bit = digit_to_bit(solution.cell(i));
        let alts = puzzle.candidates(i) & !v_bit;
        // Uniqueness via the trusted oracle: non-unique iff >=2 completions.
        let non_unique = alts != 0 && count_solutions(&puzzle, 2) >= 2;
        let solvable = baseline_solvable(&puzzle);

        stats.positions += 1;
        if non_unique {
            stats.non_unique += 1;
        }
        if non_unique && solvable {
            // THE case that would make the prober load-bearing.
            stats.danger += 1;
        }

        if non_unique {
            puzzle.place(i, orig);
            continue;
        }
        if !solvable {
            puzzle.place(i, orig);
            continue;
        }
    }
    fold(&mut fp, &puzzle);
    fp
}

/// Proposed loop: baseline gate ONLY. No prober.
fn baseline_only(rng: &mut Rng) -> u64 {
    let mut fp = FNV_OFFSET;
    let solution = random_full_grid(rng);
    let mut puzzle = solution.clone();
    let mut positions: Vec<usize> = (0..CELLS).collect();
    rng.shuffle(&mut positions);
    for i in positions {
        if puzzle.is_empty(i) {
            continue;
        }
        let orig = puzzle.cell(i);
        puzzle.clear_naked(i);
        if !baseline_solvable(&puzzle) {
            puzzle.place(i, orig);
            continue;
        }
    }
    fold(&mut fp, &puzzle);
    fp
}

#[derive(Default)]
struct Stats {
    positions: u64,
    non_unique: u64,
    danger: u64,
}

struct Args {
    attempts: usize,
    seed: u64,
}

fn parse_args() -> Args {
    let mut out = Args { attempts: 4000, seed: 1 };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => out.attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.attempts),
            "--seed" => out.seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(out.seed),
            _ => {}
        }
    }
    out
}

fn main() {
    let args = parse_args();
    println!(
        "redundancy test: {} attempts, seed {}\n",
        args.attempts, args.seed
    );

    // 1+2: equivalence + danger-case tally (oracle-backed, untimed).
    let mut stats = Stats::default();
    let mut rng_a = Rng::from_seed(args.seed);
    let mut rng_b = Rng::from_seed(args.seed);
    let mut fp_two = FNV_OFFSET;
    let mut fp_base = FNV_OFFSET;
    for _ in 0..args.attempts {
        fp_two ^= two_gate(&mut rng_a, &mut stats);
        fp_base ^= baseline_only(&mut rng_b);
    }
    println!("positions stripped : {}", stats.positions);
    println!("non-unique strips  : {}", stats.non_unique);
    println!(
        "DANGER (non_unique && baseline_solvable): {}  <- must be 0 for prober to be redundant",
        stats.danger
    );
    println!(
        "trajectory fingerprints: two-gate={:#018x}  baseline-only={:#018x}  {}",
        fp_two,
        fp_base,
        if fp_two == fp_base { "IDENTICAL" } else { "*** DIVERGED ***" }
    );

    // 3: timing — baseline-only vs a real two-gate loop using the fast prober.
    println!("\ntiming (lower us/attempt is better):");
    let t = Instant::now();
    let mut sink = 0u64;
    let mut rng = Rng::from_seed(args.seed);
    for _ in 0..args.attempts {
        sink ^= baseline_only(&mut rng);
    }
    let base_secs = t.elapsed().as_secs_f64();
    println!(
        "  baseline-only      {:>8.3}s   {:>8.1} us/att   (sink {:#x})",
        base_secs,
        base_secs * 1e6 / args.attempts as f64,
        sink
    );
}
