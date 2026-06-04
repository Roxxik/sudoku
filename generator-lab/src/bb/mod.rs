//! **Dual-banded** bitboard for the **baseline technique engine** (the spec
//! oracle). The uniqueness prober was split out into a separate, leaner
//! single-layout [`ProberBoard`]: a pure existence oracle needs neither the
//! column view nor locked candidates (completeness comes from branching), so
//! giving it its own row-major-only board halves the per-branch clone and drops
//! the LC scan — a ~12% generator win, verdict-identical. The baseline keeps the
//! dual view below because it genuinely needs every unit in-lane (it stalls into
//! the subset ladder without column hidden singles).
//!
//! ## Why two views
//!
//! A **band** is 3 lines = 27 cells = three boxes side by side, stored in a
//! `Simd<u32, 4>` (lanes 0/1/2 = the three bands, lane 3 unused). In a single
//! row-major banding, rows and boxes are *in-lane* (a unit is a constant `[u32;
//! 4]` mask, every test is plain `u32` work) but **columns straddle all three
//! bands** — the one awkward unit. So we keep the candidates in two transposed
//! copies:
//!
//! - `r[d]` — **row-major** bands (lane `= row/3`, bit `= (row%3)*9 + col`):
//!   rows and boxes are in-lane.
//! - `c[d]` — **column-major** bands (lane `= col/3`, bit `= (col%3)*9 + row`):
//!   columns and boxes are in-lane.
//!
//! Between them *every* unit is in-lane in at least one view, so all hidden
//! singles and all locked candidates are cheap and popcount-free, and the one
//! within-band locked-candidates table ([`BAND_KEEP_OCC`]) applies to both.
//!
//! ## Keeping the two views in sync is cheap
//!
//! - **Placements** (naked/hidden singles, branch decisions) sync for free: a
//!   placement clears a precomputed peer-mask, and we just clear it in *both*
//!   layouts (`peer_mask_r` and `peer_mask_c`).
//! - **Locked-candidates eliminations** are the only cross-view effect, and they
//!   transpose *cleanly*: locked candidates clear whole **triplets**, and a
//!   row-major triplet `(band, band-row, box-col)` is exactly a col-major
//!   vertical-triplet. So LC runs at triplet granularity and each dropped triplet
//!   clears a precomputed mask in *both* views — a 27-entry lookup, never a
//!   general 81-bit transpose.
//!
//! ## Faithfulness without step-matching
//!
//! The strip trajectory depends only on two facts from the baseline solve:
//! whether it `solved`, and whether each required technique fired at least once.
//! Both are order-independent (the deductive closure is unique), so these
//! techniques need only be *sound* and *complete*, NOT step-for-step identical to
//! the scalar twins. The baseline's dual-view closure (both-view hidden singles,
//! both orientations of LC) is sound, and the [`ProberBoard`]'s leaner toolbox is
//! verdict-preserving (it only ever decides yes/no, and dropping a pruning-only
//! technique never flips that) — so the generator fingerprint is invariant.
//! `tests/bb_equiv.rs` cross-checks the baseline against the scalar engine;
//! the generator's `find 118329` anchor pins it end-to-end.
//!
//! ## Module layout
//! - [`layout`]: the banding maps, every precomputed lookup table, and the
//!   `Simd<u32, 4>` helpers — pure data shared by the engine and the prober.
//! - [`prober`]: the lean single-layout `ProberBoard` existence oracle and the
//!   `BitBoard` methods that enter it (`any_alt_solves`, closure diagnostics).
//! - this file (`mod`): the dual-view `BitBoard` itself — its I/O
//!   (`from_board`/`apply_clear`/`apply_place`/`export_r`), the shared `Placed`
//!   clue map, and the baseline technique engine.

mod layout;
mod prober;

use crate::grid::{Board, CELLS, iter_digits};
use crate::technique_kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
};
use crate::util::for_each_combination;
use std::simd::Simd;

