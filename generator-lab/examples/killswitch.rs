//! THE KILL-SWITCH: a minimal but COMPLETE W=8 refill prober vs the lean scalar
//! prober, on the real query stream, per core. This is the one measurement no
//! microbench could give — closurekernel (1.68x) timed only the uniform closure
//! and explicitly excluded DFS branch / clone / refill. This includes all of it:
//!
//!   scalar : single-layout, no-LC, smear-place, MRV-branch existence DFS — i.e.
//!            `banded-sl-nolc`, the bar the SIMT rewrite must beat. One query at a
//!            time (the shipped shape).
//!   simt   : the SAME closure as the per-pass primitive, but W=8 puzzles per
//!            `Simd<u32,8>` with PER-LANE explicit DFS stacks and a GREEDY REFILL
//!            scheduler — a lane that reaches a verdict immediately pulls the next
//!            query from the FIFO. This is granularity C from ARCHITECTURE.md, the
//!            only design that drives utilization to ~1.0.
//!
//! The FIFO is the real query stream replayed from the strip loop (the same
//! collection closurekernel/warpsim use). A full FIFO is exactly the "plentiful
//! independent queries" condition warpsim showed yields U~=1.0, so this isolates
//! the realized vector-prober speedup from the host-side coroutine plumbing that
//! would keep the FIFO full in production (a separate, non-vectorized step).
//!
//! Both produce IDENTICAL verdicts per query (existence is deterministic), cross-
//! checked against each other AND the real dual-layout prober. The reported number
//! is T_scalar / T_simt per core: the go/no-go. The model predicts ~1.2-1.5x here;
//! below ~1.2x the residue (clone/refill/divergence) ate the closure win — stop.
//!
//! Usage: cargo run --release -p generator-lab --example killswitch -- [--attempts N=2000] [--iters I=30] [--mode train|drill]

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

/// The scalar oracle on the new prober stack: build a `DigitGrid` from the strip's
/// `cells` (the cleared puzzle), forbid `orig` at `i` to restrict the cell to its
/// alternates, and ask whether some other digit still completes — the new-repr twin
/// of bb's old alt-completion existence probe, the soundness reference for the packed prober.
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

// ---- a single board: row-major r[9][3] + unsolved[3] = 30 u32 (single-layout) ----
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

// ===================== SCALAR closure + DFS (the bar) =======================

#[inline]
fn smear_s(group: [u32; 3]) -> [u32; 3] {
    let fold = |g: u32| (g & 0x1FF) | ((g >> 9) & 0x1FF) | ((g >> 18) & 0x1FF);
    let col_occ = fold(group[0]) | fold(group[1]) | fold(group[2]);
    let colpeer = col_occ | (col_occ << 9) | (col_occ << 18);
    let mut out = [0u32; 3];
    for b in 0..3 {
        let g = group[b];
        let mut rp = 0;
        for i in 0..3 {
            if g & ROW_MASK[i] != 0 {
                rp |= ROW_MASK[i];
            }
        }
        let mut bp = 0;
        for k in 0..3 {
            if g & BOX_CELLS[k] != 0 {
                bp |= BOX_CELLS[k];
            }
        }
        out[b] = rp | bp | colpeer;
    }
    out
}

/// Conflict for scalar: a placed group whose cells collide in a unit (distinct
/// touched units < group size) is a contradiction.
#[inline]
fn conflict_s(group: [u32; 3]) -> bool {
    let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
    let fold = |g: u32| (g & 0x1FF) | ((g >> 9) & 0x1FF) | ((g >> 18) & 0x1FF);
    let col = (fold(group[0]) | fold(group[1]) | fold(group[2])).count_ones();
    let mut rows = 0u32;
    let mut boxes = 0u32;
    for b in 0..3 {
        for i in 0..3 {
            if group[b] & ROW_MASK[i] != 0 {
                rows += 1;
            }
        }
        for k in 0..3 {
            if group[b] & BOX_CELLS[k] != 0 {
                boxes += 1;
            }
        }
    }
    rows < n || col < n || boxes < n
}

/// Run the closure to fixpoint. Returns (board, dead).
#[inline]
fn closure_core(mut b: Bd) -> (Bd, bool) {
    let mut dead = false;
    loop {
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
                if group[0] | group[1] | group[2] == 0 {
                    continue;
                }
                if conflict_s(group) {
                    dead = true;
                }
                let peers = smear_s(group);
                for k in 0..3 {
                    b.u[k] &= !group[k];
                    b.r[d][k] &= !peers[k];
                }
            }
        }
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
            if group[0] | group[1] | group[2] == 0 {
                continue;
            }
            if conflict_s(group) {
                dead = true;
            }
            let peers = smear_s(group);
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

