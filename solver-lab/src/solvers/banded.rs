//! Variant `banded`: the dual-banded existence prober ported from
//! `generator-lab`'s `bb.rs` (the recently-rewritten + optimized banded solver).
//!
//! Backported here so we can settle, with numbers, whether the optimized banded
//! engine actually beats the standing champ [`bitboard-simd`](super::bitboard_simd)
//! on the same strip-loop workload — rather than trusting a stale claim either
//! way.
//!
//! ## Representation
//!
//! A **band** is 3 lines = 27 cells, stored in lanes 0/1/2 of a `Simd<u32, 4>`
//! (lane 3 unused). Candidates are kept in two transposed copies so every unit is
//! in-lane in at least one view:
//! - `r[d]` — row-major bands (lane `= row/3`, bit `= (row%3)*9 + col`): rows and
//!   boxes in-lane.
//! - `c[d]` — column-major bands (lane `= col/3`, bit `= (col%3)*9 + row`):
//!   columns and boxes in-lane.
//!
//! That makes hidden singles and both orientations of locked candidates cheap and
//! popcount-free, driven off the `BAND_KEEP_OCC` / `SINGLE9` tables. Only the
//! existence-DFS half of `bb.rs` is ported (the baseline technique engine lives in
//! the shared `baseline_solvable` gate, not the prober).

use crate::grid::{Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

/// One v128 holds a digit's three 27-bit bands in lanes 0/1/2; lane 3 stays zero.
type B = Simd<u32, 4>;
const ZERO: B = Simd::from_array([0, 0, 0, 0]);

// --- layout maps: cell <-> (lane, bit) for each banding -----------------------
const fn rm_lane(cell: usize) -> usize {
    (cell / 9) / 3
}
const fn rm_bit(cell: usize) -> usize {
    ((cell / 9) % 3) * 9 + cell % 9
}
const fn cm_lane(cell: usize) -> usize {
    (cell % 9) / 3
}
const fn cm_bit(cell: usize) -> usize {
    ((cell % 9) % 3) * 9 + cell / 9
}
/// Inverse of (`rm_lane`, `rm_bit`): the cell at row-major position (lane, bit).
#[inline(always)]
fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}
/// Inverse of (`cm_lane`, `cm_bit`): the cell at column-major position (lane, bit).
#[inline(always)]
fn cm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (b % 9) * 9 + (3 * lane + b / 9)
}

const BITS_R: [[u32; 4]; CELLS] = build_bits(true);
const BITS_C: [[u32; 4]; CELLS] = build_bits(false);
const PEER_MASK_R: [[u32; 4]; CELLS] = build_peer_mask(true);
const PEER_MASK_C: [[u32; 4]; CELLS] = build_peer_mask(false);

const fn build_bits(row_major: bool) -> [[u32; 4]; CELLS] {
    let mut b = [[0u32; 4]; CELLS];
    let mut c = 0;
    while c < CELLS {
        let (lane, bit) = if row_major { (rm_lane(c), rm_bit(c)) } else { (cm_lane(c), cm_bit(c)) };
        b[c][lane] = 1u32 << bit;
        c += 1;
    }
    b
}

const fn build_peer_mask(row_major: bool) -> [[u32; 4]; CELLS] {
    let mut m = [[0u32; 4]; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            let p = PEERS[i][k];
            let (lane, bit) = if row_major { (rm_lane(p), rm_bit(p)) } else { (cm_lane(p), cm_bit(p)) };
            m[i][lane] |= 1u32 << bit;
            k += 1;
        }
        i += 1;
    }
    m
}

/// Within-band locked-candidates self-elimination, fully precomputed. See
/// `generator-lab/src/bb.rs` for the derivation. `BAND_KEEP_OCC[occ]` is the
/// surviving 9-bit triplet occupancy after the within-band LC fixpoint.
const BAND_KEEP_OCC: [u32; 512] = build_band_keep();

