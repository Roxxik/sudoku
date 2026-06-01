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
        // base-board changes: a cell opens (non-monotone for forward closure).
        let mut fastpath_accepts = 0u64; // alts==0 auto-accept
        let mut baseline_accepts = 0u64; // prober-unique + baseline-solved
        let mut rejects = 0u64; // probe failed -> reverted (no base change)
        let mut attempts_run = 0u64;
        // prober time split by outcome: unique probes are ~1 node = pure closure.
        let mut uniq_time = std::time::Duration::ZERO;
        let mut nonuniq_time = std::time::Duration::ZERO;
        let mut uniq_calls = 0u64;
        let mut nonuniq_calls = 0u64;
        let mut closure_time = std::time::Duration::ZERO;
        let mut closure_calls = 0u64;

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
                    fastpath_accepts += 1;
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

                // time the first-closure (clone+restrict+propagate, no branch)
                let tc = std::time::Instant::now();
                let _ = bb.probe_closure_only(i, alts);
                closure_time += tc.elapsed();
                closure_calls += 1;

                let t0 = std::time::Instant::now();
                let nonuniq = bb.any_alt_solves(i, alts);
                let dt = t0.elapsed();
                if nonuniq {
                    nonuniq_time += dt;
                    nonuniq_calls += 1;
                } else {
                    uniq_time += dt;
                    uniq_calls += 1;
                }
                if nonuniq {
                    rejects += 1;
                    cells[i] = orig;
                    bb.apply_place(i, orig, &mut placed);
                    continue;
                }
                base_calls += 1;
                let outcome = bb.baseline(baseline);
                if !outcome.solved {
                    rejects += 1;
                    cells[i] = orig;
                    bb.apply_place(i, orig, &mut placed);
                    continue;
                }
                baseline_accepts += 1;
                req_met = spec.requirement_met(&outcome.counts);
                if req_met {
                    best = Some(cells);
                }
            }
            if let Some(snap) = best {
                let _ = board_from_cells(&snap);
            }
            attempts_run += 1;
        }

        let p = probes as f64;
        let a = attempts_run as f64;
        let accepts = fastpath_accepts + baseline_accepts;
        println!("== {label} ==  {probes} probes, {base_calls} baseline-gate calls");
        println!("  empties/probe     {:>6.1}", sum_empties as f64 / p);
        println!("  base_solved/probe {:>6.1}  ({:.0}% of empties recomputed every probe)",
            sum_base_solved as f64 / p, 100.0 * sum_base_solved as f64 / sum_empties as f64);
        println!("  delta/probe       {:>6.1}  (cells left after the shared closure)",
            sum_delta as f64 / p);
        println!("  per attempt: probes {:.1}  accepts(base-change) {:.1}  [fastpath {:.1} + baseline {:.1}]  rejects {:.1}",
            p / a, accepts as f64 / a, fastpath_accepts as f64 / a, baseline_accepts as f64 / a, rejects as f64 / a);
        println!("  -> base changes ({:.1}) vs probes ({:.1}): ratio {:.2}",
            accepts as f64 / a, p / a, accepts as f64 / p);
        let ut = uniq_time.as_secs_f64();
        let nt = nonuniq_time.as_secs_f64();
        let uavg = ut * 1e9 / uniq_calls as f64;
        let navg = nt * 1e9 / nonuniq_calls as f64;
        // closure share: every probe pays ~one closure (uavg ~= closure cost);
        // nonunique probes additionally pay branch = navg - uavg.
        let closure_total = uavg * (uniq_calls + nonuniq_calls) as f64; // ns
        let branch_total = (navg - uavg) * nonuniq_calls as f64; // ns
        println!("  prober time: unique {:.0} ns/call ({} calls), nonunique {:.0} ns/call ({} calls)",
            uavg, uniq_calls, navg, nonuniq_calls);
        let _ = (closure_total, branch_total);
        // direct measurement: first-closure cost (clone+restrict+propagate, no branch)
        let cavg = closure_time.as_secs_f64() * 1e9 / closure_calls as f64;
        let full_total = ut + nt; // total any_alt_solves time
        let clo_total = closure_time.as_secs_f64();
        println!("  measured first-closure {:.0} ns/call; full prober {:.0} ns/call avg",
            cavg, full_total * 1e9 / (uniq_calls + nonuniq_calls) as f64);
        println!("  -> closure is ~{:.0}% of prober time; branch+clone overhead ~{:.0}%  (closure = redundant part)\n",
            100.0 * clo_total / full_total, 100.0 * (full_total - clo_total) / full_total);
    }
}