/// MRV branch cell on a fixpoint board: unsolved cell with fewest candidates
/// (post-closure every unsolved cell has >=2). Returns (band, bit, cand mask).
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

/// Assume digit `dd` at (band k, bit): clear every other digit's candidate there.
#[inline]
fn assign(mut b: Bd, k: usize, bit: usize, dd: usize) -> Bd {
    let m = 1u32 << bit;
    for d in 0..9 {
        if d != dd {
            b.r[d][k] &= !m;
        }
    }
    b
}

/// Scalar existence DFS, early-exit on first solution. The bar (`banded-sl-nolc`).
fn solve_scalar(b: Bd) -> bool {
    let (fb, dead) = closure_core(b);
    if dead {
        return false;
    }
    if fb.u[0] | fb.u[1] | fb.u[2] == 0 {
        return true;
    }
    let (k, bit, cand) = branch_cell(&fb);
    let mut c = cand;
    while c != 0 {
        let dd = c.trailing_zeros() as usize;
        c &= c - 1;
        if solve_scalar(assign(fb, k, bit, dd)) {
            return true;
        }
    }
    false
}

/// Same DFS, counting nodes (closures), for the equal-work cross-check vs the warp.
fn solve_scalar_count(b: Bd, nodes: &mut u64) -> bool {
    *nodes += 1;
    let (fb, dead) = closure_core(b);
    if dead {
        return false;
    }
    if fb.u[0] | fb.u[1] | fb.u[2] == 0 {
        return true;
    }
    let (k, bit, cand) = branch_cell(&fb);
    let mut c = cand;
    while c != 0 {
        let dd = c.trailing_zeros() as usize;
        c &= c - 1;
        if solve_scalar_count(assign(fb, k, bit, dd), nodes) {
            return true;
        }
    }
    false
}

// ===================== VECTOR warp: W lanes, per-pass ========================

#[derive(Clone, Copy)]
struct BdV {
    r: [[V; 3]; 9],
    u: [V; 3],
}

impl BdV {
    fn zeroed() -> Self {
        BdV { r: [[V::splat(0); 3]; 9], u: [V::splat(0); 3] }
    }
}

#[inline(always)]
fn one_bit(x: V) -> M {
    x.simd_ne(V::splat(0)) & (x & (x - V::splat(1))).simd_eq(V::splat(0))
}

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

#[inline(always)]
fn conflict_v(group: [V; 3], row_occ: V, col_occ: V, box_occ: V) -> M {
    let n = group[0].count_ones() + group[1].count_ones() + group[2].count_ones();
    row_occ.count_ones().simd_lt(n) | col_occ.count_ones().simd_lt(n) | box_occ.count_ones().simd_lt(n)
}

/// ONE propagation pass (one naked sweep + one hidden sweep) across the warp,
/// applied only to `active` lanes. Returns per-lane (changed, dead, solved). The
/// scheduler calls this repeatedly; a lane is at fixpoint when a full pass leaves
/// it unchanged. Same operations as `closure_core`, so the per-lane fixpoint is
/// identical (the closure is confluent) — only the pass granularity differs.
fn warp_pass(b: &mut BdV, active: M) -> (M, M, M) {
    let z = V::splat(0);
    let mut changed = M::splat(false);
    let mut dead = M::splat(false);

    // naked singles, one sweep
    let (mut ones, mut twos) = ([z; 3], [z; 3]);
    for d in 0..9 {
        for k in 0..3 {
            twos[k] |= ones[k] & b.r[d][k];
            ones[k] |= b.r[d][k];
        }
    }
    let mut singles = [z; 3];
    for k in 0..3 {
        dead |= (b.u[k] & !ones[k]).simd_ne(z); // a cell with no candidate
        singles[k] = b.u[k] & ones[k] & !twos[k];
    }
    for d in 0..9 {
        let group = [singles[0] & b.r[d][0], singles[1] & b.r[d][1], singles[2] & b.r[d][2]];
        let (peers, ro, co, bo) = smear_v(group);
        dead |= conflict_v(group, ro, co, bo);
        for k in 0..3 {
            let gm = active.select(group[k], z);
            b.u[k] &= !gm;
            b.r[d][k] &= !active.select(peers[k], z);
            changed |= gm.simd_ne(z);
        }
    }
    // hidden singles, one sweep
    for d in 0..9 {
        let mut group = [z; 3];
        for k in 0..3 {
            let live = b.r[d][k] & b.u[k];
            for rr in 0..3 {
                let rc = live & V::splat(ROW_MASK[rr]);
                group[k] |= one_bit(rc).select(rc, z);
            }
            for bx in 0..3 {
                let bc = live & V::splat(BOX_CELLS[bx]);
                group[k] |= one_bit(bc).select(bc, z);
            }
        }
        let (peers, ro, co, bo) = smear_v(group);
        dead |= conflict_v(group, ro, co, bo);
        for k in 0..3 {
            let gm = active.select(group[k], z);
            b.u[k] &= !gm;
            b.r[d][k] &= !active.select(peers[k], z);
            changed |= gm.simd_ne(z);
        }
    }

    dead &= active;
    changed &= active;
    let empties = b.u[0].count_ones() + b.u[1].count_ones() + b.u[2].count_ones();
    let solved = active & empties.simd_eq(z) & !dead;
    (changed, dead, solved)
}

