//! W=8 closure-kernel gate: does the vectorized `propagate`-to-fixpoint pack
//! ~3x IN CONTEXT (with per-lane masking + fixpoint-depth divergence that the
//! isolated microbenches — sweepbench/placesmear/psbench — left out)?
//!
//! This is NOT the full prober (no DFS stacks, no refill scheduler — those are the
//! real build). It isolates the foundational claim: the uniform part of the kernel
//! composes. It collects real restricted boards from the strip loop (exactly the
//! states the existence prober starts its DFS from), then times propagate-to-fixpoint
//! two ways on identical boards:
//!
//!   scalar : single-layout, smear place, smear hidden-single placement, no LC
//!            (the UPDATED scalar prober's closure), one board at a time.
//!   simt   : the same closure, W=8 puzzles per Simd<u32,8>, masked, run to the
//!            slowest lane's fixpoint.
//!
//! Both are validated to reach the same fixpoint per board (solved? + empty-cell
//! count). The closure is confluent, so smear/batching order doesn't change it.
//! Reported: ns/board each + packing factor — the in-context kernel speedup.
//!
//! Usage: cargo run --release -p generator-lab --example closurekernel -- [--attempts N=2000] [--iters I=200]

#![feature(portable_simd)]

use std::simd::cmp::{SimdPartialEq, SimdPartialOrd};
use std::simd::num::SimdUint;
use std::simd::{Mask, Select, Simd};
use std::time::Instant;

use generator_lab::bb::{BitBoard, Placed};
use generator_lab::fill::random_full_grid;
use generator_lab::grid::{CELLS, Digit, PEERS, digit_to_bit};
use generator_lab::probe::{Prober, Search};
use generator_lab::repr::banded::{Bands, RowMajor};
use generator_lab::repr::{DigitGrid, Marks, SearchState};
use generator_lab::rng::Rng;
use generator_lab::scan::Bivalue;
use generator_lab::spec_for_mode;

const W: usize = 8;
type V = Simd<u32, W>;
type M = Mask<i32, W>;

/// The scalar existence oracle on the new prober stack: build a `DigitGrid` from the
/// strip's cleared `cells`, forbid `orig` at `i` to restrict the cell to its
/// alternates, and ask whether some other digit still completes — the new-repr twin
/// of bb's old alt-completion existence probe, the soundness reference for the single-layout DFS.
fn oracle_alt_solves(cells: &[Digit; CELLS], i: usize, orig: Digit) -> bool {
    let grid = DigitGrid::from_array(core::array::from_fn(|c| {
        generator_lab::repr::Digit::new(cells[c])
    }));
    let mut probe = SearchState::<Bands<RowMajor>>::from_digits(&grid);
    probe.forbid(i, generator_lab::repr::Digit::new(orig).expect("nonzero clue digit"));
    Search::<Bivalue>::has_completion(probe)
}

const fn rm_lane(c: usize) -> usize {
    (c / 9) / 3
}
const fn rm_bit(c: usize) -> usize {
    ((c / 9) % 3) * 9 + c % 9
}

const BOX_CELLS: [u32; 3] = {
    let mut t = [0u32; 3];
    let mut k = 0;
    while k < 3 {
        let c3 = 0b111u32 << (3 * k);
        t[k] = c3 | (c3 << 9) | (c3 << 18);
        k += 1;
    }
    t
};
const ROW_MASK: [u32; 3] = [0x1FF, 0x1FF << 9, 0x1FF << 18];

// ---- a single board: row-major r[9][3] + unsolved[3] ----
#[derive(Clone, Copy)]
struct Bd {
    r: [[u32; 3]; 9],
    u: [u32; 3],
}

fn from_cells_restricted(cells: &[Digit; CELLS], i: usize, alts: u16) -> Bd {
    let mut b = Bd { r: [[0; 3]; 9], u: [0; 3] };
    for c in 0..CELLS {
        if cells[c] == 0 {
            let (l, bit) = (rm_lane(c), rm_bit(c));
            b.u[l] |= 1 << bit;
            let mut used = 0u16;
            for &q in &PEERS[c] {
                if cells[q] != 0 {
                    used |= 1 << (cells[q] - 1);
                }
            }
            let cand = if c == i { alts } else { !used & 0x1FF };
            for d in 0..9 {
                if cand & (1 << d) != 0 {
                    b.r[d][l] |= 1 << bit;
                }
            }
        }
    }
    b
}

