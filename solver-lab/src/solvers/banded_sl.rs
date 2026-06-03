//! Variant `banded-sl`: a SINGLE-LAYOUT fork of [`banded`](super::banded) — the
//! row-major view only, no column-major copy.
//!
//! Purpose: settle, on FAIR ground, whether dropping the column view helps the
//! *prober in isolation*. In `generator-lab` the prober and baseline share one
//! dual-view `BitBoard`, so a single-layout cfg there changes the baseline too
//! (it stalls into the subset ladder) and the result is confounded. Here the
//! baseline is the separate scalar `baseline_solvable` gate, held fixed across all
//! variants, so `banded` vs `banded-sl` isolates exactly the column view's net
//! value in the existence search: it drops column hidden singles and box↔column
//! (claiming) LC, plus all `c`/`unsolved_c` maintenance (half the `place` work and
//! half the per-branch clone), against more DFS nodes from the lost pruning.
//!
//! Everything else is identical to `banded` (same tables, per-cell `place_singles`,
//! row-major LC via the `rmask` half of `RM_LC_TRIP`, recursive `solve_first`), so
//! the delta is the layout alone. Correctness is pinned by the oracle cross-check.

use crate::grid::{Board, CELLS, PEERS};
use crate::solvers::UniqProber;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

type B = Simd<u32, 4>;
const ZERO: B = Simd::from_array([0, 0, 0, 0]);

const fn rm_lane(cell: usize) -> usize {
    (cell / 9) / 3
}
const fn rm_bit(cell: usize) -> usize {
    ((cell / 9) % 3) * 9 + cell % 9
}
#[inline(always)]
fn rm_cell(lane: usize, bit: u32) -> usize {
    let b = bit as usize;
    (3 * lane + b / 9) * 9 + b % 9
}

const BITS_R: [[u32; 4]; CELLS] = build_bits();
const PEER_MASK_R: [[u32; 4]; CELLS] = build_peer_mask();

const fn build_bits() -> [[u32; 4]; CELLS] {
    let mut b = [[0u32; 4]; CELLS];
    let mut c = 0;
    while c < CELLS {
        b[c][rm_lane(c)] = 1u32 << rm_bit(c);
        c += 1;
    }
    b
}

const fn build_peer_mask() -> [[u32; 4]; CELLS] {
    let mut m = [[0u32; 4]; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            let p = PEERS[i][k];
            m[i][rm_lane(p)] |= 1u32 << rm_bit(p);
            k += 1;
        }
        i += 1;
    }
    m
}

const BAND_KEEP_OCC: [u32; 512] = build_band_keep();

const fn build_band_keep() -> [u32; 512] {
    let mut t = [0u32; 512];
    let mut occ = 0usize;
    while occ < 512 {
        let mut keep = occ as u32;
        loop {
            let mut next = keep;
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

/// Row-major cell mask of the triplet dropped by row-major LC (the `rmask` half of
/// `banded`'s `RM_LC_TRIP` — the column-major mirror is not needed single-layout).
const RM_LC_TRIP_R: [[[u32; 4]; 9]; 3] = build_lc_trip();

const fn build_lc_trip() -> [[[u32; 4]; 9]; 3] {
    let mut out = [[[0u32; 4]; 9]; 3];
    let mut b = 0;
    while b < 3 {
        let mut t = 0;
        while t < 9 {
            let a = t / 3;
            let g = t % 3;
            let mut rmask = [0u32; 4];
            rmask[b] = 0b111 << (a * 9 + 3 * g);
            out[b][t] = rmask;
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
fn peer_mask_r(c: usize) -> B {
    Simd::from_array(PEER_MASK_R[c])
}
#[inline(always)]
fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}
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

/// Single-layout board: nine row-major digit boards + the empty-cell mask.
#[derive(Clone)]
struct BitBoard {
    r: [B; 9],
    unsolved_r: B,
}

struct Sieve {
    ones: B,
    twos: B,
}

enum Prop {
    Solved,
    Contradiction,
    Stuck,
}

impl BitBoard {
    fn from_board_candidates(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        let mut ur = [0u32; 4];
        for cell in 0..CELLS {
            if b.cell(cell) == 0 {
                ur[rm_lane(cell)] |= 1u32 << rm_bit(cell);
                let mut m = b.candidates(cell);
                while m != 0 {
                    let d = m.trailing_zeros() as usize;
                    m &= m - 1;
                    r[d][rm_lane(cell)] |= 1u32 << rm_bit(cell);
                }
            }
        }
        BitBoard { r: r.map(Simd::from_array), unsolved_r: Simd::from_array(ur) }
    }

    #[inline(always)]
    fn place(&mut self, cell: usize, d: u8) {
        crate::counters::bump_placements();
        self.unsolved_r &= !bit_r(cell);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.r.get_unchecked_mut((d - 1) as usize) &= !peer_mask_r(cell);
        }
    }

    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        for d in 0..9 {
            let b = unsafe { *self.r.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
    }

    #[inline]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            let group = singles & unsafe { *self.r.get_unchecked(d) };
            if !nonzero(group) {
                continue;
            }
            let mut peers_r = ZERO;
            for lane in 0..3 {
                let mut g = group[lane];
                while g != 0 {
                    let cell = rm_cell(lane, g.trailing_zeros());
                    g &= g - 1;
                    peers_r |= peer_mask_r(cell);
                }
            }
            if nonzero(peers_r & group) {
                return false;
            }
            self.unsolved_r &= !group;
            self.r[d] &= !peers_r;
        }
        true
    }

    /// Row-major band update: locked candidates (box↔row only — no column copy)
    /// and hidden singles in rows + boxes.
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
                        self.r[d] &= !Simd::from_array(RM_LC_TRIP_R[b][t]);
                    }
                    live = (self.r[d] & self.unsolved_r)[b];
                }
                for rr in 0..3 {
                    let s = SINGLE9[((live >> (9 * rr)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(rm_cell(b, 9 * rr + s as u32), d as u8 + 1);
                        changed = true;
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
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
            if !self.band_update_rm() {
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

/// `banded-sl` existence probe.
pub struct Probe {
    base: BitBoard,
}

impl UniqProber for Probe {
    const NAME: &'static str = "banded-sl";

    fn from_board(board: &Board) -> Self {
        crate::counters::bump_build();
        Probe { base: BitBoard::from_board_candidates(board) }
    }

    #[inline]
    fn has_solution_with(&mut self, i: usize, d: u8) -> bool {
        crate::counters::bump_clones();
        crate::counters::bump_entry_clone();
        let mut s = self.base.clone();
        s.place(i, d);
        s.solve_first()
    }

    fn any_alt_solves(&mut self, i: usize, alts: crate::grid::Mask) -> bool {
        crate::counters::bump_clones();
        let mut s = self.base.clone();
        let kr = bit_r(i);
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
            }
        }
        s.solve_first()
    }
}