#[inline]
fn extract_lane(b: &BdV, j: usize) -> Bd {
    let mut o = Bd { r: [[0; 3]; 9], u: [0; 3] };
    for d in 0..9 {
        for k in 0..3 {
            o.r[d][k] = b.r[d][k].as_array()[j];
        }
    }
    for k in 0..3 {
        o.u[k] = b.u[k].as_array()[j];
    }
    o
}

/// Masked per-lane store: blend lane `j` of the warp to `s` (the AoSoA scatter the
/// design proposes — one vector blend per word, the rest of the lane untouched).
#[inline]
fn insert_lane(b: &mut BdV, lane_mask: M, s: &Bd) {
    for d in 0..9 {
        for k in 0..3 {
            b.r[d][k] = lane_mask.select(V::splat(s.r[d][k]), b.r[d][k]);
        }
    }
    for k in 0..3 {
        b.u[k] = lane_mask.select(V::splat(s.u[k]), b.u[k]);
    }
}

/// One per-lane DFS stack frame: the saved fixpoint state + the branch cell and
/// the digits not yet tried (the clone the design pays per branch).
struct Frame {
    saved: Bd,
    k: usize,
    bit: usize,
    rest: u16,
}

/// Per-run counters: enough to prove the warp does the SAME work as scalar (equal
/// nodes) and to read off utilization (active lane-passes / total lane-slots).
#[derive(Default)]
struct WarpStats {
    ticks: u64,
    active_passes: u64, // sum of active-lane count over all ticks
    nodes: u64,         // closures started = every board loaded into a lane
}

/// Run the whole FIFO through the W=8 greedy-refill warp. Writes each query's
/// verdict into `out`; returns a checksum (count of true) so timing can't elide.
fn run_warp(
    queries: &[Bd],
    stacks: &mut [Vec<Frame>; W],
    lane_mask: &[M; W],
    out: &mut [bool],
    st: &mut WarpStats,
) -> u64 {
    let n = queries.len();
    let mut warp = BdV::zeroed();
    let mut active = [false; W];
    let mut qof = [0usize; W];
    let mut next = 0usize;

    // initial fill
    for j in 0..W {
        if next < n {
            insert_lane(&mut warp, lane_mask[j], &queries[next]);
            stacks[j].clear();
            qof[j] = next;
            active[j] = true;
            next += 1;
            st.nodes += 1;
        }
    }

    let mut found = 0u64;
    let mut active_mask = M::from_array(active);
    // Safety backstop: nodes are tiny (~3.3/query), so this never trips in practice.
    let tick_cap = (n as u64 + 64) * 4096;

    while active_mask.any() {
        st.ticks += 1;
        st.active_passes += active_mask.to_bitmask().count_ones() as u64;
        if st.ticks > tick_cap {
            eprintln!("killswitch: tick cap hit (likely a warp bug) — aborting");
            break;
        }
        let (changed, dead, solved) = warp_pass(&mut warp, active_mask);

        for j in 0..W {
            if !active[j] {
                continue;
            }
            // verdict reached this tick? (solved => found; exhausted handled in backtrack)
            let mut verdict: Option<bool> = None;
            if solved.test(j) {
                verdict = Some(true);
            } else if dead.test(j) {
                // backtrack to nearest frame with a remaining candidate
                let mut restored = false;
                while let Some(f) = stacks[j].last_mut() {
                    if f.rest == 0 {
                        stacks[j].pop();
                        continue;
                    }
                    let dd = f.rest.trailing_zeros() as usize;
                    f.rest &= f.rest - 1;
                    let child = assign(f.saved, f.k, f.bit, dd);
                    insert_lane(&mut warp, lane_mask[j], &child);
                    restored = true;
                    st.nodes += 1;
                    break;
                }
                if !restored {
                    verdict = Some(false); // stack exhausted, no alt solves
                }
            } else if !changed.test(j) {
                // stuck at fixpoint with empties: branch (push clone, place first cand)
                let cur = extract_lane(&warp, j);
                let (k, bit, cand) = branch_cell(&cur);
                let dd = cand.trailing_zeros() as usize;
                let rest = cand & (cand - 1);
                stacks[j].push(Frame { saved: cur, k, bit, rest });
                let child = assign(cur, k, bit, dd);
                insert_lane(&mut warp, lane_mask[j], &child);
                st.nodes += 1;
            }
            // else: still changing, keep propagating in place

            if let Some(v) = verdict {
                out[qof[j]] = v;
                found += v as u64;
                // refill this lane from the FIFO
                if next < n {
                    insert_lane(&mut warp, lane_mask[j], &queries[next]);
                    stacks[j].clear();
                    qof[j] = next;
                    next += 1;
                    st.nodes += 1;
                } else {
                    active[j] = false;
                }
            }
        }
        active_mask = M::from_array(active);
    }
    found
}