const fn build_band_keep() -> [u32; 512] {
    let mut t = [0u32; 512];
    let mut occ = 0usize;
    while occ < 512 {
        let mut keep = occ as u32; // 9-bit triplet occupancy
        loop {
            let mut next = keep;
            // pointing: box-column k whose occupied triplets are a single band-row.
            let mut k = 0;
            while k < 3 {
                let r0 = (next >> k) & 1;
                let r1 = (next >> (3 + k)) & 1;
                let r2 = (next >> (6 + k)) & 1;
                if r0 + r1 + r2 == 1 {
                    let r = if r0 == 1 { 0 } else if r1 == 1 { 1 } else { 2 };
                    let mut kk = 0;
                    while kk < 3 {
                        if kk != k {
                            next &= !(1 << (r * 3 + kk));
                        }
                        kk += 1;
                    }
                }
                k += 1;
            }
            // claiming: band-row r whose occupied triplets are a single box-column.
            let mut r = 0;
            while r < 3 {
                let c0 = (next >> (r * 3)) & 1;
                let c1 = (next >> (r * 3 + 1)) & 1;
                let c2 = (next >> (r * 3 + 2)) & 1;
                if c0 + c1 + c2 == 1 {
                    let k = if c0 == 1 { 0 } else if c1 == 1 { 1 } else { 2 };
                    let mut rr = 0;
                    while rr < 3 {
                        if rr != r {
                            next &= !(1 << (rr * 3 + k));
                        }
                        rr += 1;
                    }
                }
                r += 1;
            }
            if next == keep {
                break;
            }
            keep = next;
        }
        t[occ] = keep;
        occ += 1;
    }
    t
}

/// Hidden-single lookup for a 9-bit unit candidate mask: the lone bit's index if
/// exactly one is set, else `0xFF`.
const SINGLE9: [u8; 512] = build_single9();

const fn build_single9() -> [u8; 512] {
    let mut t = [0xFFu8; 512];
    let mut v = 1usize;
    while v < 512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros() as u8;
        }
        v += 1;
    }
    t
}

/// For a triplet dropped by row-major LC — band `b`, triplet `t = 3*r + k` — the
/// cell masks to clear in BOTH views. See `bb.rs`.
const RM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(true);
/// The col-major analogue: band `b` (column-stack), triplet `t = 3*cc + br`.
const CM_LC_TRIP: [[([u32; 4], [u32; 4]); 9]; 3] = build_lc_trip(false);

const fn build_lc_trip(row_major: bool) -> [[([u32; 4], [u32; 4]); 9]; 3] {
    let mut out = [[([0u32; 4], [0u32; 4]); 9]; 3];
    let mut b = 0;
    while b < 3 {
        let mut t = 0;
        while t < 9 {
            let a = t / 3; // band-row (rm) or col-within-stack (cm)
            let g = t % 3; // box-col (rm) or box-row (cm)
            let mut rmask = [0u32; 4];
            let mut cmask = [0u32; 4];
            if row_major {
                rmask[b] = 0b111 << (a * 9 + 3 * g);
                let big_r = 3 * b + a;
                cmask[g] = (1 << big_r) | (1 << (9 + big_r)) | (1 << (18 + big_r));
            } else {
                cmask[b] = 0b111 << (a * 9 + 3 * g);
                let big_c = 3 * b + a;
                rmask[g] = (1 << big_c) | (1 << (9 + big_c)) | (1 << (18 + big_c));
            }
            out[b][t] = (rmask, cmask);
            t += 1;
        }
        b += 1;
    }
    out
}

#[inline(always)]
fn bit_r(c: usize) -> B {
    Simd::from_array(BITS_R[c])
}
#[inline(always)]
fn bit_c(c: usize) -> B {
    Simd::from_array(BITS_C[c])
}
#[inline(always)]
fn peer_mask_r(c: usize) -> B {
    Simd::from_array(PEER_MASK_R[c])
}
#[inline(always)]
fn peer_mask_c(c: usize) -> B {
    Simd::from_array(PEER_MASK_C[c])
}
#[inline(always)]
fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}
/// Cell of the lowest set bit in a row-major value.
#[inline(always)]
fn first_rm(x: B) -> usize {
    if x[0] != 0 {
        rm_cell(0, x[0].trailing_zeros())
    } else if x[1] != 0 {
        rm_cell(1, x[1].trailing_zeros())
    } else {
        rm_cell(2, x[2].trailing_zeros())
    }
}
/// 9-bit triplet occupancy of one band's 27-bit candidate set.
#[inline(always)]
fn triplet_occ(m: u32) -> usize {
    let s = m | (m >> 1) | (m >> 2);
    let occ = (s & 1)
        | ((s >> 3) & 1) << 1
        | ((s >> 6) & 1) << 2
        | ((s >> 9) & 1) << 3
        | ((s >> 12) & 1) << 4
        | ((s >> 15) & 1) << 5
        | ((s >> 18) & 1) << 6
        | ((s >> 21) & 1) << 7
        | ((s >> 24) & 1) << 8;
    occ as usize
}