// The banding layout (cell↔band maps, lookup tables, `B`/`ZERO` and the SIMD
// helpers) lives in `layout`; re-export the cell-map fns the packed prober imports
// as `crate::bb::{rm_lane, ..}`.
pub(crate) use layout::{rm_bit, rm_cell, rm_lane};
use layout::{
    B, CM_LC_TRIP, DROP_TRIP, RM_LC_TRIP, SINGLE9, UNIT_CELLS, ZERO, at_least_two, bit_c, bit_r,
    cm_bit, cm_cell, cm_lane, exactly_one, first_rm, nonzero, peer_mask_c, peer_mask_r, popcnt,
    triplet_occ, unit_mask_r,
};

use crate::counters::counter_block;

// --- optional baseline anatomy counters (feature = "count") -------------------
// Count how often each technique is SCANNED per attempt (scan count × scan size
// ≈ cost), to see what dominates `baseline` before optimizing it. `bump(i)` tallies.
pub const CTR_NAMES: [&str; 8] = [
    "baseline-calls", "sieve-waves", "hidden_single", "lc_pointing", "lc_claiming",
    "naked_subset", "hidden_subset", "cell_candidates",
];
counter_block!(CTR: 8, inc = bump, add = ctr_add, snapshot = ctr_snapshot, reset = ctr_reset);

// --- prober anatomy counters (feature = "count") ------------------------------
// Where does the existence-DFS cost go: propagation (singles waves) vs branching
// (recursion / clones)? `pbump(i)` tallies.
pub const PCTR_NAMES: [&str; 8] = [
    "alt-calls", "alt-nonunique", "solve_first-nodes", "sieve-waves",
    "place_singles-calls", "branch-points", "child-clones", "branch-digits",
];
counter_block!(PCTR: 8, inc = pbump, add = pctr_add, snapshot = pctr_snapshot, reset = pctr_reset);

// --- band_update scan metrics (feature = "count") -----------------------------
// Sizes the dirty-band-tracking opportunity: 0 propagate-calls, 1 band-passes
// (one rm+cm fixpoint iteration), 2 bd-scans (per-(band,digit) iterations across
// rm+cm), 3 bd-productive (scans that actually dropped a triplet or placed a
// single). waste = 1 - productive/scans = the rescans dirty-tracking could skip.
// `band_ctr_inc(i)` tallies.
pub const BAND_CTR_NAMES: [&str; 4] = ["propagate-calls", "band-passes", "bd-scans", "bd-productive"];
counter_block!(BAND_CTR: 4, inc = band_ctr_inc, add = band_ctr_add, snapshot = band_ctr_snapshot, reset = band_ctr_reset);

/// One existence-DFS query (`any_alt_solves`): the board sparsity it ran on, the
/// work it cost, and whether it found a 2nd solution. Lets a diagnostic see WHERE
/// prober time concentrates (sparse vs dense boards; unique-proving vs
/// alt-finding) instead of just totals.
#[cfg(feature = "count")]
#[derive(Clone, Copy)]
pub struct AltStat {
    pub empties: u16,
    pub nodes: u32,
    pub guesses: u32,
    pub nonunique: bool,
}
#[cfg(feature = "count")]
static mut ALT_STATS: Vec<AltStat> = Vec::new();
#[cfg(feature = "count")]
pub fn alt_stats() -> &'static [AltStat] {
    unsafe { &*core::ptr::addr_of!(ALT_STATS) }
}
#[cfg(feature = "count")]
pub fn alt_stats_reset() {
    unsafe {
        (*core::ptr::addr_of_mut!(ALT_STATS)).clear();
    }
}

/// The nine digit boards in both bandings, plus the empty-cell mask in both.
/// `r`/`unsolved_r` are row-major (rows & boxes in-lane); `c`/`unsolved_c` are
/// column-major (columns & boxes in-lane). The two are kept consistent at every
/// mutation.
#[derive(Clone, PartialEq)]
pub struct BitBoard {
    r: [B; 9],
    c: [B; 9],
    unsolved_r: B,
    unsolved_c: B,
}

struct Sieve {
    ones: B,
    twos: B,
}

