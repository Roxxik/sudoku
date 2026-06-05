//! THROWAWAY: bb strip path profiling target, mirror of proberprof, for head-to-head
//! self-time comparison of bb's prober vs the new Search prober.
use generator_lab::bb::{BitBoard, Placed};
use generator_lab::fill::random_full_grid;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::rng::Rng;

fn strip_bb(rng: &mut Rng, solution: &Board) -> usize {
    let mut cells: [_; CELLS] = core::array::from_fn(|i| solution.cell(i));
    let mut bb = BitBoard::from_board(solution);
    let mut placed = Placed::from_board(solution);
    let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut positions);
    for cell in positions {
        let orig = cells[cell];
        if orig == 0 { continue; }
        cells[cell] = 0;
        let cand = bb.apply_clear(cell, orig, &mut placed);
        let alts = cand & !digit_to_bit(orig);
        if alts != 0 && bb.any_alt_solves(cell, alts) {
            cells[cell] = orig;
            bb.apply_place(cell, orig, &mut placed);
        }
    }
    cells.iter().filter(|&&d| d != 0).count()
}

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5000);
    let mut givens = 0usize;
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let solution = random_full_grid(&mut rng);
        givens += strip_bb(&mut rng, &solution);
    }
    std::hint::black_box(givens);
    println!("{n} seeds, avg givens {:.1}", givens as f64 / n as f64);
}
