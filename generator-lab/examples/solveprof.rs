//! Single-variant difficulty-gate profiling target: build the real gate-board
//! stream (as `solvebench` does), then run EXACTLY ONE engine over it in a tight
//! loop so `perf` attributes cleanly. `solvebench` interleaves three engines and
//! the slow `LogicSolver` swamps the samples; this fixes the engine via argv so
//! `perf record` / `perf stat` see one closure.
//!
//! Build + profile:
//!   cargo build --release -p generator-lab --example solveprof
//!   perf stat -d target/release/examples/solveprof bb
//!   perf stat -d target/release/examples/solveprof fused
//!   perf record -g --call-graph dwarf target/release/examples/solveprof fused
//!   perf report --stdio | head -60
//!
//! Args: solveprof <bb|fused|logic> [--attempts N] [--iters I] [--mode train|drill]

use generator_lab::bb::BitBoard;
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::repr::banded::{Bands, DualBandedMarkGrid, RowMajor};
use generator_lab::repr::{DigitGrid, Marks, SearchState};
use generator_lab::rng::Rng;
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec_for_mode;

type M = Bands<RowMajor>;

fn state_of(b: &Board) -> SearchState<M> {
    let grid = DigitGrid::parse(&b.to_line()).expect("valid line");
    SearchState::<M>::from_digits(&grid)
}

fn dual_of(b: &Board) -> DualBandedMarkGrid {
    let grid = DigitGrid::parse(&b.to_line()).expect("valid line");
    DualBandedMarkGrid::from_digits(&grid)
}

fn main() {
    let mut engine = String::from("fused");
    let mut attempts = 2000usize;
    let mut iters = 60usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "bb" | "fused" | "logic" => engine = a,
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

    let mut acc = 0u64;
    let t = std::time::Instant::now();
    match engine.as_str() {
        "bb" => {
            let bbs: Vec<BitBoard> = boards.iter().map(BitBoard::from_board).collect();
            for _ in 0..iters {
                for bb in &bbs {
                    acc = acc.wrapping_add(bb.baseline(baseline, forced).solved as u64);
                }
            }
        }
        "logic" => {
            let states: Vec<SearchState<M>> = boards.iter().map(state_of).collect();
            for _ in 0..iters {
                for s in &states {
                    acc = acc.wrapping_add(LogicSolver::solve_tracked(s, baseline).solved as u64);
                }
            }
        }
        _ => {
            let duals: Vec<DualBandedMarkGrid> = boards.iter().map(dual_of).collect();
            for _ in 0..iters {
                for d in &duals {
                    acc = acc.wrapping_add(FusedLogicSolver::solve_tracked(d, baseline).solved as u64);
                }
            }
        }
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);
    println!("engine={engine}  {n} boards x {iters} iters   {ns:.1} ns/call");
}
