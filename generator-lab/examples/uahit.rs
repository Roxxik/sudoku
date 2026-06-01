//! THROWAWAY: measure the unavoidable-set cache hit-rate, to decide whether
//! caching can cheaply answer the expensive nonunique probes (~52% of prober
//! time, ~30% of total).
//!
//! When removing cell i makes the puzzle non-unique, a 2nd solution S' exists;
//! the cells where S' differs from the known solution S form an unavoidable set
//! (all empty, since S' agrees with every given). We decompose that diff into
//! MINIMAL unavoidable sets (the diff of two solutions is a disjoint union of
//! small alternating cycles), cache them per attempt, and count how often a later
//! nonunique probe uncovers an ALREADY-cached set (a hit = no DFS needed).
//!
//! Decision is driven by the real `bb.any_alt_solves` so the strip trajectory is
//! identical to the generator; a self-contained scalar solver supplies the
//! witness grid only on cache misses (the bands don't store placed digits).

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::generator::{board_from_cells, random_full_grid};
use generator_lab::grid::{CELLS, Digit, digit_to_bit};
use generator_lab::rng::Rng;
use generator_lab::spec_for_mode;

/// Scalar MRV backtracking: fill the empty cells of `grid` (0 = empty) to any
/// completion, with cell `i_cell` restricted to the digits in `alts` (bit d-1).
/// Returns true with `grid` completed. Used only to obtain a witness 2nd solution.
fn solve_rec(grid: &mut [u8; CELLS], i_cell: usize, alts: u16) -> bool {
    let mut best = usize::MAX;
    let mut bestn = 10u32;
    let mut bestcands = 0u16;
    for c in 0..CELLS {
        if grid[c] != 0 {
            continue;
        }
        let mut cm = 0u16;
        for d in 1..=9u8 {
            if c == i_cell && alts & (1 << (d - 1)) == 0 {
                continue;
            }
            if peers_ok(grid, c, d) {
                cm |= 1 << (d - 1);
            }
        }
        let n = cm.count_ones();
        if n == 0 {
            return false;
        }
        if n < bestn {
            bestn = n;
            best = c;
            bestcands = cm;
        }
    }
    if best == usize::MAX {
        return true;
    }
    let mut m = bestcands;
    while m != 0 {
        let d = (m.trailing_zeros() + 1) as u8;
        m &= m - 1;
        grid[best] = d;
        if solve_rec(grid, i_cell, alts) {
            return true;
        }
        grid[best] = 0;
    }
    false
}

fn peers_ok(grid: &[u8; CELLS], c: usize, d: u8) -> bool {
    let (r, co) = (c / 9, c % 9);
    let (br, bc) = ((r / 3) * 3, (co / 3) * 3);
    for k in 0..9 {
        if grid[r * 9 + k] == d || grid[k * 9 + co] == d {
            return false;
        }
        if grid[(br + k / 3) * 9 + bc + k % 3] == d {
            return false;
        }
    }
    true
}

/// Decompose the diff between solutions `s2` and `sol` into minimal unavoidable
/// sets: cells coupled by sharing a (unit, digit) balance are unioned; connected
/// components are the minimal sets. Returns each as an 81-bit mask.
fn decompose(sol: &[u8; CELLS], s2: &[u8; CELLS]) -> Vec<u128> {
    let mut cells: Vec<usize> = Vec::new();
    for c in 0..CELLS {
        if s2[c] != sol[c] {
            cells.push(c);
        }
    }
    let n = cells.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut firstk = [usize::MAX; 27 * 9]; // key = unit*9 + (digit-1)
    for li in 0..n {
        let c = cells[li];
        let (row, col) = (c / 9, c % 9);
        let bx = (row / 3) * 3 + col / 3;
        for unit in [row, 9 + col, 18 + bx] {
            for d in [sol[c], s2[c]] {
                let k = unit * 9 + (d as usize - 1);
                if firstk[k] == usize::MAX {
                    firstk[k] = li;
                } else {
                    let (a, b) = (find(&mut parent, firstk[k]), find(&mut parent, li));
                    if a != b {
                        parent[a] = b;
                    }
                }
            }
        }
    }
    let mut by_root: std::collections::HashMap<usize, u128> = std::collections::HashMap::new();
    for li in 0..n {
        let r = find(&mut parent, li);
        *by_root.entry(r).or_insert(0) |= 1u128 << cells[li];
    }
    by_root.into_values().collect()
}

