//! Isolated difficulty-gate bench: the old bitboard baseline (`bb::BitBoard::baseline`,
//! the dual-view fused-closure engine) vs the new-representation fused logic solver
//! (`solve::FusedLogicSolver` on the dual-banded packing) on the SAME real query
//! stream — the boards the strip loop actually hands the spec gate.
//!
//! Mirrors `probebench`'s shape for the prober: walk a real strip, snapshot every
//! board at the point the baseline gate runs (post uniqueness gate), then time both
//! engines over the prebuilt stream so construction is out of the measured loop.
//!
//! The composable `LogicSolver` is the correctness oracle (~6x bb), so it is NOT
//! timed here — it would swamp the run; `tests/logic_equiv.rs` cross-checks it (and
//! the fused engine) against both reference engines instead. This bench is purely
//! the fused-vs-bb head-to-head: the band closure cores are on par; the gap is the
//! subset ladder, which the fused path leaves on the composable per-cell `get` path
//! (the `DualBandedMarkGrid` is digit-major, so each `get` is a 9-board SIMD scan).
//!
//! Verdicts are cross-checked first (solved + requirement_met + subset counts MUST
//! agree, the generator's actual contract) so this is a go/no-go, not a guess.
//!
//! Run: cargo run --release -p generator-lab --example solvebench -- [--attempts N=2000] [--iters I=30] [--mode train|drill]

use std::time::Instant;

use generator_lab::bb::BitBoard;
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::repr::banded::DualBandedMarkGrid;
use generator_lab::repr::{DigitGrid, Marks};
use generator_lab::rng::Rng;
use generator_lab::solve::{FusedLogicSolver, Solver};
use generator_lab::spec_for_mode;
use generator_lab::technique_kinds::{NAKED_PAIR, NUM, SolveTrace};

/// Build the dual-banded grid the fused fast path runs on.
fn dual_of(b: &Board) -> DualBandedMarkGrid {
    let grid = DigitGrid::parse(&b.to_line()).expect("valid line");
    DualBandedMarkGrid::from_digits(&grid)
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
    let forced = spec.forced_mask();
    let mut rng = Rng::from_seed(1);

    // ---- collect the real gate-board stream ----
    // At every uniqueness gate that survives `any_alt_solves`, snapshot the board the
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
            let bb = BitBoard::from_board(&puzzle);
            let alts = puzzle.candidates(i) & !digit_to_bit(solution.cell(i));
            if alts != 0 && bb.any_alt_solves(i, alts) {
                puzzle.place(i, orig);
                continue;
            }
            boards.push(puzzle.clone());
            if !bb.baseline(baseline, forced).solved {
                puzzle.place(i, orig);
            }
        }
    }
    let n = boards.len();

    // ---- prebuild both timed representations (out of the timed loop) ----
    let bbs: Vec<BitBoard> = boards.iter().map(BitBoard::from_board).collect();
    let duals: Vec<DualBandedMarkGrid> = boards.iter().map(dual_of).collect();

    // ---- soundness: the fused engine vs bb on the generator's contract ----
    let cmp = |a: &SolveTrace, b: &SolveTrace| -> bool {
        a.solved == b.solved
            && spec.requirement_met(&a.counts) == spec.requirement_met(&b.counts)
            && (NAKED_PAIR..NUM).all(|k| a.counts[k] == b.counts[k])
    };
    let mut mis_fused = 0u64;
    let mut solved = 0u64;
    for q in 0..n {
        let bbo = bbs[q].baseline(baseline, forced);
        mis_fused += !cmp(&bbo, &FusedLogicSolver::solve_tracked(&duals[q], baseline)) as u64;
        solved += bbo.solved as u64;
    }

    let mode_name = if mode == 1 { "drill" } else { "train" };
    println!(
        "solvebench: mode={mode_name}, {n} gate boards  ({:.1}% solved)",
        100.0 * solved as f64 / n as f64
    );
    println!("  verdict mismatches vs bb   fused {mis_fused}   <- MUST be 0\n");

    // ---- timing ----
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for bb in &bbs {
            acc = acc.wrapping_add(bb.baseline(baseline, forced).solved as u64);
        }
    }
    let bb_ns = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;

    let t = Instant::now();
    for _ in 0..iters {
        for d in &duals {
            acc = acc.wrapping_add(FusedLogicSolver::solve_tracked(d, baseline).solved as u64);
        }
    }
    let fused_ns = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    println!("  bb baseline (fused dual-view)    {bb_ns:8.1} ns/call");
    println!("  FusedLogicSolver (new dual-view) {fused_ns:8.1} ns/call   {:.2}x bb", fused_ns / bb_ns);
}
