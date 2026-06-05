//! THROWAWAY: compare prober node counts (solve_first/count invocations) between the
//! bb strip and the new Search<Bivalue> strip, same strategy, to see if the new path
//! does more search work (weaker propagation) or just costs more per node.
//! Build: cargo run --release -p generator-lab --features count --example nodecount
use generator_lab::bb::{
    band_ctr_reset, band_ctr_snapshot, pctr_reset, pctr_snapshot, BitBoard, Placed,
};
use generator_lab::fill::{random_full_grid, random_solution};
use generator_lab::generate::strip_to_minimal;
use generator_lab::grid::{Board, CELLS, digit_to_bit};
use generator_lab::probe::Search;
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;

fn strip_bb(rng: &mut Rng, solution: &Board) {
    let mut cells: [_; CELLS] = core::array::from_fn(|i| solution.cell(i));
    let mut bb = BitBoard::from_board(solution);
    let mut placed = Placed::from_board(solution);
    let mut pos: [usize; CELLS] = core::array::from_fn(|i| i);
    rng.shuffle(&mut pos);
    for cell in pos {
        let orig = cells[cell];
        if orig == 0 {
            continue;
        }
        cells[cell] = 0;
        let cand = bb.apply_clear(cell, orig, &mut placed);
        let alts = cand & !digit_to_bit(orig);
        if alts != 0 && bb.any_alt_solves(cell, alts) {
            cells[cell] = orig;
            bb.apply_place(cell, orig, &mut placed);
        }
    }
}

fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000);

    pctr_reset();
    band_ctr_reset();
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let sol = random_solution(&mut rng);
        let _ = strip_to_minimal::<Bands<RowMajor>, Search<Bivalue>>(&mut rng, &sol);
    }
    let new_nodes = pctr_snapshot()[2];
    let new_prop = band_ctr_snapshot()[0];
    let new_pass = band_ctr_snapshot()[1];

    pctr_reset();
    band_ctr_reset();
    for seed in 0..n {
        let mut rng = Rng::from_seed(seed);
        let sol = random_full_grid(&mut rng);
        strip_bb(&mut rng, &sol);
    }
    let bb_nodes = pctr_snapshot()[2];
    let bb_prop = band_ctr_snapshot()[0];
    let bb_pass = band_ctr_snapshot()[1];

    println!("n={n}");
    println!("                       new      bb     new/bb");
    println!("nodes/puzzle       {:8.1} {:7.1}   {:.3}", new_nodes as f64 / n as f64, bb_nodes as f64 / n as f64, new_nodes as f64 / bb_nodes as f64);
    println!("propagate/puzzle   {:8.1} {:7.1}   {:.3}", new_prop as f64 / n as f64, bb_prop as f64 / n as f64, new_prop as f64 / bb_prop as f64);
    println!("band-pass/puzzle   {:8.1} {:7.1}   {:.3}", new_pass as f64 / n as f64, bb_pass as f64 / n as f64, new_pass as f64 / bb_pass as f64);
}
