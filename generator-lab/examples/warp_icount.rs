//! Deterministic fixed-work driver for `warp_pass_full`, for SDE `-mix` instruction
//! counting (drift-free A/B of a kernel micro-change). Mirrors benches/warp_pass.rs's
//! strip pool exactly, but runs a FIXED number of `to_fixpoint` rounds with no criterion
//! machinery, so SDE's `*total` / per-opcode dynamic counts isolate the kernel.
//!
//! Build both variants and compare under SDE:
//!   cargo build --release --features bench --example warp_icount
//!   ~/opt/sde-external-.../sde64 -mix -omix mix.out -- target/release/examples/warp_icount
//!
//! The harness work (pool clone per round) is identical across builds, so any delta in
//! the VPOPCNTD / VPSUBD / *total columns is purely the kernel edit.
use generator_lab::bench_seam::WarpInput;
use generator_lab::fill::random_solution;
use generator_lab::repr::{CELLS, DigitGrid};
use generator_lab::rng::Rng;

const SEED: u64 = 0xC0FFEE; // identical to benches/warp_pass.rs
const WARPS: usize = 8;
const CLUE_MIN: usize = 24;
const CLUE_MAX: usize = 54;
const ROUNDS: usize = 2000; // fixed work; SDE is deterministic so this only sets the SNR

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
                break;
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

fn main() {
    let pool = strip_pool(WARPS * WarpInput::LANES);
    let template: Vec<WarpInput> =
        pool.chunks(WarpInput::LANES).map(WarpInput::from_grids).collect();

    // Fixed work: re-stamp the template each round (WarpInput is Copy) and drive to
    // fixpoint. The clone is build-invariant; only the kernel differs between builds.
    let mut checksum: u64 = 0;
    for _ in 0..ROUNDS {
        let mut ws = template.clone();
        for w in ws.iter_mut() {
            checksum = checksum.wrapping_add(w.to_fixpoint() as u64);
        }
    }
    // Print so the loop can't be elided; also a correctness invariant across builds.
    println!("checksum={checksum} rounds={ROUNDS} warps={}", template.len());
}