fn main() {
    let mut attempts = 2000usize;
    let mut iters = 30usize;
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

    // ---- collect the real query FIFO from the strip loop ----
    let spec = spec_for_mode(mode);
    let baseline = spec.baseline_mask();
    let forced = spec.forced_mask();
    let mut rng = Rng::from_seed(1);
    let mut queries: Vec<Bd> = Vec::new();
    let mut reals: Vec<bool> = Vec::new(); // real dual-layout prober verdicts (soundness ref)
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
            queries.push(from_cells_restricted(&cells, i, alts));
            let real = oracle_alt_solves(&cells, i, orig);
            reals.push(real);
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
    let n = queries.len();

    let lane_mask: [M; W] = core::array::from_fn(|j| {
        let mut a = [false; W];
        a[j] = true;
        M::from_array(a)
    });
    let mut stacks: [Vec<Frame>; W] = core::array::from_fn(|_| Vec::with_capacity(64));

    // ---- soundness: scalar vs real, warp vs real, warp vs scalar ----
    let mut scalar_nodes = 0u64;
    let scalar_verdicts: Vec<bool> =
        queries.iter().map(|&b| solve_scalar_count(b, &mut scalar_nodes)).collect();
    let mut warp_verdicts = vec![false; n];
    let mut wstats = WarpStats::default();
    run_warp(&queries, &mut stacks, &lane_mask, &mut warp_verdicts, &mut wstats);

    let mut scalar_vs_real = 0u64;
    let mut warp_vs_real = 0u64;
    let mut warp_vs_scalar = 0u64;
    let mut nonunique = 0u64;
    for q in 0..n {
        if scalar_verdicts[q] != reals[q] {
            scalar_vs_real += 1;
        }
        if warp_verdicts[q] != reals[q] {
            warp_vs_real += 1;
        }
        if warp_verdicts[q] != scalar_verdicts[q] {
            warp_vs_scalar += 1;
        }
        nonunique += reals[q] as u64;
    }

    println!("killswitch: W={W}, {n} queries  (non-unique {:.1}%)", 100.0 * nonunique as f64 / n as f64);
    println!("  verdict mismatches  scalar-vs-prober {scalar_vs_real}  warp-vs-prober {warp_vs_real}  warp-vs-scalar {warp_vs_scalar}   <- all MUST be 0");
    let util = wstats.active_passes as f64 / (wstats.ticks * W as u64) as f64;
    println!(
        "  equal work: scalar nodes {scalar_nodes}  warp nodes {}  (MUST match)   util {:.1}%  ({} ticks, {:.2} nodes/query)\n",
        wstats.nodes,
        100.0 * util,
        wstats.ticks,
        wstats.nodes as f64 / n as f64,
    );

    // ---- timing ----
    let mut acc = 0u64;
    let t = Instant::now();
    for _ in 0..iters {
        for &b in &queries {
            acc = acc.wrapping_add(solve_scalar(b) as u64);
        }
    }
    let ns_s = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    let mut acc = 0u64;
    let mut throwaway = WarpStats::default();
    let t = Instant::now();
    for _ in 0..iters {
        acc = acc.wrapping_add(run_warp(&queries, &mut stacks, &lane_mask, &mut warp_verdicts, &mut throwaway));
    }
    let ns_v = t.elapsed().as_secs_f64() * 1e9 / (n * iters) as f64;
    std::hint::black_box(acc);

    println!("  scalar (banded-sl-nolc)  {ns_s:>8.2} ns/query");
    println!("  simt   (W=8 refill warp) {ns_v:>8.2} ns/query");
    println!("\n  PER-CORE prober speedup (scalar / simt): {:.2}x", ns_s / ns_v);
    println!("  [go/no-go: >~1.2x justifies the full build; <~1.2x means the residue ate the closure win]");
}
