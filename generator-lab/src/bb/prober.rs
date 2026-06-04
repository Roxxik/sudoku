//! The lean single-layout existence prober — the uniqueness gate's own board,
//! deliberately NOT a [`BitBoard`]. See [`ProberBoard`] for why it drops the
//! column view and locked candidates, and [`super`] for the dual-view baseline it
//! is split from. The `BitBoard` methods that *enter* the prober
//! ([`BitBoard::any_alt_solves`] and the throwaway closure diagnostics) live here
//! too, since they build a `ProberBoard` from a `BitBoard`'s row-major half.

use super::layout::{B, SINGLE9, ZERO, bit_r, first_rm, nonzero, peer_mask_r, popcnt, rm_cell};
use super::{BitBoard, Prop, Sieve, band_ctr_inc, pbump, sieve};
#[cfg(feature = "count")]
use super::{ALT_STATS, AltStat, PCTR};

/// Lean **single-layout** existence prober — the uniqueness gate's own board,
/// deliberately NOT a [`BitBoard`].
///
/// The prober is a pure existence oracle: it only answers *does a completion
/// exist*, never reports a deduction trace, so any technique that merely *prunes*
/// the search (never flips the yes/no) is optional — completeness comes from the
/// branch. That frees it from everything the baseline needs the dual view for:
///
/// - **Row-major bands only** (`r`/`unsolved_r`, no `c`/`unsolved_c`). Rows and
///   boxes are in-lane; columns straddle bands and are simply never swept as
///   units — branching still reaches every completion. This halves both the
///   per-branch clone and the per-placement peer-mask work (no column mirror).
/// - **No locked candidates.** LC barely prunes the existence search (~1.06x
///   more DFS nodes without it) yet costs a `triplet_occ` + `DROP_TRIP` lookup on
///   every (band,digit) scan, so dropping it is a net win.
///
/// Both effects are verdict-preserving, so the generator fingerprint is exactly
/// invariant (the strip trajectory reads only the prober's yes/no). Validated as
/// the fastest banded variant — `banded-sl-nolc` — in solver-lab's decoupled
/// prober bench, oracle-pinned against the dual engine.
#[derive(Clone)]
struct ProberBoard {
    r: [B; 9],
    unsolved_r: B,
}

impl ProberBoard {
    /// Snapshot a [`BitBoard`]'s row-major half — the only view the prober needs.
    /// The column bands and `unsolved_c` are dropped on the floor (a strided copy
    /// of 10 `u32x4` instead of the dual board's 20).
    #[inline(always)]
    fn from_bitboard(bb: &BitBoard) -> Self {
        ProberBoard { r: bb.r, unsolved_r: bb.unsolved_r }
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell, forbid `d` on its
    /// peers — one `unsolved` clear and one peer-mask AND-NOT, no column mirror.
    #[inline(always)]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved_r &= !bit_r(cell);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.r.get_unchecked_mut((d - 1) as usize) &= !peer_mask_r(cell);
        }
    }

    /// Naked-single sieve over the row-major boards — the shared [`sieve`] over
    /// this board's `r`.
    #[cfg_attr(not(prof_solver), inline(always))]
    #[cfg_attr(prof_solver, inline(never))]
    fn sieve(&self) -> Sieve {
        sieve(&self.r)
    }

    /// Place a wave of naked singles into the row-major view. Returns false if two
    /// singles of the same digit are peers (a contradiction).
    #[cfg_attr(not(prof_solver), inline)]
    #[cfg_attr(prof_solver, inline(never))]
    fn place_singles(&mut self, singles: B) -> bool {
        for d in 0..9 {
            // SAFETY: d in 0..9.
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
            // peer_mask excludes the cell itself, so a group cell lands in the
            // accumulated peers iff another group cell peers it.
            if nonzero(peers_r & group) {
                return false;
            }
            self.unsolved_r &= !group;
            self.r[d] &= !peers_r;
        }
        true
    }

    /// Row-major band update: hidden singles in the three rows and three boxes of
    /// each band off the `SINGLE9` table — the in-lane units of this view. No
    /// locked candidates, no column sweep. The transpose of [`BitBoard::band_update_rm`]'s
    /// hidden-single half; the LC and column-major work is gone.
    #[cfg_attr(prof_solver, inline(never))]
    fn band_update_rm(&mut self) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                #[cfg(feature = "count")]
                let mut prod = false;
                band_ctr_inc(2);
                let mut live = (self.r[d] & self.unsolved_r)[b];
                // Hidden singles in the three rows (each a contiguous 9-bit chunk).
                for rr in 0..3 {
                    let s = SINGLE9[((live >> (9 * rr)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(rm_cell(b, 9 * rr + s as u32), d as u8 + 1);
                        changed = true;
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
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
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
                #[cfg(feature = "count")]
                if prod {
                    band_ctr_inc(3);
                }
            }
        }
        changed
    }

    /// Propagate to a fixpoint: naked singles (cross-digit sieve) drained first,
    /// then the row-major hidden-single band update, looping until nothing
    /// changes. The single-layout no-LC counterpart of [`BitBoard::propagate`].
    #[cfg_attr(prof_solver, inline(never))]
    fn propagate(&mut self) -> Prop {
        band_ctr_inc(0);
        loop {
            loop {
                pbump(3);
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
                pbump(4);
                if !self.place_singles(singles) {
                    return Prop::Contradiction;
                }
            }
            band_ctr_inc(1);
            if !self.band_update_rm() {
                return Prop::Stuck;
            }
        }
    }

    #[cfg_attr(not(prof_solver), inline)]
    #[cfg_attr(prof_solver, inline(never))]
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

    /// True iff the grid has at least one completion. Single-layout: naked singles
    /// + row/box hidden singles to a fixpoint, then branch — columns are reached
    /// only through branching, which is complete.
    fn solve_first(&mut self) -> bool {
        pbump(2);
        loop {
            match self.propagate() {
                Prop::Solved => return true,
                Prop::Contradiction => return false,
                Prop::Stuck => {}
            }
            pbump(5);
            let (cell, mask) = self.branch_cell();
            let mut m = mask;
            loop {
                let d = m.trailing_zeros() as u8 + 1;
                m &= m - 1;
                pbump(7);
                if m == 0 {
                    self.place(cell, d);
                    break;
                }
                pbump(6);
                let mut child = self.clone();
                child.place(cell, d);
                if child.solve_first() {
                    return true;
                }
            }
        }
    }
}

