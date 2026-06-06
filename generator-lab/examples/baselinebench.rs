//! Isolated packed-baseline-solver throughput + profiling target: NOTHING but the W=8
//! SIMT baseline logic solver ([`solve::simt::PackedSolver`]). It harvests a faithful
//! corpus of baseline-gate boards from a real generator run
//! ([`generate::random_simt::collect_baseline_boards`]) once, converts them to row-major
//! SoA queries, then replays the corpus through [`PackedSolver::solve`] in a timed loop
//! — no strip walk, no uniqueness prober, no grid fill in the measured region. So `perf`
//! attributes cleanly to the closure kernel (`warp_pass_full`/`smear_v`) and the scalar
//! subset fallback (`subset_step`).
//!
//! Build + profile:
//!   cargo build --release --features profiling --example baselinebench
//!   perf record -g --call-graph dwarf target/release/examples/baselinebench
//!   perf report --stdio | head -40
//!
//! Usage: cargo run --release --example baselinebench -- [corpus_att=8000] [reps=20] [mode=1]
//! (mode 1 = drill(HiddenQuad), the no-LC closure; mode 0 = train, once LC lands.)

use generator_lab::generate::random_simt::collect_baseline_boards;
use generator_lab::repr::Marks;
use generator_lab::repr::banded::DualSolverState;
use generator_lab::solve::simt::{PackedSolver, SolveQuery};
use generator_lab::solve::{FusedLogicSolver, Solver};
use generator_lab::spec::kinds::SolveTrace;
use generator_lab::spec_for_mode;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_att: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(8000);
    let reps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let mode: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();

    let lanes = 8;
    let per_lane = corpus_att / lanes;
    let boards = collect_baseline_boards(1, &spec, lanes, per_lane);
    let queries: Vec<SolveQuery> = boards.iter().map(SolveQuery::from_digits).collect();
    println!(
        "baselinebench mode={mode}: corpus {} boards (from {} att), {reps} reps",
        queries.len(),
        per_lane * lanes
    );

    let mut solver = PackedSolver::new();
    let mut out = vec![SolveTrace::default(); queries.len()];
    let total = reps as f64 * queries.len() as f64;

    // A/B over where locked candidates run: in the vectorized closure vs. scalar off the
    // warp (a stall step). For drill (no LC) the two are identical.
    for lc_in_closure in [true, false] {
        solver.solve_with(baseline, lc_in_closure, &queries, &mut out); // warmup
        let solved: usize = out.iter().filter(|t| t.solved).count();
        let req_met: usize = out.iter().filter(|t| spec.requirement_met(&t.counts)).count();

        let t = Instant::now();
        for _ in 0..reps {
            solver.solve_with(baseline, lc_in_closure, &queries, &mut out);
        }
        let dt = t.elapsed();
        let ns = dt.as_secs_f64() * 1e9 / total;
        std::hint::black_box(&out);
        let tag = if lc_in_closure { "SIMT LC-in-warp " } else { "SIMT LC-off-warp" };
        println!(
            "  {tag} {:>7.2} ns/board   ({:.3} M/s)   solved {}/{} ({:.1}%)   req-met {}",
            ns,
            1e3 / ns,
            solved,
            queries.len(),
            100.0 * solved as f64 / queries.len() as f64,
            req_met
        );
    }
    // Re-time the default (in-closure) for the speedup line below.
    let t = Instant::now();
    for _ in 0..reps {
        solver.solve(baseline, &queries, &mut out);
    }
    let ns = t.elapsed().as_secs_f64() * 1e9 / total;

    // Scalar baseline (the per-lane FusedLogicSolver this replaces), on the same corpus.
    // Pre-build the dual states once so only the solve is timed, matching the SIMT region.
    let duals: Vec<DualSolverState> = boards.iter().map(DualSolverState::from_digits).collect();
    let mut sscalar = 0usize;
    for d in &duals {
        if FusedLogicSolver::solve_tracked(d, baseline).solved {
            sscalar += 1;
        }
    }
    let t = Instant::now();
    for _ in 0..reps {
        for d in &duals {
            std::hint::black_box(FusedLogicSolver::solve_tracked(d, baseline).solved);
        }
    }
    let dts = t.elapsed();
    let nss = dts.as_secs_f64() * 1e9 / total;
    std::hint::black_box(sscalar);
    println!(
        "  scalar {:>7.2} ns/board   ({:.3} M boards/s)   solved {}",
        nss,
        1e3 / nss,
        sscalar
    );
    println!("  speedup  {:.2}x", nss / ns);
}
