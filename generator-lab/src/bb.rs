//! Shared bitboard core for BOTH the uniqueness prober (existence DFS) and the
//! baseline technique engine.
//!
//! Profiling the split-representation generator showed candidate propagation
//! running twice per stripped position — `place_singles` on the prober's
//! transposed bitboard AND `drain_naked_singles` on the scalar `Board` — plus a
//! `from_board` rebuild bridging the two. The fix is one representation: the
//! nine 81-bit digit boards (`board[d]` bit `c` = digit `d+1` placeable at cell
//! `c`), held in `Simd<u64, 2>` (one `v128` on wasm/ARM, one SSE2 register on
//! native). A placement is two SIMD AND-NOTs instead of 20 scalar candidate
//! writes — the win that makes the technique engine cheap.
//!
//! ## Faithfulness without step-matching
//!
//! The strip trajectory depends only on two facts from the baseline solve:
//! whether it `solved`, and whether each required technique fired at least once.
//! Both are **order-independent**: easiest-first applies every easier technique
//! to its fixpoint before a harder one is ever tried, and that fixpoint (the
//! deductive closure) is unique. So these bitboard techniques need only be
//! *sound* and *complete* (same closure as the scalar twins), NOT step-for-step
//! identical. `tests/bb_equiv.rs` cross-checks (solved, per-kind fired) against
//! the scalar engine over the real trajectory; the generator's `find 118329`
//! anchor pins it end-to-end.

use crate::grid::{BOX_UNITS, Board, COL_UNITS, CELLS, PEERS, ROW_UNITS, iter_digits};
use crate::techniques::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, LC_CLAIMING, LC_POINTING, Mask,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, Outcome,
};
use crate::util::for_each_combination;
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

type B = Simd<u64, 2>;
const ZERO: B = Simd::from_array([0, 0]);

// --- optional baseline anatomy counters (feature = "count") -------------------
// Count how often each technique is SCANNED per attempt (scan count × scan size
// ≈ cost), to see what dominates `baseline` before optimizing it.
pub const CTR_NAMES: [&str; 8] = [
    "baseline-calls", "sieve-waves", "hidden_single", "lc_pointing", "lc_claiming",
    "naked_subset", "hidden_subset", "cell_candidates",
];
#[cfg(feature = "count")]
static CTR: [core::sync::atomic::AtomicU64; 8] = [const { core::sync::atomic::AtomicU64::new(0) }; 8];
#[inline(always)]
fn bump(_i: usize) {
    #[cfg(feature = "count")]
    CTR[_i].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}
#[cfg(feature = "count")]
pub fn ctr_snapshot() -> [u64; 8] {
    core::array::from_fn(|i| CTR[i].load(core::sync::atomic::Ordering::Relaxed))
}
#[cfg(feature = "count")]
pub fn ctr_reset() {
    for a in CTR.iter() {
        a.store(0, core::sync::atomic::Ordering::Relaxed);
    }
}

const PEER_MASK: [[u64; 2]; CELLS] = build_peer_mask();
const BITS: [[u64; 2]; CELLS] = build_bits();
/// 27 units (9 rows, 9 cols, 9 boxes) as cell lists, in scan order.
const UNIT_CELLS: [[usize; 9]; 27] = build_unit_cells();
/// The same 27 units as two-lane bit masks. Rows are 0..9, cols 9..18, boxes 18..27.
const UNIT_MASK: [[u64; 2]; 27] = build_unit_masks();

const fn build_peer_mask() -> [[u64; 2]; CELLS] {
    let mut m = [[0u64; 2]; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            let p = PEERS[i][k];
            m[i][p / 64] |= 1u64 << (p % 64);
            k += 1;
        }
        i += 1;
    }
    m
}

const fn build_bits() -> [[u64; 2]; CELLS] {
    let mut b = [[0u64; 2]; CELLS];
    let mut c = 0;
    while c < CELLS {
        b[c][c / 64] = 1u64 << (c % 64);
        c += 1;
    }
    b
}

const fn build_unit_cells() -> [[usize; 9]; 27] {
    let mut u = [[0usize; 9]; 27];
    let mut i = 0;
    while i < 9 {
        u[i] = ROW_UNITS[i];
        u[9 + i] = COL_UNITS[i];
        u[18 + i] = BOX_UNITS[i];
        i += 1;
    }
    u
}