/// The nine digit boards in both bandings plus the empty-cell mask in both.
#[derive(Clone)]
struct BitBoard {
    r: [B; 9],
    c: [B; 9],
    unsolved_r: B,
    unsolved_c: B,
}

struct Sieve {
    ones: B,
    twos: B,
}

/// Why band propagation stopped.
enum Prop {
    Solved,
    Contradiction,
    Stuck,
}

impl BitBoard {
    fn from_board_candidates(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        let mut c = [[0u32; 4]; 9];
        let mut ur = [0u32; 4];
        let mut uc = [0u32; 4];
        for cell in 0..CELLS {
            if b.cell(cell) == 0 {
                ur[rm_lane(cell)] |= 1u32 << rm_bit(cell);
                uc[cm_lane(cell)] |= 1u32 << cm_bit(cell);
                let mut m = b.candidates(cell);
                while m != 0 {
                    let d = m.trailing_zeros() as usize;
                    m &= m - 1;
                    r[d][rm_lane(cell)] |= 1u32 << rm_bit(cell);
                    c[d][cm_lane(cell)] |= 1u32 << cm_bit(cell);
                }
            }
        }
        BitBoard {
            r: r.map(Simd::from_array),
            c: c.map(Simd::from_array),
            unsolved_r: Simd::from_array(ur),
            unsolved_c: Simd::from_array(uc),
        }
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell in both views, forbid
    /// `d` on its peers in both views.
    #[inline(always)]
    fn place(&mut self, cell: usize, d: u8) {
        crate::counters::bump_placements();
        self.unsolved_r &= !bit_r(cell);
        self.unsolved_c &= !bit_c(cell);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.r.get_unchecked_mut((d - 1) as usize) &= !peer_mask_r(cell);
            *self.c.get_unchecked_mut((d - 1) as usize) &= !peer_mask_c(cell);
        }
    }

