//! THROWAWAY: measure cross-probe propagation redundancy. For each prober call
//! in the real strip loop, compare:
//!   - empties      : empty cells on the base board at the probe
//!   - base_solved  : cells the UNRESTRICTED base closure (naked+hidden singles,
//!                    LC to fixpoint, no branch) determines on its own
//!   - delta        : empties - base_solved = cells still open after the shared
//!                    closure (what a probe would have left to do incrementally)
//! If base_solved is large and stable across probes, that closure is recomputed
//! from scratch on every one of the ~52 probes + ~28 baseline calls per attempt.

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::generator::board_from_cells;
use generator_lab::generator::random_full_grid;
use generator_lab::grid::{CELLS, Digit, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;

fn main() {
    let attempts = 4000usize;
    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);
        let baseline = spec.baseline_mask();
        let mut rng = Rng::from_seed(1);

        let mut probes = 0u64;
        let mut sum_empties = 0u64;
        let mut sum_base_solved = 0u64;
        let mut sum_delta = 0u64;
        // baseline-gate calls (the other consumer of the closure)
        let mut base_calls = 0u64;

        for _ in 0..attempts {
            let solution = random_full_grid(&mut rng);
            let mut positions: Vec<usize> = (0..CELLS).collect();
            rng.shuffle(&mut positions);
            let mut bb = BitBoard::from_board(&solution);
            let mut placed = Placed::from_board(&solution);
            let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));
            let mut req_met = false;
            let mut best: Option<[Digit; CELLS]> = None;

            for i in positions {
                if cells[i] == 0 {
                    continue;
                }
                let orig = cells[i];
                cells[i] = 0;
                let cand = bb.apply_clear(i, orig, &mut placed);
                let alts = cand & !digit_to_bit(orig);
                if alts == 0 {
                    if req_met {
                        best = Some(cells);
                    }
                    continue;
                }

                // measure the shared base closure (unrestricted, no branch)
                let empties = bb.count_empties();
                let base_solved = bb.closure_solved();
                probes += 1;
                sum_empties += empties as u64;
                sum_base_solved += base_solved as u64;
                sum_delta += (empties - base_solved) as u64;

                if bb.any_alt_solves(i, alts) {
                    cells[i] = orig;
                    bb.apply_place(i, orig, &mut placed);
                    continue;
                }
                base_calls += 1;
                let outcome = bb.baseline(baseline);
                if !outcome.solved {
                    cells[i] = orig;
                    bb.apply_place(i, orig, &mut placed);
                    continue;
                }
                req_met = spec.requirement_met(&outcome.counts);
                if req_met {
                    best = Some(cells);
                }
            }
            if let Some(snap) = best {
                let _ = board_from_cells(&snap);
            }
        }

        let p = probes as f64;
        println!("== {label} ==  {probes} probes, {base_calls} baseline-gate calls");
        println!("  empties/probe     {:>6.1}", sum_empties as f64 / p);
        println!("  base_solved/probe {:>6.1}  ({:.0}% of empties recomputed every probe)",
            sum_base_solved as f64 / p, 100.0 * sum_base_solved as f64 / sum_empties as f64);
        println!("  delta/probe       {:>6.1}  (cells left after the shared closure)\n",
            sum_delta as f64 / p);
    }
}