const fn build_unit_masks() -> [[u64; 2]; 27] {
    let mut m = [[0u64; 2]; 27];
    let mut u = 0;
    while u < 27 {
        let mut k = 0;
        while k < 9 {
            let c = UNIT_CELLS[u][k];
            m[u][c / 64] |= 1u64 << (c % 64);
            k += 1;
        }
        u += 1;
    }
    m
}

#[inline(always)]
fn peer_mask(c: usize) -> B {
    Simd::from_array(PEER_MASK[c])
}
#[inline(always)]
fn bit(c: usize) -> B {
    Simd::from_array(BITS[c])
}
#[inline(always)]
fn unit_mask(u: usize) -> B {
    Simd::from_array(UNIT_MASK[u])
}
#[inline(always)]
fn nonzero(x: B) -> bool {
    x.simd_ne(ZERO).any()
}
#[inline(always)]
fn popcnt(x: B) -> u32 {
    x[0].count_ones() + x[1].count_ones()
}
/// Lowest set bit's index in the 128-bit value.
#[inline(always)]
fn trailing(x: B) -> u32 {
    let lo = x[0];
    if lo != 0 {
        lo.trailing_zeros()
    } else {
        64 + x[1].trailing_zeros()
    }
}

/// The nine digit boards plus the empty-cell mask. Shared by prober and baseline.
#[derive(Clone, PartialEq)]
pub struct BitBoard {
    /// `board[d]` bit `c` set iff digit `d+1` is still placeable at empty cell `c`.
    board: [B; 9],
    /// Bit `c` set iff cell `c` is still empty.
    unsolved: B,
}

struct Sieve {
    ones: B,
    twos: B,
}

impl BitBoard {
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn from_board(b: &Board) -> Self {
        let mut board = [[0u64; 2]; 9];
        let mut unsolved = [0u64; 2];
        for c in 0..CELLS {
            if b.cell(c) == 0 {
                unsolved[c / 64] |= 1u64 << (c % 64);
                for d in iter_digits(b.candidates(c)) {
                    board[(d - 1) as usize][c / 64] |= 1u64 << (c % 64);
                }
            }
        }
        BitBoard {
            board: board.map(Simd::from_array),
            unsolved: Simd::from_array(unsolved),
        }
    }