    /// Naked-single sieve over the row-major boards.
    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.r.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
    }

    /// Place a wave of naked singles (cells `singles`) into both views. Returns
    /// false if two singles of the same digit are peers (a contradiction).
    #[inline]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let group = singles & unsafe { *self.r.get_unchecked(d) };
            if !nonzero(group) {
                continue;
            }
            let mut peers_r = ZERO;
            let mut peers_c = ZERO;
            let mut group_c = ZERO;
            for lane in 0..3 {
                let mut g = group[lane];
                while g != 0 {
                    let cell = rm_cell(lane, g.trailing_zeros());
                    g &= g - 1;
                    peers_r |= peer_mask_r(cell);
                    peers_c |= peer_mask_c(cell);
                    group_c |= bit_c(cell);
                }
            }
            if nonzero(peers_r & group) {
                return false;
            }
            self.unsolved_r &= !group;
            self.unsolved_c &= !group_c;
            self.r[d] &= !peers_r;
            self.c[d] &= !peers_c;
        }
        true
    }

    /// Fused row-major band update: locked candidates (box<->row) and hidden
    /// singles (rows + boxes) off each band value, via `BAND_KEEP_OCC`/`SINGLE9`.
    fn band_update_rm(&mut self) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                let mut live = (self.r[d] & self.unsolved_r)[b];
                let occ = triplet_occ(live);
                let mut dropped = occ as u32 & !BAND_KEEP_OCC[occ];
                if dropped != 0 {
                    changed = true;
                    while dropped != 0 {
                        let t = dropped.trailing_zeros() as usize;
                        dropped &= dropped - 1;
                        let (rm, cm) = RM_LC_TRIP[b][t];
                        self.r[d] &= !Simd::from_array(rm);
                        self.c[d] &= !Simd::from_array(cm);
                    }
                    live = (self.r[d] & self.unsolved_r)[b];
                }
                // Hidden singles in the three rows (each a contiguous 9-bit chunk).
                for rr in 0..3 {
                    let s = SINGLE9[((live >> (9 * rr)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(rm_cell(b, 9 * rr + s as u32), d as u8 + 1);
                        changed = true;
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
                // Hidden singles in the three boxes (gather each box's 9 bits).
                for k in 0..3 {
                    let bk = ((live >> (3 * k)) & 7)
                        | (((live >> (9 + 3 * k)) & 7) << 3)
                        | (((live >> (18 + 3 * k)) & 7) << 6);
                    let s = SINGLE9[bk as usize] as usize;
                    if s != 0xFF {
                        let bit = (s / 3) * 9 + 3 * k as usize + s % 3;
                        self.place(rm_cell(b, bit as u32), d as u8 + 1);
                        changed = true;
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
            }
        }
        changed
    }

    /// Fused column-major band update: box<->column LC and hidden singles in
    /// columns. Boxes are already covered row-major, so only columns are swept.
    fn band_update_cm(&mut self) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                let mut live = (self.c[d] & self.unsolved_c)[b];
                let occ = triplet_occ(live);
                let mut dropped = occ as u32 & !BAND_KEEP_OCC[occ];
                if dropped != 0 {
                    changed = true;
                    while dropped != 0 {
                        let t = dropped.trailing_zeros() as usize;
                        dropped &= dropped - 1;
                        let (rm, cm) = CM_LC_TRIP[b][t];
                        self.r[d] &= !Simd::from_array(rm);
                        self.c[d] &= !Simd::from_array(cm);
                    }
                    live = (self.c[d] & self.unsolved_c)[b];
                }
                for cc in 0..3 {
                    let s = SINGLE9[((live >> (9 * cc)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(cm_cell(b, 9 * cc + s as u32), d as u8 + 1);
                        changed = true;
                        live = (self.c[d] & self.unsolved_c)[b];
                    }
                }
            }
        }
        changed
    }

    /// Run all band propagation to a fixpoint.
    fn propagate(&mut self) -> Prop {
        loop {
            loop {
                let Sieve { ones, twos } = self.sieve();
                if nonzero(self.unsolved_r & !ones) {
                    return Prop::Contradiction;
                }
                if !nonzero(self.unsolved_r) {
                    return Prop::Solved;
                }
                let singles = self.unsolved_r & ones & !twos;
                if !nonzero(singles) {
                    break;
                }
                if !self.place_singles(singles) {
                    return Prop::Contradiction;
                }
            }
            let mut changed = self.band_update_rm();
            changed |= self.band_update_cm();
            if !changed {
                return Prop::Stuck;
            }
        }
    }

    #[inline]
    fn branch_cell(&self) -> (usize, u16) {
        let mut ones = ZERO;
        let mut twos = ZERO;
        let mut threes = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.r.get_unchecked(d) };
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        let bivalue = self.unsolved_r & twos & !threes;
        let pick = if nonzero(bivalue) { bivalue } else { self.unsolved_r };
        let cell = first_rm(pick);
        let cb = bit_r(cell);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if nonzero(self.r[d] & cb) {
                mask |= 1 << d;
            }
        }
        (cell, mask)
    }

    /// True iff the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            match self.propagate() {
                Prop::Solved => return true,
                Prop::Contradiction => return false,
                Prop::Stuck => {}
            }
            let (cell, mask) = self.branch_cell();
            let mut m = mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                if m == 0 {
                    self.place(cell, d);
                    break;
                }
                crate::counters::bump_clones();
                let mut child = self.clone();
                child.place(cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }
}

/// `banded` existence probe.
pub struct Probe {
    base: BitBoard,
}

impl UniqProber for Probe {
    const NAME: &'static str = "banded";

    fn from_board(board: &Board) -> Self {
        crate::counters::bump_build();
        Probe {
            base: BitBoard::from_board_candidates(board),
        }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        crate::counters::bump_clones();
        crate::counters::bump_entry_clone();
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }

    /// Override mirroring `generator-lab`'s banded `any_alt_solves`: restrict cell
    /// `i` to the alternate digits in one clone and ask for any completion in a
    /// single solve (the solver branches over the alts internally). Boolean-equal
    /// to the per-digit default loop, which is what `find 118329` / the oracle pin.
    fn any_alt_solves(&mut self, i: usize, alts: crate::grid::Mask) -> bool {
        crate::counters::bump_clones();
        let mut s = self.base.clone();
        let (kr, kc) = (bit_r(i), bit_c(i));
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
                s.c[d] &= !kc;
            }
        }
        s.solve_first()
    }
}