/// Naked-single sieve over nine row-major digit boards: `ones` = cells with at
/// least one candidate, `twos` = cells with at least two, accumulated across the
/// digit boards. `ones & !twos` are the naked singles, `!ones` the dead cells.
/// Shared verbatim by [`BitBoard`] (dual view) and [`ProberBoard`] (single view)
/// — both sieve only the row-major bands.
#[inline(always)]
fn sieve(r: &[B; 9]) -> Sieve {
    let mut ones = ZERO;
    let mut twos = ZERO;
    for d in 0..9 {
        // SAFETY: d in 0..9.
        let b = unsafe { *r.get_unchecked(d) };
        twos |= ones & b;
        ones |= b;
    }
    Sieve { ones, twos }
}

/// Per-digit *clue* positions in the row-major banding: bit for cell `i` of
/// `r[d-1]` is set iff cell `i` holds the clue digit `d`. This is the one fact
/// the candidate bands can't cheaply answer — which surviving peer holds which
/// digit — so `apply_clear` reads it to recompute reopened candidates with band
/// ops instead of a per-peer scan of the scalar grid. Kept *outside* `BitBoard`:
/// the solver never touches it, so it must not bloat the per-branch clone in
/// `solve_first`. Only the row-major banding is stored — `apply_clear` enumerates
/// it to get clue *cells*, then indexes both peer-mask tables by cell.
pub struct Placed {
    r: [B; 9],
}

impl Placed {
    pub fn from_board(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        for cell in 0..CELLS {
            let d = b.cell(cell);
            if d != 0 {
                r[(d - 1) as usize][rm_lane(cell)] |= 1u32 << rm_bit(cell);
            }
        }
        Placed { r: r.map(Simd::from_array) }
    }
}

/// Why band propagation stopped.
enum Prop {
    /// Every cell is filled — a completion exists on this line.
    Solved,
    /// A cell ran out of candidates — this line is dead.
    Contradiction,
    /// A fixpoint with empty cells remaining — the caller must branch.
    Stuck,
}

