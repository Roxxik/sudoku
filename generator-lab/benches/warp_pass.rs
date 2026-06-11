//! Criterion microbench for the SIMT cheap-closure kernel `warp_pass_full` (the warp's
//! per-pass baseline gate; see [`generator_lab::solve::simt`]).
//!
//! Run:    cargo bench --features bench --bench warp_pass
//! A/B:    cargo bench --features bench --bench warp_pass -- --save-baseline before
//!         <edit the kernel>
//!         cargo bench --features bench --bench warp_pass -- --baseline before
//!
//! Two things make criterion the right tool here, over a hand-rolled best-of-N loop:
//!
//!  - `warp_pass_full` mutates the warp **in place** and drives it toward solved, so a
//!    naive `b.iter(|| pass())` would measure the no-op steady state after the board
//!    converges. `iter_batched` hands each *timed* call a fresh copy of the loaded warp
//!    (the copy is setup, not timed) — see [`WarpInput`]'s `Copy`/`Clone`.
//!  - `--save-baseline`/`--baseline` give a statistically-checked before/after, which is
//!    what a kernel A/B in this lab actually wants.
//!
//! Input: partial grids sampled across a real strip's working clue band — **not** minimal
//! puzzles. The warp's baseline gate spends its passes on the many non-minimal boards a
//! strip visits, so we strip a full solution in random order and keep the boards whose
//! clue count lands in [`CLUE_MIN`, `CLUE_MAX`], not the sparse minimal endpoint. The
//! kernel is branchless vector ALU over all 9 digits x 3 bands, so per-pass cost is
//! nearly input-independent; the pool's job is to be a representative *density*, which
//! the printed clue spread makes visible.

use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use generator_lab::bench_seam::WarpInput;
use generator_lab::fill::random_solution;
use generator_lab::repr::{CELLS, DigitGrid};
use generator_lab::rng::Rng;

const SEED: u64 = 0xC0FFEE; // fixed: the board pool is identical run to run
const WARPS: usize = 8; // 8 warps x 8 lanes = 64 boards (a bit more than one warp)
const CLUE_MIN: usize = 24; // floor: below this we approach the sparse minimal endpoint
const CLUE_MAX: usize = 54; // ceiling: above this the board is trivially single-solved

/// Sample `want` partial boards across real strips' working clue band. Removing cells
/// from a full solution in random order reproduces the board-density distribution the
/// warp's baseline gate sees mid-strip; keeping only boards in [`CLUE_MIN`, `CLUE_MAX`]
/// holds the pool in the non-minimal working region. (These are density-representative
/// partial boards, not necessarily uniquely-solvable puzzles — irrelevant to a singles
/// throughput microbench.)
fn strip_pool(want: usize) -> Vec<DigitGrid> {
    let mut rng = Rng::from_seed(SEED);
    let mut boards = Vec::with_capacity(want);
    while boards.len() < want {
        let mut digits = random_solution(&mut rng).0;
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        for &cell in &positions {
            if digits.get(cell).is_none() {
                continue;
            }
            digits.clear(cell);
            let clues = digits.digit_count();
            if clues < CLUE_MIN {
                break; // past the working band — start a fresh solution
            }
            if clues <= CLUE_MAX {
                boards.push(digits.clone());
                if boards.len() >= want {
                    break;
                }
            }
        }
    }
    boards
}

fn warp_pass(c: &mut Criterion) {
    let pool = strip_pool(WARPS * WarpInput::LANES);

    // Surface the input regime instead of hiding it: the clue spread of the pool.
    let clues: Vec<usize> = pool.iter().map(DigitGrid::digit_count).collect();
    let mean = clues.iter().sum::<usize>() as f64 / clues.len() as f64;
    eprintln!(
        "warp_pass pool: {} boards / {} warps, clues min={} mean={:.1} max={}",
        pool.len(),
        WARPS,
        clues.iter().min().unwrap(),
        mean,
        clues.iter().max().unwrap(),
    );

    let warps: Vec<WarpInput> =
        pool.chunks(WarpInput::LANES).map(WarpInput::from_grids).collect();

    let mut g = c.benchmark_group("warp_pass_full");
    g.throughput(Throughput::Elements(warps.len() as u64)); // per warp-pass (8 lanes wide)

    // The literal microbench: ONE cheap-closure pass over every warp in the pool.
    g.bench_function("one_pass", |b| {
        b.iter_batched(
            || warps.clone(),
            |mut ws| {
                for w in ws.iter_mut() {
                    black_box(w.pass());
                }
            },
            BatchSize::SmallInput,
        );
    });

    // Drive each warp to fixpoint (repeat passes until no active lane progresses) —
    // closer to the real per-strip baseline cost than a single pass.
    g.bench_function("to_fixpoint", |b| {
        b.iter_batched(
            || warps.clone(),
            |mut ws| {
                for w in ws.iter_mut() {
                    black_box(w.to_fixpoint());
                }
            },
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

criterion_group!(benches, warp_pass);
criterion_main!(benches);
