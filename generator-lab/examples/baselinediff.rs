//! Diagnostic: find boards where the SIMT baseline solver disagrees with the scalar
//! FusedLogicSolver, and dump them (with the composable LogicSolver as oracle) for
//! debugging the closure/LC.
//!
//! Usage: cargo run --release --example baselinediff -- [corpus_att=16000] [mode=0] [max_show=10]

use generator_lab::generate::random_simt::collect_baseline_boards;
use generator_lab::repr::Marks;
use generator_lab::repr::banded::DualSolverState;
use generator_lab::solve::simt::{PackedSolver, SolveQuery};
use generator_lab::solve::{FusedLogicSolver, LogicSolver, Solver};
use generator_lab::spec::kinds::{NAMES, NUM};
use generator_lab::spec::kinds::SolveTrace;
use generator_lab::subset_spec_for_mode;

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_att: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16000);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_show: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);

    let spec = subset_spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let boards = collect_baseline_boards(1, &spec, 8, corpus_att / 8);
    let queries: Vec<SolveQuery> = boards.iter().map(SolveQuery::from_digits).collect();
    let mut out = vec![SolveTrace::default(); queries.len()];
    PackedSolver::new().solve(baseline, &queries, &mut out);

    let mut solved_mismatch = 0usize;
    let mut count_mismatch = 0usize;
    let mut shown = 0usize;
    let mut simt_solved = 0usize;
    let mut fused_solved = 0usize;
    for (i, g) in boards.iter().enumerate() {
        let dual = DualSolverState::from_digits(g);
        let reference = FusedLogicSolver::solve_tracked(&dual, baseline);
        let mine = out[i];
        if mine.solved {
            simt_solved += 1;
        }
        if reference.solved {
            fused_solved += 1;
        }
        let solved_bad = mine.solved != reference.solved;
        let counts_bad = (0..NUM).any(|k| mine.counts[k] != reference.counts[k] && k >= 4);
        if solved_bad {
            solved_mismatch += 1;
        }
        if counts_bad {
            count_mismatch += 1;
        }
        if (solved_bad || counts_bad) && shown < max_show {
            shown += 1;
            let composable = LogicSolver::solve_tracked(&dual, baseline);
            println!("--- board {i}: {} ---", g.to_line());
            println!(
                "  SIMT  solved={} counts={:?}",
                mine.solved,
                kinds(&mine.counts)
            );
            println!(
                "  fused solved={} counts={:?}",
                reference.solved,
                kinds(&reference.counts)
            );
            println!(
                "  logic solved={} counts={:?}",
                composable.solved,
                kinds(&composable.counts)
            );
        }
    }
    println!(
        "\ntotal boards {}  solved-mismatch {}  subset-count-mismatch {}",
        boards.len(),
        solved_mismatch,
        count_mismatch
    );
    println!("aggregate: SIMT solved {simt_solved}  fused solved {fused_solved}");
}

fn kinds(counts: &[u16; NUM]) -> Vec<(&'static str, u16)> {
    (0..NUM).filter(|&k| counts[k] > 0).map(|k| (NAMES[k], counts[k])).collect()
}