fn main() {
    let attempts = 4000usize;
    for (mode, label) in [(0u32, "train"), (1u32, "drill")] {
        let spec = spec_for_mode(mode);
        let baseline = spec.baseline_mask();
        let forced = spec.forced_mask();
        let mut rng = Rng::from_seed(1);

        let mut probes = 0u64;
        let mut nonuniq = 0u64;
        let mut hits = 0u64;
        let mut misses = 0u64;
        let mut sum_cache = 0u64;
        let mut sum_uset = 0u64;
        let mut uset_count = 0u64;
        let mut sum_comps = 0u64;
        let mut min_uset = u32::MAX;
        let mut max_cache = 0usize;
        let mut attempts_run = 0u64;

        for _ in 0..attempts {
            let solution = random_full_grid(&mut rng);
            let sol: [u8; CELLS] = core::array::from_fn(|c| solution.cell(c));
            let mut positions: Vec<usize> = (0..CELLS).collect();
            rng.shuffle(&mut positions);
            let mut bb = BitBoard::from_board(&solution);
            let mut placed = Placed::from_board(&solution);
            let mut cells: [Digit; CELLS] = sol;
            let mut req_met = false;
            let mut best: Option<[Digit; CELLS]> = None;
            let mut cache: Vec<u128> = Vec::new();

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

                probes += 1;
                let nonu = bb.any_alt_solves(i, alts);
                if nonu {
                    nonuniq += 1;
                    let empties: u128 = (0..CELLS).fold(0u128, |m, c| {
                        if cells[c] == 0 { m | (1u128 << c) } else { m }
                    });
                    sum_cache += cache.len() as u64;
                    max_cache = max_cache.max(cache.len());
                    if cache.iter().any(|&u| u & !empties == 0) {
                        hits += 1;
                    } else {
                        misses += 1;
                        let mut grid = cells;
                        let ok = solve_rec(&mut grid, i, alts);
                        assert!(ok, "scalar solver must find the 2nd solution bb reported");
                        let comps = decompose(&sol, &grid);
                        sum_comps += comps.len() as u64;
                        for u in comps {
                            sum_uset += u.count_ones() as u64;
                            uset_count += 1;
                            min_uset = min_uset.min(u.count_ones());
                            if !cache.iter().any(|&v| v & !u == 0) {
                                cache.retain(|&v| u & !v != 0);
                                cache.push(u);
                            }
                        }
                    }
                    cells[i] = orig;
                    bb.apply_place(i, orig, &mut placed);
                } else {
                    let outcome = bb.baseline(baseline, forced);
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
            }
            if let Some(snap) = best {
                let _ = board_from_cells(&snap);
            }
            attempts_run += 1;
        }

        let a = attempts_run as f64;
        println!("== {label} ==  {attempts_run} attempts");
        println!("  probes/att {:.1}  nonunique/att {:.1}", probes as f64 / a, nonuniq as f64 / a);
        println!("  cache HIT rate among nonunique: {:.1}%  ({hits} hits / {nonuniq} nonunique)",
            100.0 * hits as f64 / nonuniq as f64);
        println!("  hits/att {:.1}  misses(DFS-discoveries)/att {:.1}", hits as f64 / a, misses as f64 / a);
        println!("  avg components per witness {:.2}  (1.0 => diff is a single minimal set)",
            sum_comps as f64 / misses as f64);
        println!("  minimal-set size: avg {:.1} cells, smallest {}",
            sum_uset as f64 / uset_count as f64, min_uset);
        println!("  avg cache size at nonunique probe {:.1}  (max {})\n",
            sum_cache as f64 / nonuniq as f64, max_cache);
    }
}