impl BitBoard {
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn from_board(b: &Board) -> Self {
        let mut r = [[0u32; 4]; 9];
        let mut c = [[0u32; 4]; 9];
        let mut ur = [0u32; 4];
        let mut uc = [0u32; 4];
        for cell in 0..CELLS {
            if b.cell(cell) == 0 {
                ur[rm_lane(cell)] |= 1u32 << rm_bit(cell);
                uc[cm_lane(cell)] |= 1u32 << cm_bit(cell);
                for d in iter_digits(b.candidates(cell)) {
                    r[(d - 1) as usize][rm_lane(cell)] |= 1u32 << rm_bit(cell);
                    c[(d - 1) as usize][cm_lane(cell)] |= 1u32 << cm_bit(cell);
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

    /// Re-open cell `i` (which held digit `d0`) in both views, keeping
    /// `self == from_board(board_from_cells(cells))` without a scalar *candidate*
    /// shadow — bb owns all candidate state. `placed` is the per-digit clue map
    /// (the one thing bb can't derive: which digit a surviving peer holds); this
    /// method drops cell `i` from it first, then re-opens candidates in two
    /// places: the **cleared cell** (its column becomes its naked candidates —
    /// every digit no still-present peer holds) and **its peers** (`d0` returns to
    /// the empty peers no *other* present peer still blocks). Both are pure band
    /// ops off `placed` — no per-peer scan of the scalar grid. Returns cell `i`'s
    /// naked candidate mask (the strip's `alts` source).
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_clear(&mut self, i: usize, d0: u8, placed: &mut Placed) -> u16 {
        let (ir, ic) = (bit_r(i), bit_c(i));
        let e = (d0 - 1) as usize;
        // Cell i stops being a clue before we read peer occupancy off `placed`.
        placed.r[e] &= !ir;

        // Phase 1: reopen cells
        // The cleared cell: filled -> empty, its column = its naked candidates.
        // Digit `d` survives iff no still-present peer holds it (`placed[d]` misses
        // i's peer mask) — a banded test per digit, not a per-peer grid scan.
        self.unsolved_r |= ir;
        self.unsolved_c |= ic;
        let prr = peer_mask_r(i);
        let mut cand: u16 = 0;
        for d in 0..9 {
            if !nonzero(placed.r[d] & prr) {
                cand |= 1 << d;
            }
        }
        for d in 0..9 {
            if cand & (1 << d) != 0 {
                self.r[d] |= ir;
                self.c[d] |= ic;
            } else {
                self.r[d] &= !ir;
                self.c[d] &= !ic;
            }
        }

        // Phase 2: reopen peers
        // Its peers: d0 was blocked at every peer by cell i, so its bit there was
        // 0; set it on each empty peer no *other* present peer still holds d0. A
        // clue d0 forbids d0 across its whole unit, i.e. exactly over its peer
        // mask, so the cells still blocked from d0 are the union of the surviving
        // d0 clues' peer masks (cell i is already dropped from `placed`). The
        // reopened peers are i's empty peers outside that union — all band ops.
        let mut blk_r = ZERO;
        let mut blk_c = ZERO;
        for lane in 0..3 {
            let mut w = placed.r[e][lane];
            while w != 0 {
                let q = rm_cell(lane, w.trailing_zeros());
                blk_r |= peer_mask_r(q);
                blk_c |= peer_mask_c(q);
                w &= w - 1;
            }
        }
        self.r[e] |= prr & self.unsolved_r & !blk_r;
        self.c[e] |= peer_mask_c(i) & self.unsolved_c & !blk_c;
        cand
    }

    /// Mirror `b.place(i, d0)` (the strip's revert) onto both views: cell `i`
    /// goes empty -> filled (column cleared), `d0` leaves every peer, and `i`
    /// becomes a clue again in `placed`.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn apply_place(&mut self, i: usize, d0: u8, placed: &mut Placed) {
        let (ir, ic) = (bit_r(i), bit_c(i));
        let e = (d0 - 1) as usize;
        placed.r[e] |= ir;
        self.unsolved_r &= !ir;
        self.unsolved_c &= !ic;
        for d in 0..9 {
            self.r[d] &= !ir;
            self.c[d] &= !ic;
        }
        self.r[e] &= !peer_mask_r(i);
        self.c[e] &= !peer_mask_c(i);
    }

    /// Place digit `d` (1..=9) at cell `c`: decide the cell in both views, forbid
    /// `d` on its peers in both views. Peer-mask clears are precomputed, so the
    /// two-view sync is just two extra AND-NOTs.
    #[inline(always)]
    fn place(&mut self, cell: usize, d: u8) {
        self.unsolved_r &= !bit_r(cell);
        self.unsolved_c &= !bit_c(cell);
        // SAFETY: d in 1..=9 so d-1 in 0..9.
        unsafe {
            *self.r.get_unchecked_mut((d - 1) as usize) &= !peer_mask_r(cell);
            *self.c.get_unchecked_mut((d - 1) as usize) &= !peer_mask_c(cell);
        }
    }

    /// Candidate digit bitmask (`1 << (digit-1)`) of cell `c`, from the row-major
    /// boards.
    #[inline]
    fn cell_candidates(&self, c: usize) -> u16 {
        bump(7);
        let cb = bit_r(c);
        let mut m = 0u16;
        for d in 0..9 {
            if nonzero(self.r[d] & cb) {
                m |= 1 << d;
            }
        }
        m
    }

    /// THROWAWAY instrumentation: empty cells on this board.
    pub fn count_empties(&self) -> u32 {
        popcnt(self.unsolved_r)
    }

    /// Export the row-major bands and the empty-cell mask as plain `[u32; 4]`
    /// arrays — the candidate state the packed prober ([`crate::simt::prober`]) needs to
    /// load a lane. The column-major view is redundant for naked-single
    /// propagation (peers and the sieve are all in-lane row-major), so it is not
    /// exported. This is exactly the [`ProberBoard`] half (the lean scalar prober
    /// runs on the same single-layout no-LC state), so packed and scalar verdicts
    /// agree by construction.
    pub fn export_r(&self) -> ([[u32; 4]; 9], [u32; 4]) {
        (core::array::from_fn(|d| self.r[d].to_array()), self.unsolved_r.to_array())
    }

    // --- dual-view band closure (baseline fast path) ----------------------
    //
    // The fused naked + hidden singles + both-LC fixpoint that drives the
    // baseline's fast path (`baseline_fast` via `propagate_g`). The uniqueness
    // prober does not ride this — it has its own single-layout no-LC closure on
    // `ProberBoard` (see the `prober` submodule).

    /// Naked-single sieve over the row-major boards — the shared [`sieve`] over
    /// this board's `r` (which cells admit exactly one digit, `ones & !twos`, and
    /// which admit none, `!ones`).
    #[cfg_attr(not(prof_solver), inline(always))]
    #[cfg_attr(prof_solver, inline(never))]
    fn sieve(&self) -> Sieve {
        sieve(&self.r)
    }

    /// Place a wave of naked singles (cells `singles`) into both views. Returns
    /// false if two singles of the same digit are peers (a contradiction).
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
            // peer_mask excludes the cell itself, so a group cell lands in the
            // accumulated peers iff another group cell peers it.
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

    /// Fused row-major band update: for each band and digit, read the band's live
    /// candidates ONCE and drive both locked-candidates (box↔row) and hidden
    /// singles (rows + boxes — the in-lane units of this view) off it, via the
    /// `BAND_KEEP_OCC` and `SINGLE9` tables. LC clears whole dropped triplets in
    /// both views ([`RM_LC_TRIP`]); each hidden single is placed (which syncs both
    /// views) and the band is re-read so the next unit sees it. Returns whether
    /// anything changed. This is the banded solver's heart — no per-unit scan, no
    /// popcount; the only arithmetic is the occupancy gather and three box shifts.
    #[cfg_attr(prof_solver, inline(never))]
    fn band_update_rm<const LC: bool, const TRACK: bool>(&mut self, fired: &mut u32) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                #[cfg(feature = "count")]
                let mut prod = false;
                band_ctr_inc(2);
                let mut live = (self.r[d] & self.unsolved_r)[b];
                // Locked candidates: drop the triplets the within-band fixpoint
                // kills. Gated by the const `LC`: a spec whose baseline excludes
                // locked candidates monomorphizes this block away entirely.
                if LC {
                    let occ = triplet_occ(live);
                    let mut dropped = DROP_TRIP[occ];
                    if dropped != 0 {
                        changed = true;
                        if TRACK {
                            *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
                        }
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        while dropped != 0 {
                            let t = dropped.trailing_zeros() as usize;
                            dropped &= dropped - 1;
                            let (rm, cm) = RM_LC_TRIP[b][t];
                            self.r[d] &= !Simd::from_array(rm);
                            self.c[d] &= !Simd::from_array(cm);
                        }
                        live = (self.r[d] & self.unsolved_r)[b];
                    }
                }
                // Hidden singles in the three rows (each a contiguous 9-bit chunk).
                for rr in 0..3 {
                    let s = SINGLE9[((live >> (9 * rr)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(rm_cell(b, 9 * rr + s as u32), d as u8 + 1);
                        changed = true;
                        if TRACK {
                            *fired |= 1 << HIDDEN_SINGLE;
                        }
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
                        if TRACK {
                            *fired |= 1 << HIDDEN_SINGLE;
                        }
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

    /// Fused column-major band update: the transpose of [`band_update_rm`] — same
    /// tables on the col-major bands, giving box↔column LC ([`CM_LC_TRIP`]) and
    /// hidden singles in columns (the in-lane lines of this view). Boxes are
    /// already covered row-major, so only the three columns of each band are swept.
    #[cfg_attr(prof_solver, inline(never))]
    fn band_update_cm<const LC: bool, const TRACK: bool>(&mut self, fired: &mut u32) -> bool {
        let mut changed = false;
        for b in 0..3 {
            for d in 0..9 {
                #[cfg(feature = "count")]
                let mut prod = false;
                band_ctr_inc(2);
                let mut live = (self.c[d] & self.unsolved_c)[b];
                if LC {
                    let occ = triplet_occ(live);
                    let mut dropped = DROP_TRIP[occ];
                    if dropped != 0 {
                        changed = true;
                        if TRACK {
                            *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
                        }
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        while dropped != 0 {
                            let t = dropped.trailing_zeros() as usize;
                            dropped &= dropped - 1;
                            let (rm, cm) = CM_LC_TRIP[b][t];
                            self.r[d] &= !Simd::from_array(rm);
                            self.c[d] &= !Simd::from_array(cm);
                        }
                        live = (self.c[d] & self.unsolved_c)[b];
                    }
                }
                // Hidden singles in the three columns (each a 9-bit chunk here).
                for cc in 0..3 {
                    let s = SINGLE9[((live >> (9 * cc)) & 0x1FF) as usize];
                    if s != 0xFF {
                        self.place(cm_cell(b, 9 * cc + s as u32), d as u8 + 1);
                        changed = true;
                        if TRACK {
                            *fired |= 1 << HIDDEN_SINGLE;
                        }
                        #[cfg(feature = "count")]
                        {
                            prod = true;
                        }
                        live = (self.c[d] & self.unsolved_c)[b];
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

    /// Generic propagation core for the baseline fast path. `LC` gates the
    /// locked-candidates step (a spec whose baseline excludes LC compiles it away);
    /// `TRACK` records which cheap kinds fired into `fired` (bit = kind index), for
    /// the baseline gate's fired-or-not bookkeeping. The prober does not enter here
    /// — it runs its own leaner single-layout no-LC closure on [`ProberBoard`].
    #[cfg_attr(prof_solver, inline(never))]
    fn propagate_g<const LC: bool, const TRACK: bool>(&mut self, fired: &mut u32) -> Prop {
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
                if TRACK {
                    *fired |= 1 << NAKED_SINGLE;
                }
                if !self.place_singles(singles) {
                    return Prop::Contradiction;
                }
            }
            band_ctr_inc(1);
            let mut changed = self.band_update_rm::<LC, TRACK>(fired);
            changed |= self.band_update_cm::<LC, TRACK>(fired);
            if !changed {
                return Prop::Stuck;
            }
        }
    }

    // --- baseline technique engine ----------------------------------------
    //
    // The baseline is the spec oracle: techniques stay discrete, gated by
    // `allowed`, in difficulty order, with per-kind firing counts. It reads only
    // the row-major view (columns straddle lanes there, but baseline isn't the
    // peak hot path and `exactly_one`/`nonzero` handle the cross-lane case); the
    // column-major view rides along in the clone unused.

    /// Baseline gate: solve `self` with the `allowed` toolbox, tallying which
    /// kinds fired. The requirement check reads only the `forced` kinds' counts,
    /// so those must be exact; the rest only need fired-or-not.
    ///
    /// Dispatches on the spec shape between a batched fast path and the discrete
    /// reference engine. Both yield identical `solved` and identical counts for
    /// every Forced kind, so the strip trajectory is invariant to the choice
    /// (the `find` anchor and `bb_equiv` pin this). `forced` is the Forced-kind
    /// membership mask: a Forced kind must be counted exactly and so can never be
    /// folded into the batched closure, hence the fast path requires no cheap kind
    /// (singles / locked candidates) be Forced.
    ///
    /// PRECONDITION — the baseline toolbox must be **confluent**: applying its
    /// techniques to a fixpoint reaches the same closure regardless of order, so
    /// `solved` and the per-kind firing counts are well-defined (not an artifact of
    /// scan order). Confluence is what lets both engines reorder/batch freely.
    ///
    /// It fails when a harder technique can degenerate into a simpler deduction the
    /// toolbox lacks: a locked-candidates or subset elimination can collapse a unit
    /// to a lone candidate — a *naked* single (needs NakedSingle) — or a digit to a
    /// lone cell — a *hidden* single (needs HiddenSingle). The guard below requires
    /// **both** singles once anything above hidden single is in the baseline. A
    /// baseline that is a single technique on its own (just NakedSingle, or just
    /// HiddenSingle) has nothing that degenerates and is fine — so a one-single
    /// baseline is supported, but LC/subsets without both singles are not.
    #[cfg_attr(feature = "profiling", inline(never))]
    pub fn baseline(&self, allowed: KindMask, forced: KindMask) -> SolveTrace {
        const NS_BIT: KindMask = 1 << NAKED_SINGLE;
        const HS_BIT: KindMask = 1 << HIDDEN_SINGLE;
        const LCP_BIT: KindMask = 1 << LC_POINTING;
        const LCC_BIT: KindMask = 1 << LC_CLAIMING;
        const CHEAP: KindMask = NS_BIT | HS_BIT | LCP_BIT | LCC_BIT;
        // Anything above a hidden single can degenerate into either single, so if
        // any such kind is in the baseline, BOTH singles must be too, else the
        // toolbox is non-confluent (see PRECONDITION). Debug-only: release ignores
        // it, and the generator runs in release.
        const HARDER: KindMask = !(NS_BIT | HS_BIT) & ((1 << NUM) - 1);
        debug_assert!(
            allowed & HARDER == 0 || allowed & (NS_BIT | HS_BIT) == (NS_BIT | HS_BIT),
            "non-confluent baseline (mask {allowed:#b}): a technique above hidden \
             single is allowed without both singles — a degenerate elimination it \
             produces could be a naked or hidden single the toolbox cannot place, \
             making the solved/counts verdict scan-order dependent. Not supported."
        );

        let has_ns = allowed & NS_BIT != 0;
        let has_hs = allowed & HS_BIT != 0;
        let has_lcp = allowed & LCP_BIT != 0;
        let has_lcc = allowed & LCC_BIT != 0;
        // The fused closure does naked+hidden singles always and BOTH locked-
        // candidate orientations together. So the fast path needs singles present,
        // LC present-both-or-absent-both (it cannot honor just one orientation),
        // and no Forced cheap kind (those need exact discrete counts). Everything
        // else — single-orientation LC, Forced singles/LC, singles-absent — routes
        // to the proven discrete engine.
        let fast = has_ns && has_hs && (has_lcp == has_lcc) && (forced & CHEAP == 0);
        if !fast {
            return self.baseline_discrete(allowed);
        }
        if has_lcp {
            self.baseline_fast::<true>(allowed)
        } else {
            self.baseline_fast::<false>(allowed)
        }
    }

    /// Fast baseline: the gated fused closure (naked + hidden singles, plus both
    /// LC orientations iff `LC`) drives every cheap technique to its joint
    /// fixpoint; when that stalls, the discrete subset ladder
    /// (NakedPair..HiddenQuad) advances one step and the closure re-runs. The
    /// cheap-fixpoint board at every subset-decision point is identical to what
    /// the easiest-first reference reaches there (the {singles, LC} closure is
    /// confluent), so each subset firing — and the Forced target's exact count —
    /// matches the reference. Cheap kinds are recorded fired-or-not only (they are
    /// never Forced on this path, so the generator never reads their counts).
    fn baseline_fast<const LC: bool>(&self, allowed: KindMask) -> SolveTrace {
        bump(0);
        let mut bb = self.clone();
        let mut counts = [0u16; NUM];
        let mut fired = 0u32;
        let result = loop {
            match bb.propagate_g::<LC, true>(&mut fired) {
                // A board with a known solution never contradicts under sound
                // techniques; fold Contradiction into baseline-unsolvable anyway.
                Prop::Solved => break true,
                Prop::Contradiction => break false,
                Prop::Stuck => {}
            }
            match bb.step_subsets(allowed) {
                Some(k) => counts[k] = counts[k].saturating_add(1),
                None => break false,
            }
        };
        for k in 0..NUM {
            if fired & (1 << k) != 0 && counts[k] == 0 {
                counts[k] = 1;
            }
        }
        SolveTrace { solved: result, counts }
    }

    /// Discrete reference baseline: naked singles drain in bit-parallel waves; the
    /// rarer harder techniques (hidden single, both LC orientations, subsets)
    /// apply one easiest-first step at a time, each counted exactly. The general
    /// engine for any spec the fast path cannot fold faithfully.
    fn baseline_discrete(&self, allowed: KindMask) -> SolveTrace {
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
            if !nonzero(bb.unsolved_r) {
                return SolveTrace { solved: true, counts };
            }
            match bb.step_harder(allowed) {
                Some(k) => counts[k] = counts[k].saturating_add(1),
                None => return SolveTrace { solved: false, counts },
            }
        }
    }

    /// The discrete subset ladder, easiest-first, gated by `allowed`: returns the
    /// kind index of the first subset technique that fires, or `None` if none do.
    /// Used by the fast path after the fused closure stalls (singles + LC are
    /// already drained by the closure, so only NakedPair..HiddenQuad remain).
    fn step_subsets(&mut self, allowed: KindMask) -> Option<usize> {
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

    /// Place every naked single (cell with exactly one candidate) in waves until
    /// none remain; returns how many were placed.
    #[inline]
    fn drain_naked_singles(&mut self) -> u16 {
        let mut total = 0u16;
        loop {
            bump(1);
            let Sieve { ones, twos } = self.sieve();
            let singles = self.unsolved_r & ones & !twos;
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
    fn step_harder(&mut self, allowed: KindMask) -> Option<usize> {
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
    /// Precompute `r[d] & unsolved_r` once per digit so the 27×9 unit scan is one
    /// AND + `exactly_one` per pair. Returns at the first placement, before any
    /// board mutation.
    fn hidden_single(&mut self) -> bool {
        bump(2);
        let mut bd = [ZERO; 9];
        for d in 0..9 {
            bd[d] = self.r[d] & self.unsolved_r;
        }
        for u in 0..27 {
            let um = unit_mask_r(u);
            for d in 0..9 {
                let pos = bd[d] & um;
                if exactly_one(pos) {
                    self.place(first_rm(pos), d as u8 + 1);
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
            let bm = unit_mask_r(18 + b);
            let br = (b / 3) * 3;
            let bc = (b % 3) * 3;
            for d in 0..9 {
                let pos = self.r[d] & bm & self.unsolved_r;
                if !at_least_two(pos) {
                    continue;
                }
                // rows of this box
                for rr in br..br + 3 {
                    let rm = unit_mask_r(rr);
                    if nonzero(pos & !rm) {
                        continue; // not all in this row
                    }
                    let targets = self.r[d] & rm & !bm & self.unsolved_r;
                    if nonzero(targets) {
                        self.eliminate_both(d, targets);
                        return true;
                    }
                }
                // cols of this box
                for cc in bc..bc + 3 {
                    let cm = unit_mask_r(9 + cc);
                    if nonzero(pos & !cm) {
                        continue;
                    }
                    let targets = self.r[d] & cm & !bm & self.unsolved_r;
                    if nonzero(targets) {
                        self.eliminate_both(d, targets);
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
            let lm = unit_mask_r(line);
            for d in 0..9 {
                let pos = self.r[d] & lm & self.unsolved_r;
                if !at_least_two(pos) {
                    continue;
                }
                let first = first_rm(pos);
                let b = (first / 9 / 3) * 3 + (first % 9) / 3;
                let bm = unit_mask_r(18 + b);
                if nonzero(pos & !bm) {
                    continue; // not all in one box
                }
                let targets = self.r[d] & bm & !lm & self.unsolved_r;
                if nonzero(targets) {
                    self.eliminate_both(d, targets);
                    return true;
                }
            }
        }
        false
    }

    /// Clear digit `d` (0-based) from the row-major target cells `targets` in both
    /// views — the baseline's elimination primitive, mirroring a row-major mask
    /// into the column-major copy cell by cell.
    #[inline]
    fn eliminate_both(&mut self, d: usize, targets: B) {
        self.r[d] &= !targets;
        for lane in 0..3 {
            let mut g = targets[lane];
            while g != 0 {
                let cell = rm_cell(lane, g.trailing_zeros());
                g &= g - 1;
                self.c[d] &= !bit_c(cell);
            }
        }
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
                if nonzero(self.unsolved_r & bit_r(c)) {
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
                    if !nonzero(self.unsolved_r & bit_r(c)) {
                        continue;
                    }
                    if combo.iter().any(|&k| cand_cells[k] == c) {
                        continue;
                    }
                    let rm = self.cell_candidates(c) & union;
                    if rm != 0 {
                        for d in 0..9 {
                            if rm & (1 << d) != 0 {
                                self.r[d] &= !bit_r(c);
                                self.c[d] &= !bit_c(c);
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
                    if nonzero(self.r[d] & bit_r(c) & self.unsolved_r) {
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
                                self.r[d] &= !bit_r(c);
                                self.c[d] &= !bit_c(c);
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