    /// Mirror `b.clear_naked(i)` (cell `i` held digit `d0`) onto the bitboard
    /// incrementally, keeping `self == from_board(b)` without a full rebuild.
    /// A clear only (a) re-opens cell `i`'s whole candidate column and (b)
    /// restores the cleared digit `d0` to the now-unblocked empty peers — no
    /// other cell or digit moves.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_clear(&mut self, b: &Board, i: usize, d0: u8) {
        let ib = bit(i);
        // (a) cell i: filled -> empty, column = its naked candidates.
        self.unsolved |= ib;
        let cand = b.candidates(i);
        for e in 0..9 {
            if cand & (1 << e) != 0 {
                self.board[e] |= ib;
            } else {
                self.board[e] &= !ib;
            }
        }
        // (b) d0 was blocked at every peer by cell i, so its bit there was 0;
        // set it on peers that are now empty and can take d0 (pure OR).
        let db = 1u16 << (d0 - 1);
        let mut add = ZERO;
        for &p in &PEERS[i] {
            if b.cell(p) == 0 && b.candidates(p) & db != 0 {
                add |= bit(p);
            }
        }
        self.board[(d0 - 1) as usize] |= add;
    }

    /// Mirror `b.place(i, d0)` (the strip's revert) onto the bitboard: cell `i`
    /// goes empty -> filled (column cleared) and `d0` leaves every peer.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_place(&mut self, i: usize, d0: u8) {
        let ib = bit(i);
        self.unsolved &= !ib;
        for e in 0..9 {
            self.board[e] &= !ib;
        }
        self.board[(d0 - 1) as usize] &= !peer_mask(i);
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell, forbid `d` on peers.
    #[inline(always)]
    fn place(&mut self, c: usize, d: u8) {
        self.unsolved &= !bit(c);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.board.get_unchecked_mut((d - 1) as usize) &= !peer_mask(c);
        }
    }

    /// Candidate digit bitmask (`1 << (digit-1)`) of cell `c`, from the boards.
    #[inline]
    fn cell_candidates(&self, c: usize) -> u16 {
        bump(7);
        let cb = bit(c);
        let mut m = 0u16;
        for d in 0..9 {
            if nonzero(self.board[d] & cb) {
                m |= 1 << d;
            }
        }
        m
    }

    // --- prober (existence DFS) -------------------------------------------

    #[inline(always)]
    fn sieve(&self) -> Sieve {
        let mut ones = ZERO;
        let mut twos = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            twos |= ones & b;
            ones |= b;
        }
        Sieve { ones, twos }
    }

    #[inline]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let group = singles & unsafe { *self.board.get_unchecked(d) };
            if !nonzero(group) {
                continue;
            }
            let mut peers = ZERO;
            for lane in 0..2 {
                let mut g = group[lane];
                let base = lane * 64;
                while g != 0 {
                    let c = base + g.trailing_zeros() as usize;
                    g &= g - 1;
                    if nonzero(peers & bit(c)) {
                        return false;
                    }
                    peers |= peer_mask(c);
                }
            }
            self.unsolved &= !group;
            self.board[d] &= !peers;
        }
        true
    }

    #[inline]
    fn branch_cell(&self) -> (usize, u16) {
        let mut ones = ZERO;
        let mut twos = ZERO;
        let mut threes = ZERO;
        for d in 0..9 {
            // SAFETY: d in 0..9.
            let b = unsafe { *self.board.get_unchecked(d) };
            threes |= twos & b;
            twos |= ones & b;
            ones |= b;
        }
        let bivalue = self.unsolved & twos & !threes;
        let pick = if nonzero(bivalue) { bivalue } else { self.unsolved };
        let c = trailing(pick) as usize;
        let cb = bit(c);
        let mut mask: u16 = 0;
        for d in 0..9 {
            if nonzero(self.board[d] & cb) {
                mask |= 1 << d;
            }
        }
        (c, mask)
    }

    /// True iff the grid has at least one completion.
    fn solve_first(&mut self) -> bool {
        loop {
            let Sieve { ones, twos } = self.sieve();
            if nonzero(self.unsolved & !ones) {
                return false;
            }
            if !nonzero(self.unsolved) {
                return true;
            }
            let singles = self.unsolved & ones & !twos;
            if nonzero(singles) {
                if !self.place_singles(singles) {
                    return false;
                }
                continue;
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
                let mut child = self.clone();
                child.place(cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }

    /// Uniqueness gate: restrict cell `i` to the alternate digits `alts` and ask
    /// whether any completion exists — i.e. stripping `i` made the puzzle
    /// non-unique. Consumes a clone, so the base is untouched for the baseline.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn any_alt_solves(&self, i: usize, alts: u16) -> bool {
        let mut s = self.clone();
        let keep = bit(i);
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.board[d] &= !keep;
            }
        }
        s.solve_first()
    }

    // --- baseline technique engine ----------------------------------------

    /// Solve with the `allowed` toolbox, easiest-first, tallying which kinds
    /// fired. Naked singles drain in bit-parallel waves (the hot part); the
    /// rarer harder techniques apply one step at a time. Clones the base, so the
    /// prober can reuse it.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn baseline(&self, allowed: Mask) -> Outcome {
        bump(0);
        let mut bb = self.clone();
        let mut counts = [0u16; NUM];
        let ns = allowed & (1 << NAKED_SINGLE) != 0;
        loop {
            if ns {
                let n = bb.drain_naked_singles();
                if n > 0 {
                    counts[NAKED_SINGLE] = counts[NAKED_SINGLE].saturating_add(n);
                    continue;
                }
            }
            if !nonzero(bb.unsolved) {
                return Outcome { solved: true, counts };
            }
            match bb.step_harder(allowed) {
                Some(k) => counts[k] = counts[k].saturating_add(1),
                None => return Outcome { solved: false, counts },
            }
        }
    }

    /// Place every naked single (cell with exactly one candidate) in waves until
    /// none remain; returns how many were placed. Popcount-free detection via the
    /// sieve; bit-parallel placement via `place_singles`.
    #[inline]
    fn drain_naked_singles(&mut self) -> u16 {
        let mut total = 0u16;
        loop {
            bump(1);
            let Sieve { ones, twos } = self.sieve();
            let singles = self.unsolved & ones & !twos;
            if !nonzero(singles) {
                return total;
            }
            let n = popcnt(singles) as u16;
            // On a uniquely-solvable board (the only kind baseline sees) naked
            // singles are forced moves and never conflict, so place_singles holds.
            if !self.place_singles(singles) {
                return total;
            }
            total = total.saturating_add(n);
        }
    }

    /// Apply the first applicable harder technique (hidden single up to hidden
    /// quad, in difficulty order, gated by `allowed`); return its kind index.
    fn step_harder(&mut self, allowed: Mask) -> Option<usize> {
        if allowed & (1 << HIDDEN_SINGLE) != 0 && self.hidden_single() {
            return Some(HIDDEN_SINGLE);
        }
        if allowed & (1 << LC_POINTING) != 0 && self.lc_pointing() {
            return Some(LC_POINTING);
        }
        if allowed & (1 << LC_CLAIMING) != 0 && self.lc_claiming() {
            return Some(LC_CLAIMING);
        }
        if allowed & (1 << NAKED_PAIR) != 0 && self.naked_subset(2) {
            return Some(NAKED_PAIR);
        }
        if allowed & (1 << HIDDEN_PAIR) != 0 && self.hidden_subset(2) {
            return Some(HIDDEN_PAIR);
        }
        if allowed & (1 << NAKED_TRIPLE) != 0 && self.naked_subset(3) {
            return Some(NAKED_TRIPLE);
        }
        if allowed & (1 << HIDDEN_TRIPLE) != 0 && self.hidden_subset(3) {
            return Some(HIDDEN_TRIPLE);
        }
        if allowed & (1 << NAKED_QUAD) != 0 && self.naked_subset(4) {
            return Some(NAKED_QUAD);
        }
        if allowed & (1 << HIDDEN_QUAD) != 0 && self.hidden_subset(4) {
            return Some(HIDDEN_QUAD);
        }
        None
    }

    /// Hidden single: a digit with exactly one placeable empty cell in a unit.
    /// Precompute `board[d] & unsolved` once per digit (9 ANDs) so the 27×9 unit
    /// scan is one AND + popcount per pair instead of two ANDs. Valid because we
    /// return at the first placement, before any board mutation.
    fn hidden_single(&mut self) -> bool {
        bump(2);
        let mut bd = [ZERO; 9];
        for d in 0..9 {
            bd[d] = self.board[d] & self.unsolved;
        }
        for u in 0..27 {
            let um = unit_mask(u);
            for d in 0..9 {
                let pos = bd[d] & um;
                if popcnt(pos) == 1 {
                    let c = trailing(pos) as usize;
                    self.place(c, d as u8 + 1);
                    return true;
                }
            }
        }
        false
    }

    /// Locked candidates (pointing): a digit confined to one line within a box
    /// is eliminated from the rest of that line.
    fn lc_pointing(&mut self) -> bool {
        bump(3);
        for b in 0..9 {
            let bm = unit_mask(18 + b);
            let br = (b / 3) * 3;
            let bc = (b % 3) * 3;
            for d in 0..9 {
                let pos = self.board[d] & bm & self.unsolved;
                if popcnt(pos) < 2 {
                    continue;
                }
                // rows of this box
                for r in br..br + 3 {
                    let rm = unit_mask(r);
                    if nonzero(pos & !rm) {
                        continue; // not all in this row
                    }
                    let targets = self.board[d] & rm & !bm & self.unsolved;
                    if nonzero(targets) {
                        self.board[d] &= !targets;
                        return true;
                    }
                }
                // cols of this box
                for cc in bc..bc + 3 {
                    let cm = unit_mask(9 + cc);
                    if nonzero(pos & !cm) {
                        continue;
                    }
                    let targets = self.board[d] & cm & !bm & self.unsolved;
                    if nonzero(targets) {
                        self.board[d] &= !targets;
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Locked candidates (claiming): a digit confined to one box within a line
    /// is eliminated from the rest of that box.
    fn lc_claiming(&mut self) -> bool {
        bump(4);
        for line in 0..18 {
            // 0..9 rows, 9..18 cols
            let lm = unit_mask(line);
            for d in 0..9 {
                let pos = self.board[d] & lm & self.unsolved;
                if popcnt(pos) < 2 {
                    continue;
                }
                let first = trailing(pos) as usize;
                let b = (first / 9 / 3) * 3 + (first % 9) / 3;
                let bm = unit_mask(18 + b);
                if nonzero(pos & !bm) {
                    continue; // not all in one box
                }
                let targets = self.board[d] & bm & !lm & self.unsolved;
                if nonzero(targets) {
                    self.board[d] &= !targets;
                    return true;
                }
            }
        }
        false
    }

    /// Naked subset of `size`: `size` cells in a unit whose candidate union is
    /// exactly `size` digits — those digits leave the other cells of the unit.
    fn naked_subset(&mut self, size: usize) -> bool {
        bump(5);
        for u in 0..27 {
            let cells = UNIT_CELLS[u];
            // candidate cells: empty, 2..=size candidates.
            let mut cand_cells: [usize; 9] = [0; 9];
            let mut cand_masks: [u16; 9] = [0; 9];
            let mut n = 0usize;
            for &c in &cells {
                if nonzero(self.unsolved & bit(c)) {
                    let m = self.cell_candidates(c);
                    let pc = m.count_ones() as usize;
                    if pc >= 2 && pc <= size {
                        cand_cells[n] = c;
                        cand_masks[n] = m;
                        n += 1;
                    }
                }
            }
            if n < size {
                continue;
            }
            let idx: [usize; 9] = core::array::from_fn(|k| k);
            let mut applied = false;
            for_each_combination(&idx[..n], size, |combo| {
                let union: u16 = combo.iter().map(|&k| cand_masks[k]).fold(0, |a, x| a | x);
                if union.count_ones() as usize != size {
                    return true; // keep searching
                }
                // eliminate union digits from the OTHER cells of the unit.
                let mut did = false;
                for &c in &cells {
                    if !nonzero(self.unsolved & bit(c)) {
                        continue;
                    }
                    if combo.iter().any(|&k| cand_cells[k] == c) {
                        continue;
                    }
                    let rm = self.cell_candidates(c) & union;
                    if rm != 0 {
                        for d in 0..9 {
                            if rm & (1 << d) != 0 {
                                self.board[d] &= !bit(c);
                            }
                        }
                        did = true;
                    }
                }
                applied = did;
                !did // stop once we eliminated something
            });
            if applied {
                return true;
            }
        }
        false
    }

    /// Hidden subset of `size`: `size` digits confined to the same `size` cells
    /// of a unit — the other digits leave those cells.
    fn hidden_subset(&mut self, size: usize) -> bool {
        bump(6);
        for u in 0..27 {
            let cells = UNIT_CELLS[u];
            // position mask (over the 9 unit-cell indices) per digit.
            let mut pos: [u16; 9] = [0; 9];
            let mut digits: [usize; 9] = [0; 9];
            let mut n = 0usize;
            for d in 0..9 {
                let mut p: u16 = 0;
                for (k, &c) in cells.iter().enumerate() {
                    if nonzero(self.board[d] & bit(c) & self.unsolved) {
                        p |= 1 << k;
                    }
                }
                let pc = p.count_ones() as usize;
                if pc >= 2 && pc <= size {
                    pos[d] = p;
                    digits[n] = d;
                    n += 1;
                }
            }
            if n < size {
                continue;
            }
            let idx: [usize; 9] = core::array::from_fn(|k| k);
            let mut applied = false;
            for_each_combination(&idx[..n], size, |combo| {
                let union: u16 = combo.iter().map(|&k| pos[digits[k]]).fold(0, |a, x| a | x);
                if union.count_ones() as usize != size {
                    return true;
                }
                let keep: u16 = combo.iter().map(|&k| 1u16 << digits[k]).fold(0, |a, x| a | x);
                let mut did = false;
                for k in 0..9 {
                    if union & (1 << k) == 0 {
                        continue;
                    }
                    let c = cells[k];
                    let rm = self.cell_candidates(c) & !keep;
                    if rm != 0 {
                        for d in 0..9 {
                            if rm & (1 << d) != 0 {
                                self.board[d] &= !bit(c);
                            }
                        }
                        did = true;
                    }
                }
                applied = did;
                !did
            });
            if applied {
                return true;
            }
        }
        false
    }
}
