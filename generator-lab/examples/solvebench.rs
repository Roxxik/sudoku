//! Isolated difficulty-gate bench: the new-representation fused logic solver
//! (`solve::FusedLogicSolver` on the dual-banded packing) vs the composable
//! `LogicSolver` oracle, on the SAME real query stream — the boards the strip loop
//! actually hands the spec gate.
//!
//! Mirrors `probebench`'s shape for the prober: walk a real strip, snapshot every
//! board at the point the baseline gate runs (post uniqueness gate), then time the
//! engine over the prebuilt stream so construction is out of the measured loop.
//!
//! The composable `LogicSolver` is the correctness oracle (~6x slower), so it is the
//! cross-check here, not a timed competitor; `tests/logic_equiv.rs` cross-checks it
//! (and the fused engine) against the reference engines too. This bench reports the
//! fused engine's ns/call: the band closure cores are fast; the remaining gap is the
//! subset ladder, which the fused path leaves on the composable per-cell `get` path
//! (the `DualBandedMarkGrid` is digit-major, so each `get` is a 9-board SIMD scan).
//!
//! Verdicts are cross-checked first (solved + requirement_met + subset counts MUST
//! agree, the generator's actual contract) so this is a go/no-go, not a guess.
//!
//! Run: cargo run --release -p generator-lab --example solvebench -- [--attempts N=2000] [--iters I=30] [--mode train|drill]

use std::time::Instant;

use generator_lab::fill::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::{Bands, DualBandedMarkGrid, RowMajor};
use generator_lab::repr::{DigitGrid, Marks, SearchState};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec_for_mode;
use generator_lab::technique_kinds::{NAKED_PAIR, NUM, SolveTrace};

/// The banded packing the uniqueness prober branches on.
type RM = Bands<RowMajor>;
/// The uniqueness prober: scan/sieve `Search` with the `Bivalue` branch strategy.
type P = Search<Bivalue>;

/// Build the dual-banded grid the fused fast path runs on.
fn dual_of(b: &Board) -> DualBandedMarkGrid {
    let grid = DigitGrid::parse(&b.to_line()).expect("valid line");
    DualBandedMarkGrid::from_digits(&grid)
}

/// True iff stripping the clue at `cell` (whose true digit is `orig`) lost uniqueness:
/// forbid `orig` to restrict the cell to its alternates and ask whether some other
/// digit still completes (the new-stack twin of bb's old alt-completion existence probe).
fn alt_solves(b: &Board, cell: usize, orig: generator_lab::grid::Digit) -> bool {
    let grid = DigitGrid::parse(&b.to_line()).expect("valid line");
    let mut probe = SearchState::<RM>::from_digits(&grid);
    let d = generator_lab::repr::Digit::new(orig).expect("nonzero clue digit");
    probe.forbid(cell, d);
    P::has_completion(probe)
}

fn main() {
    let mut attempts = 2000usize;
    let mut iters = 30usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }

    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let mut rng = Rng::from_seed(1);

    // ---- collect the real gate-board stream ----
    // At every uniqueness gate that survives the prober, snapshot the board the
    // baseline gate runs on, then advance the strip exactly as the generator would
    // (revert a strip the baseline can't solve) so the stream is the genuine workload.
    let mut boards: Vec<Board> = Vec::new();
    for _ in 0..attempts {
        let solution = random_full_grid(&mut rng);
        let mut puzzle = solution.clone();
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        for i in positions {
            if puzzle.is_empty(i) {
                continue;
            }
            let orig = puzzle.cell(i);
            puzzle.clear_naked(i);
            let alts = puzzle.candidates(i) & !digit_to_bit(solution.cell(i));
            if alts != 0 && alt_solves(&puzzle, i, orig) {
                puzzle.place(i, orig);
                continue;
            }
            boards.push(puzzle.clone());
            if !FusedLogicSolver::solve_tracked(&dual_of(&puzzle), baseline).solved {
                puzzle.place(i, orig);
            }
        }
    }
    let n = boards.len();

    // ---- prebuild the timed representation (out of the timed loop) ----
    let duals: Vec<DualBandedMarkGrid> = boards.iter().map(dual_of).collect();

    // ---- soundness: the fused engine vs the composable oracle on the generator's
    // contract (solved + requirement_met + subset counts). ----
    let cmp = |a: &SolveTrace, b: &SolveTrace| -> bool {
        a.solved == b.solved
            && spec.requirement_met(&a.counts) == spec.requirement_met(&b.counts)
            && (NAKED_PAIR..NUM).all(|k| a.counts[k] == b.counts[k])
    };
    let mut mis_fused = 0u64;
    let mut solved = 0u64;
    for q in 0..n {
        let oracle = LogicSolver::solve_tracked(&duals[q], baseline);
        mis_fused += !cmp(&oracle, &FusedLogicSolver::solve_tracked(&duals[q], baseline)) as u64;
        solved += oracle.solved as u64;
    }

    let mode_name = if mode == 1 { "drill" } else { "train" };
    println!(
        "solvebench: mode={mode_name}, {n} gate boards  ({:.1}% solved)",
        100.0 * solved as f64 / n as f64
    );
    println!("  verdict mismatches vs oracle   fused {mis_fused}   <- MUST be 0\n");

    // ---- timing ----
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for d in &duals {
            acc = acc.wrapping_add(FusedLogicSolver::solve_tracked(d, baseline).solved as u64);
        }
    }
    let fused_ns = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    println!("  FusedLogicSolver (new dual-view) {fused_ns:8.1} ns/call");
}