impl BitBoard {
    /// Uniqueness gate: restrict cell `i` to the alternate digits `alts` and ask
    /// whether any completion exists — i.e. stripping `i` made the puzzle
    /// non-unique. Runs on a lean single-layout [`ProberBoard`] built from this
    /// board's row-major half (the dual `BitBoard` is the baseline's; the prober
    /// needs neither the column view nor locked candidates — see [`ProberBoard`]),
    /// so the base is untouched for the baseline.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn any_alt_solves(&self, i: usize, alts: u16) -> bool {
        pbump(0);
        let mut s = ProberBoard::from_bitboard(self);
        let kr = bit_r(i);
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
            }
        }
        #[cfg(feature = "count")]
        let (n0, g0, empties) = unsafe { (PCTR[2], PCTR[5], popcnt(self.unsolved_r) as u16) };
        let r = s.solve_first();
        if r {
            pbump(1);
        }
        #[cfg(feature = "count")]
        unsafe {
            (*core::ptr::addr_of_mut!(ALT_STATS)).push(AltStat {
                empties,
                nodes: (PCTR[2] - n0) as u32,
                guesses: (PCTR[5] - g0) as u32,
                nonunique: r,
            });
        }
        r
    }

    /// THROWAWAY instrumentation: the exact first-closure a probe pays — build the
    /// lean prober board, restrict cell `i` to `alts`, propagate ONCE to fixpoint
    /// (no branching). Timing this isolates the closure cost from the branch cost
    /// in `any_alt_solves` (which is closure + branch), so it runs the same
    /// single-layout no-LC closure the real prober does.
    pub fn probe_closure_only(&self, i: usize, alts: u16) -> bool {
        let mut s = ProberBoard::from_bitboard(self);
        let kr = bit_r(i);
        for d in 0..9 {
            if alts & (1 << d) == 0 {
                s.r[d] &= !kr;
            }
        }
        matches!(s.propagate(), Prop::Solved)
    }

    /// THROWAWAY instrumentation: how many cells the prober's unrestricted
    /// propagation closure (naked + row/box hidden singles, no LC, NO branch)
    /// determines.
    pub fn closure_solved(&self) -> u32 {
        let before = popcnt(self.unsolved_r);
        let mut s = ProberBoard::from_bitboard(self);
        match s.propagate() {
            Prop::Contradiction => before, // dead line — treat as fully resolved
            _ => before - popcnt(s.unsolved_r),
        }
    }
}