// ---- SCALAR closure (single-layout, smear, no LC) ----
#[inline]
fn smear_s(group: [u32; 3]) -> ([u32; 3], u32, u32, u32) {
    let fold = |g: u32| (g & 0x1FF) | ((g >> 9) & 0x1FF) | ((g >> 18) & 0x1FF);
    let col_occ = fold(group[0]) | fold(group[1]) | fold(group[2]);
    let colpeer = col_occ | (col_occ << 9) | (col_occ << 18);
    let mut out = [0u32; 3];
    let (mut rows, mut boxes) = (0u32, 0u32);
    for b in 0..3 {
        let g = group[b];
        let mut rp = 0;
        let mut bp = 0;
        for i in 0..3 {
            if g & ROW_MASK[i] != 0 {
                rp |= ROW_MASK[i];
                rows |= 1 << (3 * b + i);
            }
        }
        for k in 0..3 {
            if g & BOX_CELLS[k] != 0 {
                bp |= BOX_CELLS[k];
                boxes |= 1 << (3 * b + k);
            }
        }
        out[b] = rp | bp | colpeer;
    }
    (out, rows.count_ones(), col_occ.count_ones(), boxes.count_ones())
}

/// Run the closure to fixpoint, returning the resulting board and whether a
/// contradiction was reached. `#[inline]` so `closure_scalar` (the timed path)
/// folds it back to the original codegen — the DFS collector shares the SAME
/// closure body, so the interior nodes it produces are exactly the states a
/// single-layout no-LC SIMT prober would visit.
#[inline]
fn closure_core(mut b: Bd) -> (Bd, bool) {
    let mut dead = false;
    loop {
        // naked singles to fixpoint
        loop {
            let (mut ones, mut twos) = ([0u32; 3], [0u32; 3]);
            for d in 0..9 {
                for k in 0..3 {
                    twos[k] |= ones[k] & b.r[d][k];
                    ones[k] |= b.r[d][k];
                }
            }
            let mut singles = [0u32; 3];
            let mut any = false;
            for k in 0..3 {
                if b.u[k] & !ones[k] != 0 {
                    dead = true;
                }
                singles[k] = b.u[k] & ones[k] & !twos[k];
                any |= singles[k] != 0;
            }
            if !any {
                break;
            }
            for d in 0..9 {
                let group = [singles[0] & b.r[d][0], singles[1] & b.r[d][1], singles[2] & b.r[d][2]];
                let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
                if n == 0 {
                    continue;
                }
                let (peers, dr, dc, db) = smear_s(group);
                if dr < n || dc < n || db < n {
                    dead = true;
                }
                for k in 0..3 {
                    b.u[k] &= !group[k];
                    b.r[d][k] &= !peers[k];
                }
            }
        }
        // hidden singles (rows + boxes), all digits, one pass, batched via smear
        let mut changed = false;
        for d in 0..9 {
            let mut group = [0u32; 3];
            for k in 0..3 {
                let live = b.r[d][k] & b.u[k];
                for rr in 0..3 {
                    let rc = live & ROW_MASK[rr];
                    if rc != 0 && rc & (rc - 1) == 0 {
                        group[k] |= rc;
                    }
                }
                for bx in 0..3 {
                    let bc = live & BOX_CELLS[bx];
                    if bc != 0 && bc & (bc - 1) == 0 {
                        group[k] |= bc;
                    }
                }
            }
            let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
            if n == 0 {
                continue;
            }
            let (peers, dr, dc, db) = smear_s(group);
            if dr < n || dc < n || db < n {
                dead = true;
            }
            for k in 0..3 {
                b.u[k] &= !group[k];
                b.r[d][k] &= !peers[k];
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    (b, dead)
}

/// Returns (solved, dead, empties). Thin wrapper over `closure_core` so the timed
/// scalar path and the DFS node collector cannot drift apart.
fn closure_scalar(b: Bd) -> (bool, bool, u32) {
    let (b, dead) = closure_core(b);
    let empties = b.u[0].count_ones() + b.u[1].count_ones() + b.u[2].count_ones();
    (empties == 0 && !dead, dead, empties)
}

/// MRV branch cell on a fixpoint board: the unsolved cell with the fewest
/// candidates (post-closure every unsolved cell has >=2, so this finds a bivalue
/// when one exists). Returns (band, bit-in-band, candidate-digit mask). Only
/// called when the board is stuck (empties > 0, not dead), so always Some.
fn branch_cell(b: &Bd) -> (usize, usize, u16) {
    let mut best = (0usize, 0usize, 0u16);
    let mut bestcnt = 10u32;
    for k in 0..3 {
        let mut u = b.u[k];
        while u != 0 {
            let bit = u.trailing_zeros() as usize;
            u &= u - 1;
            let m = 1u32 << bit;
            let mut cand = 0u16;
            for d in 0..9 {
                if b.r[d][k] & m != 0 {
                    cand |= 1 << d;
                }
            }
            let cnt = cand.count_ones();
            if cnt < bestcnt {
                bestcnt = cnt;
                best = (k, bit, cand);
            }
        }
    }
    best
}

/// Branch child: assume digit `dd` at (band k, bit) by clearing every other
/// digit's candidate there. The cell stays unsolved; the child's closure places
/// it as a naked single and propagates — same shape as a real probe restriction.
fn assign(mut b: Bd, k: usize, bit: usize, dd: usize) -> Bd {
    let m = 1u32 << bit;
    for d in 0..9 {
        if d != dd {
            b.r[d][k] &= !m;
        }
    }
    b
}

/// The existence DFS, single-layout no-LC, with early-exit on the first solution
/// (exactly what the alt-completion existence probe does). Pushes every node's PRE-closure board to
/// `out`, so `out` ends up holding the real per-node board population — roots and
/// interior alike. Returns whether any solution exists (the prober verdict).
fn dfs(b: Bd, out: &mut Vec<Bd>, budget: &mut u32) -> bool {
    out.push(b);
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let (fb, dead) = closure_core(b);
    if dead {
        return false;
    }
    if fb.u[0] | fb.u[1] | fb.u[2] == 0 {
        return true; // solved -> existence found, unwind
    }
    let (k, bit, cand) = branch_cell(&fb);
    let mut c = cand;
    while c != 0 {
        let dd = c.trailing_zeros() as usize;
        c &= c - 1;
        if dfs(assign(fb, k, bit, dd), out, budget) {
            return true;
        }
    }
    false
}

// ---- VECTOR closure (W=8 puzzles per Simd<u32,8>) ----
#[derive(Clone, Copy)]
struct BdV {
    r: [[V; 3]; 9],
    u: [V; 3],
}

fn load(batch: &[Bd]) -> BdV {
    let mut bv = BdV { r: [[V::splat(0); 3]; 9], u: [V::splat(0); 3] };
    for d in 0..9 {
        for k in 0..3 {
            bv.r[d][k] = V::from_array(core::array::from_fn(|j| batch[j].r[d][k]));
        }
    }
    for k in 0..3 {
        bv.u[k] = V::from_array(core::array::from_fn(|j| batch[j].u[k]));
    }
    bv
}

#[inline(always)]
fn one_bit(x: V) -> M {
    // exactly-one OR none -> caller wants "exactly one": x!=0 & x&(x-1)==0
    x.simd_ne(V::splat(0)) & (x & (x - V::splat(1))).simd_eq(V::splat(0))
}

/// Per-lane popcount — the portable-simd built-in, which lowers to `vpopcntd` on
/// AVX-512-VPOPCNTDQ (this machine) instead of a SWAR chain.
#[inline(always)]
fn popcnt_v(x: V) -> V {
    x.count_ones()
}

/// Returns (peers, row_occ, col_occ, box_occ) — the occupancies are 9-bit per lane,
/// for the conflict check (a unit with ≥2 group cells shows as distinct < |group|).
#[inline]
fn smear_v(group: [V; 3]) -> ([V; 3], V, V, V) {
    let m9 = V::splat(0x1FF);
    let mut col_occ = V::splat(0);
    for b in 0..3 {
        col_occ |= (group[b] & m9) | ((group[b] >> V::splat(9)) & m9) | ((group[b] >> V::splat(18)) & m9);
    }
    let colpeer = col_occ | (col_occ << V::splat(9)) | (col_occ << V::splat(18));
    let mut out = [V::splat(0); 3];
    let mut row_occ = V::splat(0);
    let mut box_occ = V::splat(0);
    for b in 0..3 {
        let g = group[b];
        let mut rp = V::splat(0);
        for i in 0..3 {
            let rm = V::splat(ROW_MASK[i]);
            let on = (g & rm).simd_ne(V::splat(0));
            rp |= on.select(rm, V::splat(0));
            row_occ |= on.select(V::splat(1 << (3 * b + i)), V::splat(0));
        }
        let mut bp = V::splat(0);
        for k in 0..3 {
            let bm = V::splat(BOX_CELLS[k]);
            let on = (g & bm).simd_ne(V::splat(0));
            bp |= on.select(bm, V::splat(0));
            box_occ |= on.select(V::splat(1 << (3 * b + k)), V::splat(0));
        }
        out[b] = rp | bp | colpeer;
    }
    (out, row_occ, col_occ, box_occ)
}

/// Per-lane `dead` contribution of placing `group` for one digit: a unit holding
/// ≥2 group cells (distinct touched units < group size).
#[inline(always)]
fn conflict_v(group: [V; 3], row_occ: V, col_occ: V, box_occ: V) -> M {
    let n = popcnt_v(group[0]) + popcnt_v(group[1]) + popcnt_v(group[2]);
    popcnt_v(row_occ).simd_lt(n) | popcnt_v(col_occ).simd_lt(n) | popcnt_v(box_occ).simd_lt(n)
}

/// Returns per-lane (solved_mask, empties_vector). `solved` is empties==0 AND not
/// dead (a contradiction reached), matching `closure_scalar`.
fn closure_vector(mut b: BdV) -> (V, V) {
    let mut dead = M::splat(false);
    loop {
        loop {
            let (mut ones, mut twos) = ([V::splat(0); 3], [V::splat(0); 3]);
            for d in 0..9 {
                for k in 0..3 {
                    twos[k] |= ones[k] & b.r[d][k];
                    ones[k] |= b.r[d][k];
                }
            }
            let mut singles = [V::splat(0); 3];
            let mut any = M::splat(false);
            for k in 0..3 {
                dead |= (b.u[k] & !ones[k]).simd_ne(V::splat(0)); // a cell with no candidate
                singles[k] = b.u[k] & ones[k] & !twos[k];
                any |= singles[k].simd_ne(V::splat(0));
            }
            if !any.any() {
                break;
            }
            for d in 0..9 {
                let group = [singles[0] & b.r[d][0], singles[1] & b.r[d][1], singles[2] & b.r[d][2]];
                let (peers, ro, co, bo) = smear_v(group);
                dead |= conflict_v(group, ro, co, bo);
                for k in 0..3 {
                    b.u[k] &= !group[k];
                    b.r[d][k] &= !peers[k];
                }
            }
        }
        let mut changed = M::splat(false);
        for d in 0..9 {
            let mut group = [V::splat(0); 3];
            for k in 0..3 {
                let live = b.r[d][k] & b.u[k];
                for rr in 0..3 {
                    let rc = live & V::splat(ROW_MASK[rr]);
                    group[k] |= one_bit(rc).select(rc, V::splat(0));
                }
                for bx in 0..3 {
                    let bc = live & V::splat(BOX_CELLS[bx]);
                    group[k] |= one_bit(bc).select(bc, V::splat(0));
                }
            }
            let (peers, ro, co, bo) = smear_v(group);
            dead |= conflict_v(group, ro, co, bo);
            for k in 0..3 {
                b.u[k] &= !group[k];
                b.r[d][k] &= !peers[k];
            }
            for k in 0..3 {
                changed |= group[k].simd_ne(V::splat(0));
            }
        }
        if !changed.any() {
            break;
        }
    }
    let empties = b.u[0].count_ones() + b.u[1].count_ones() + b.u[2].count_ones();
    let solved = (empties.simd_eq(V::splat(0)) & !dead).select(V::splat(1), V::splat(0));
    (solved, empties)
}

/// Validate vector==scalar fixpoint and time scalar vs simt on one board set.
fn bench_and_report(label: &str, boards: &[Bd], iters: usize) {
    let n = boards.len() - boards.len() % W;
    let boards = &boards[..n];
    if n == 0 {
        println!("  [{label}] no boards");
        return;
    }

    let mut mism = 0u64;
    let mut dead_n = 0u64;
    for base in (0..n).step_by(W) {
        let (sv, ev) = closure_vector(load(&boards[base..base + W]));
        for j in 0..W {
            let (ssolved, sdead, sempt) = closure_scalar(boards[base + j]);
            if sdead {
                dead_n += 1;
            }
            if ev[j] != sempt || (sv[j] == 1) != ssolved {
                mism += 1;
            }
        }
    }

    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for &bd in boards {
            let (s, _, e) = closure_scalar(bd);
            acc = acc.wrapping_add(s as u64).wrapping_add(e as u64);
        }
    }
    let ns_s = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    let batches: Vec<BdV> = (0..n).step_by(W).map(|base| load(&boards[base..base + W])).collect();
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for &bv in &batches {
            let (sv, ev) = closure_vector(bv);
            acc = acc.wrapping_add(sv.reduce_sum() as u64).wrapping_add(ev.reduce_sum() as u64);
        }
    }
    let ns_vp = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    println!(
        "  [{label:<11}] {n:>7} boards  dead {:>4.1}%  mism {mism}  scalar {ns_s:>7.2}ns  simt {ns_vp:>7.2}ns  packing {:.2}x",
        100.0 * dead_n as f64 / n as f64,
        ns_s / ns_vp,
    );
}

fn main() {
    let mut attempts = 2000usize;
    let mut iters = 200usize;
    let mut mode = 0u32;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--attempts" => attempts = it.next().and_then(|s| s.parse().ok()).unwrap_or(attempts),
            "--iters" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or(iters),
            "--mode" => mode = if it.next().as_deref() == Some("drill") { 1 } else { 0 },
            _ => {}
        }
    }

    // Collect real restricted boards from the strip loop.
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let mut rng = Rng::from_seed(1);
    let mut roots: Vec<Bd> = Vec::new(); // the root closure per query (shallow, ~half dead)
    let mut nodes: Vec<Bd> = Vec::new(); // EVERY DFS node, roots + interior, early-exit faithful
    let mut verdict_mism = 0u64; // my single-layout DFS vs the real prober
    let mut queries = 0u64;
    for _ in 0..attempts {
        let solution = random_full_grid(&mut rng);
        let mut positions: [usize; CELLS] = core::array::from_fn(|i| i);
        rng.shuffle(&mut positions);
        let mut bb = BitBoard::from_board(&solution);
        let mut placed = Placed::from_board(&solution);
        let mut cells: [Digit; CELLS] = core::array::from_fn(|i| solution.cell(i));
        for i in positions {
            if cells[i] == 0 {
                continue;
            }
            let orig = cells[i];
            cells[i] = 0;
            let cand = bb.apply_clear(i, orig, &mut placed);
            let alts = cand & !digit_to_bit(orig);
            if alts == 0 {
                continue;
            }
            let root = from_cells_restricted(&cells, i, alts);
            roots.push(root);
            // Collect the full DFS node population for this query and cross-check
            // the verdict against the real dual-layout prober (existence is
            // layout/technique/branch-order independent, so it MUST match).
            let mut budget = 5_000_000u32;
            let mine = dfs(root, &mut nodes, &mut budget);
            let real = oracle_alt_solves(&cells, i, orig);
            if mine != real {
                verdict_mism += 1;
            }
            queries += 1;
            if real {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
                continue;
            }
            let o = bb.baseline(baseline, forced);
            if !o.solved {
                cells[i] = orig;
                bb.apply_place(i, orig, &mut placed);
            }
        }
    }

    println!("closurekernel: W={W}, {queries} queries");
    println!(
        "  DFS verdict mismatches vs prober: {verdict_mism}   <- MUST be 0 (soundness)",
    );
    println!("  nodes/query: {:.2}  (roots {}, all nodes {})\n", nodes.len() as f64 / queries as f64, roots.len(), nodes.len());

    // Roots-only is the shallow, ~half-dead sample the ORIGINAL bench timed.
    // All-nodes adds the interior DFS states (the 78%-of-work concentration) —
    // its packing is the real per-node closure speedup the SIMT prober realizes.
    bench_and_report("root only", &roots, iters);
    bench_and_report("all nodes", &nodes, iters);
    println!("  [packing = scalar/simt, load-amortized; excludes DFS branch/clone/refill]");
}
