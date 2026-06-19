//! `FusedLogicSolver` — the **fast path** of the logic solver: bb's fused band closure
//! ported onto a single row-major [`SolverState`]. The performance counterpart
//! of the composable [`LogicSolver`](super::LogicSolver) (the analogue of
//! [`probe::Search`](crate::probe::Search) to the prober's `Singles`): identical
//! verdicts, far less work — no per-cell unit scan, no popcount.
//!
//! ## What "fused" means
//!
//! Instead of scanning each of the 27 units per technique, it reads one **band**
//! (3 lines = 27 cells, in-lane) of one digit at a time and drives several
//! techniques off that single read:
//! - **locked candidates**: a band's 9-bit triplet occupancy ([`triplet_occ`])
//!   indexes [`DROP_TRIP`], which gives the whole-triplet eliminations the
//!   within-band LC fixpoint makes (pointing *and* claiming at once);
//! - **hidden singles**: each in-lane unit (the band's three lines, and — row view
//!   only — three boxes) collapses to nine bits, and [`SINGLE9`] locates a lone
//!   candidate in one table load.
//!
//! The row-major sweep ([`band_update_rm`]) covers rows + boxes + box↔row LC with each
//! unit in-lane. Columns and box↔column LC do *not* need a second banding: the
//! single-view column sweep ([`band_update_cols`]) reads them straight off the row-major
//! board with a per-band column fold + broadcast (the scalar port of the warp kernel's
//! gather-free column pass), so no column view is held. Together with the naked-single
//! sieve they reach the full {singles, locked-candidates} fixpoint ([`propagate`]). When
//! that stalls, the discrete subset ladder advances one step —
//! reusing the *composable* [`super::techniques`] subsets (run on a one-shot cell-major
//! transpose) — and the closure re-runs. This is exactly bb's `baseline_fast`.
//!
//! ## Precondition (same as bb's fast path)
//!
//! The fused closure does naked + hidden singles *always* and both LC orientations
//! *together*, recording cheap kinds fired-or-not (not exact counts). So it is valid
//! only when the spec's baseline has **both singles**, LC **both-or-neither**, and
//! **no Forced cheap kind** (singles / locked candidates) — whose exact count the
//! requirement check would read. The two production specs (`train`/`drill`, Forced =
//! HiddenQuad) satisfy this. A spec that doesn't must use the composable
//! [`LogicSolver`](super::LogicSolver), the discrete reference. The subset counts and
//! the `solved` verdict are exact on this path (all three engines reach the identical
//! cheap-fixpoint before any subset fires), which is the contract the generator reads.

use super::{Eliminate, LogicBoard, LogicSolver, Solver, techniques};
use crate::counters::counter_block;
use crate::repr::banded::{Band, Banding, Bands, ColMajor, RowMajor};
use crate::repr::{
    Branchable, CELLS, CellIdx, Digit, GridMask, Mark, Marks, Occupancy, SolverState,
};

/// The fused solver's board: the single **row-major** candidate state. Columns and
/// box↔column LC are read off it by the in-fold column sweep ([`band_update_cols`]), so
/// the baseline no longer carries a column-major view (the `DualSolverState`'s second
/// banding) — the strip maintains one banding per clue clear/restore.
type RowState = SolverState<Bands<RowMajor>>;
use crate::scan::sieve::Sieve;
use crate::spec::kinds::{
    HIDDEN_PAIR, HIDDEN_QUAD, HIDDEN_SINGLE, HIDDEN_TRIPLE, KindMask, LC_CLAIMING, LC_POINTING,
    NAKED_PAIR, NAKED_QUAD, NAKED_SINGLE, NAKED_TRIPLE, NUM, SolveTrace,
};

// --- baseline-gate workload metrics (feature = "count") -----------------------
// Data for the SIMT-baseline-solver design: how the scalar FusedLogicSolver's work
// splits between the vectorizable cheap closure and the hard subset ladder.
//   [0] solve calls
//   [1] total propagate() invocations (outer-loop iterations = cheap-closure depth)
//   [2] total harder steps that fired (step_harder -> Some: subsets + fish + wings)
//   [3] calls that fired >= 1 harder step (i.e. reached past the cheap closure)
//   [4] calls that solved (solved == true)
//   [5] calls with counts[HIDDEN_QUAD] >= 1 (the Forced kind actually fired)
//   [6] total step_harder invocations (incl. the None that ends an unsolvable stall)
//   [10+k] per-kind k firing totals (cheap kinds 0..4 are fired-or-not, <=1/call;
//          the harder kinds 4..NUM — subsets, fish, and wings — are exact)
// Sized `10 + NUM` so the per-kind slots `[10+k]` for k in 0..NUM always fit.
counter_block!(FSTAT: 10 + NUM, inc = fstat_inc, add = fstat_add, snapshot = fstat_snapshot, reset = fstat_reset);

/// The fused fast-path logic solver over the single row-major candidate state.
/// Stateless marker, like [`LogicSolver`](super::LogicSolver); see the module docs for
/// its precondition.
pub struct FusedLogicSolver;

impl Solver<RowState> for FusedLogicSolver {
    fn solve_tracked(board: &RowState, allowed: KindMask) -> SolveTrace {
        const NS: KindMask = 1 << NAKED_SINGLE;
        const HS: KindMask = 1 << HIDDEN_SINGLE;
        const LCP: KindMask = 1 << LC_POINTING;
        const LCC: KindMask = 1 << LC_CLAIMING;
        debug_assert!(
            allowed & NS != 0 && allowed & HS != 0,
            "fused fast path requires both singles (mask {allowed:#b}); use LogicSolver"
        );
        debug_assert!(
            (allowed & LCP != 0) == (allowed & LCC != 0),
            "fused fast path cannot honor a single LC orientation (mask {allowed:#b}); use LogicSolver"
        );
        if allowed & LCP != 0 {
            fused_solve::<true>(board, allowed)
        } else {
            fused_solve::<false>(board, allowed)
        }
    }

    /// The avoid-target walk needs per-step technique gating the fused closure can't
    /// express, and it is the cold verify path — delegate to the composable engine.
    fn min_target_uses(board: &RowState, scope: KindMask, target: KindMask) -> usize {
        LogicSolver::min_target_uses(board, scope, target)
    }
}

/// Why band propagation stopped — bb's `Prop`.
enum Prop {
    /// Every cell filled — a completion.
    Solved,
    /// A live cell ran out of candidates — unsolvable under these techniques.
    Contradiction,
    /// A fixpoint with empty cells left — the caller advances the subset ladder.
    Stuck,
}

/// Solve via the gated fused closure + the discrete subset ladder, easiest-first.
/// `LC` monomorphizes the locked-candidates step away when the baseline excludes it.
fn fused_solve<const LC: bool>(board: &RowState, allowed: KindMask) -> SolveTrace {
    fstat_add(0, 1);
    let mut b = board.clone();
    let mut counts = [0u16; NUM];
    let mut fired = 0u32;
    let solved = loop {
        fstat_add(1, 1);
        match propagate::<LC>(&mut b, &mut fired) {
            // A board with a known solution never contradicts under sound techniques;
            // fold Contradiction into baseline-unsolvable anyway.
            Prop::Solved => break true,
            Prop::Contradiction => break false,
            Prop::Stuck => {}
        }
        fstat_add(6, 1);
        match step_harder(&mut b, allowed) {
            Some(k) => counts[k] = counts[k].saturating_add(1),
            None => break false,
        }
    };
    // Cheap kinds (singles, both LC orientations) are recorded fired-or-not — the
    // fused closure reorders them, so their exact per-kind count is undefined, but
    // the generator never reads a cheap kind's count on this path (see module docs).
    for k in 0..NUM {
        if fired & (1 << k) != 0 && counts[k] == 0 {
            counts[k] = 1;
        }
    }
    #[cfg(feature = "count")]
    {
        let subset_steps: u64 = (NAKED_PAIR..NUM).map(|k| counts[k] as u64).sum();
        fstat_add(2, subset_steps);
        if subset_steps > 0 {
            fstat_add(3, 1);
        }
        if solved {
            fstat_add(4, 1);
        }
        if counts[HIDDEN_QUAD] >= 1 {
            fstat_add(5, 1);
        }
        for k in 0..NUM {
            if counts[k] > 0 {
                fstat_add(10 + k, counts[k] as u64);
            }
        }
    }
    SolveTrace { solved, counts }
}

/// Drive naked singles + both fused band updates to a joint fixpoint — bb's
/// `propagate_g`. Naked singles drain first (cheapest), then the row + column band
/// sweeps; loop until a whole pass changes nothing.
#[cfg_attr(prof_solver, inline(never))]
fn propagate<const LC: bool>(b: &mut RowState, fired: &mut u32) -> Prop {
    loop {
        match drain_naked_singles(b, fired) {
            Prop::Stuck => {}
            terminal => return terminal,
        }
        let mut changed = band_update_rm::<LC>(b, fired);
        changed |= band_update_cols::<LC>(b, fired);
        if !changed {
            return Prop::Stuck;
        }
    }
}

/// Place every naked single in bit-parallel waves until none remain, classifying the
/// board each pass. The depth-2 raw sieve over the row board surfaces dead cells
/// (`unsolved & dead`), the solved verdict (`!unsolved`), and the singles
/// (`unsolved & exactly(1)`) popcount-free. On a uniquely-solvable board naked singles
/// are forced and never conflict, so a wave places them all without a contradiction
/// check.
#[cfg_attr(prof_solver, inline(never))]
fn drain_naked_singles(b: &mut RowState, fired: &mut u32) -> Prop {
    #[cfg(feature = "count")]
    fstat_add(8, 1); // drain entries
    loop {
        #[cfg(feature = "count")]
        fstat_add(7, 1); // sieve recomputes (waves + the terminal pass)
        let sieve = Sieve::<Bands<RowMajor>, 2>::compute_raw(b.candidates());
        let unsolved = b.unsolved();
        if (unsolved & sieve.dead()).any() {
            return Prop::Contradiction;
        }
        if !unsolved.any() {
            return Prop::Solved;
        }
        let singles = unsolved & sieve.exactly(1);
        if !singles.any() {
            return Prop::Stuck;
        }
        *fired |= 1 << NAKED_SINGLE;
        // Group the singles by their lone digit (`singles & board[d]`) and place each
        // group in one batched dual op, reading the digit off the board instead of a
        // per-cell candidate scan.
        for di in 0..9 {
            let d = Digit::from_index(di);
            let group = singles & b.candidates()[d];
            if group.any() {
                b.place_single_group(d, group);
            }
        }
    }
}

/// One fused row-major sweep: for each band and digit, read the band's live
/// candidates once and drive box↔row locked candidates ([`DROP_TRIP`]) and hidden
/// singles in the three rows and three boxes ([`SINGLE9`]) off it. Returns whether
/// anything changed. Mirror of bb's `band_update_rm`.
///
/// Digit-outer: the live candidate set `cand[d] & unsolved` is one SIMD AND across
/// all three bands, so compute it once per digit and extract each band's lane, rather
/// than re-AND-ing per (band, digit). A placement refreshes it (peers can cross bands).
/// Reordering the per-(band,digit) sweep is fixpoint-confluent (singles + LC are monotone
/// eliminations), so `propagate`'s closure reaches the identical board.
///
/// Band-unrolled: the three bands are swept by an explicit [`sweep_band_rm`] each (`B` a
/// constant), not a `for band in 0..3`. A runtime band index makes `live_all.band(b)` read
/// through [`as_array`](std::simd::Simd::as_array) — `vmovaps %xmm,(%rsp)` then an indexed
/// `mov (%rsp,%rb,4),..` reload every band, feeding the hot `unit_single`/`triplet_occ` off
/// a store-forward. A const index ([`Bands::band_at`]) keeps the extract a register
/// `vpextrd`. Same memory-port win the prober's `band_hidden_singles` took.
#[cfg_attr(prof_solver, inline(never))]
fn band_update_rm<const LC: bool>(b: &mut RowState, fired: &mut u32) -> bool {
    let mut changed = false;
    for di in 0..9 {
        let d = Digit::from_index(di);
        let mut live_all = b.candidates()[d] & b.unsolved();
        changed |= sweep_band_rm::<0, LC>(b, &mut live_all, d, fired);
        changed |= sweep_band_rm::<1, LC>(b, &mut live_all, d, fired);
        changed |= sweep_band_rm::<2, LC>(b, &mut live_all, d, fired);
    }
    changed
}

/// One row-major band's box↔row locked candidates + hidden singles, band index `B` a
/// constant so the lane extract is a register `vpextrd`. Refreshes the shared `live_all`
/// after each elimination/placement (it can cross bands), so the caller's next
/// `sweep_band_rm` sees it.
#[inline(always)]
fn sweep_band_rm<const B: usize, const LC: bool>(
    b: &mut RowState,
    live_all: &mut Bands<RowMajor>,
    d: Digit,
    fired: &mut u32,
) -> bool {
    let mut changed = false;
    let mut live = live_all.band_at::<B>();
    if LC {
        let dropped = DROP_TRIP[triplet_occ(live)];
        if dropped != 0 {
            changed = true;
            *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
            drop_triplets::<RowMajor>(b, d, B, dropped);
            *live_all = b.candidates()[d] & b.unsolved();
            live = live_all.band_at::<B>();
        }
    }
    // Hidden singles in the six in-lane units (three rows, three boxes), each
    // read in place via [`Band::unit_single`]: AND with a unit mask, single-bit
    // test, the bit index doubling as the band position (no gather, no table).
    for &mask in &Band::UNIT_MASKS {
        if let Some(pos) = live.unit_single(mask) {
            b.place(RowMajor::cell_at(B, pos), d);
            changed = true;
            *fired |= 1 << HIDDEN_SINGLE;
            *live_all = b.candidates()[d] & b.unsolved();
            live = live_all.band_at::<B>();
        }
    }
    changed
}

/// One **single-view** column sweep: box↔column locked candidates and hidden singles
/// in the nine columns, computed from the **row-major** view alone — the scalar port of
/// the warp kernel's gather-free column pass ([`solve::simt::warp_pass_full`](super::simt)).
/// Replaces the former column-major-banded `band_update_cm`, so the column view never has
/// to be held or kept in sync.
///
/// A column `c` holds its cells at bit positions `c`, `c+9`, `c+18` of each row-major
/// band, so OR-ing a band's three 9-bit row slices ([`band_col_occ`]) gives that band's
/// per-column occupancy, and the three bands fold to the column's total — no
/// column-major view, no transpose.
///
/// **box↔column LC.** The column-major band `B`'s [`triplet_occ`] (bit `3r+k` = column
/// `3B+r` has a candidate in box-band `k`) is exactly bit `3B+r` of row-band `k`'s
/// column occupancy, so the three row-band occupancies feed the shared [`DROP_TRIP`]
/// table directly once repacked into its `3r+k` layout ([`pack_col_triplet_occ`]). The
/// dropped triplets become whole-column-segment eliminations via [`drop_triplets`]
/// (`ColMajor` geometry — pure cell math, no column view), the same routine the
/// column-major sweep used.
///
/// **column hidden singles.** Folding the occupancy with a saturating "seen-twice"
/// companion ([`col_hidden_singles`]) gives the columns with exactly one live candidate;
/// broadcasting that 9-bit mask across the three rows (`m | m<<9 | m<<18`) and AND-ing
/// the live board picks the forced cells, placed as a group — the peer-conflict trick in
/// [`place_group_with`](crate::repr::SolverState::place_group_with) folds a contradictory
/// double-placement into a dead cell, exactly as the naked-single wave does.
///
/// Reordering vs the old per-(band, column) sweep with refresh is fixpoint-confluent
/// (singles + LC are monotone eliminations), so [`propagate`]'s closure reaches the
/// identical board; `changed` drives the re-run that closes any cross-effect.
#[cfg_attr(prof_solver, inline(never))]
fn band_update_cols<const LC: bool>(b: &mut RowState, fired: &mut u32) -> bool {
    let mut changed = false;
    for di in 0..9 {
        let d = Digit::from_index(di);
        // box↔column locked candidates, from the row view's per-band column occupancy.
        if LC {
            let live = b.candidates()[d] & b.unsolved();
            let occ = band_col_occ(live);
            for big_b in 0..3 {
                let dropped = DROP_TRIP[pack_col_triplet_occ(occ, big_b)];
                if dropped != 0 {
                    *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
                    drop_triplets::<ColMajor>(b, d, big_b, dropped);
                    changed = true;
                }
            }
        }
        // Column hidden singles, read off the post-LC board (LC may have just emptied a
        // column to a single). Broadcast the single-column mask across the three rows of
        // every band, AND the live board to recover the forced cells, place the wave.
        let live = b.candidates()[d] & b.unsolved();
        let col_single = col_hidden_singles(live);
        if col_single != 0 {
            let bc = col_single | (col_single << 9) | (col_single << 18);
            let group = live & Bands::<RowMajor>::from_band_splat(bc);
            if group.any() {
                *fired |= 1 << HIDDEN_SINGLE;
                b.place_single_group(d, group);
                changed = true;
            }
        }
    }
    changed
}

/// Each row-major band's per-column occupancy: a 9-bit mask (bit `c` set iff column `c`
/// holds a live candidate somewhere in that band's three rows), one entry per band.
#[inline]
fn band_col_occ(live: Bands<RowMajor>) -> [u32; 3] {
    let occ = |band: Band| (band.line(0) | band.line(1) | band.line(2)) as u32;
    [occ(live.band_at::<0>()), occ(live.band_at::<1>()), occ(live.band_at::<2>())]
}

/// Repack the three row-band column occupancies into column-major band `B`'s 9-bit
/// triplet occupancy in [`DROP_TRIP`]'s `3r+k` layout: bit `3r+k` (column-within-band
/// `r`, box-band `k`) = bit `3*B+r` of row-band `k`'s occupancy. The inverse-indexed
/// twin of [`triplet_occ`], built from the row view instead of a column-major band.
#[inline]
fn pack_col_triplet_occ(occ: [u32; 3], big_b: usize) -> usize {
    let mut o = 0u32;
    for r in 0..3 {
        let c = 3 * big_b + r;
        o |= ((occ[0] >> c) & 1) << (3 * r);
        o |= ((occ[1] >> c) & 1) << (3 * r + 1);
        o |= ((occ[2] >> c) & 1) << (3 * r + 2);
    }
    o as usize
}

/// The columns holding exactly one live candidate for the digit — a 9-bit mask, bit `c`
/// set iff column `c`'s nine cells hold exactly one. Fold each band's three row slices to
/// a saturating (≥1, ≥2) 9-bit pair, then merge the three bands with the same identity
/// (the warp kernel's two-level column fold, scalarized).
#[inline]
fn col_hidden_singles(live: Bands<RowMajor>) -> u32 {
    let band_fold = |band: Band| {
        let (s0, s1, s2) = (band.line(0) as u32, band.line(1) as u32, band.line(2) as u32);
        let o01 = s0 | s1;
        (o01 | s2, (s0 & s1) | (o01 & s2)) // (≥1, ≥2) over this band's three rows
    };
    let (o0, t0) = band_fold(live.band_at::<0>());
    let (o1, t1) = band_fold(live.band_at::<1>());
    let (o2, t2) = band_fold(live.band_at::<2>());
    let ones = o0 | o1 | o2;
    let twos = t0 | t1 | t2 | (o0 & o1) | (o0 & o2) | (o1 & o2);
    ones & !twos
}

/// Clear digit `d` from every cell of each dropped triplet. A triplet `t = 3*a + g` is
/// band-row (or column-within-stack) `a` × box-column (or box-row) `g`; its three cells
/// sit at band positions `9*a + 3*g + j`. The [`Banding`] `B` maps each back to a cell —
/// the **same** physical cells whether `B` is `RowMajor` (box↔row drops, from
/// [`sweep_band_rm`]) or `ColMajor` (box↔column drops, from [`band_update_cols`]) — so
/// both orientations of triplet drop share one routine over the single row-major board.
fn drop_triplets<B: Banding>(b: &mut RowState, d: Digit, band: usize, mut dropped: u32) {
    while dropped != 0 {
        let t = dropped.trailing_zeros() as usize;
        dropped &= dropped - 1;
        let (a, g) = (t / 3, t % 3);
        for j in 0..3 {
            b.forbid(B::cell_at(band, a * 9 + 3 * g + j), d);
        }
    }
}

/// The discrete "harder than the cheap closure" ladder, gated by `allowed`: the
/// first subset, fish, or bivalue-wing technique that fires, or `None`. Reuses the
/// composable [`super::techniques`] bodies (they run on any board); singles and LC
/// are already drained by the fused closure, so only the subsets
/// (NakedPair..HiddenQuad), the basic fish (X-Wing..Jellyfish), and the wings
/// (XY-/XYZ-/W-Wing) remain. The try-order is THIS engine's choice (it follows
/// core's, un-optimized — not benchmarked); branch-scoped specs never put two Expert
/// branches in scope together, so their relative order is moot in production.
///
/// The harder techniques are read-heavy (a fish scans 9x9 cells per digit x2
/// orientations; the wings scan all 81 cells), and every read is a [`Marks::get`].
/// On the digit-major [`RowState`] each `get` is a 9-board candidate scan, so
/// running them on it directly is dominated by that gather (profiled: ~28% of the
/// w-wing+jellyfish generator in `get` alone). Instead transpose the stalled board
/// **once** into a cell-major [`CellBoard`] — `get` is then an O(1) [`Mark`] load —
/// run the ladder there, and replay the handful of eliminations the fired technique
/// logged back onto `b`. This is the scalar analogue of the SIMT baseline's
/// `ladder_step` / `CellMarks` transpose, identical verdict and elimination set (the
/// techniques are deterministic over the board contract; `forbid` is commutative, so
/// the replay order is moot).
#[cfg_attr(prof_solver, inline(never))]
fn step_harder(b: &mut RowState, allowed: KindMask) -> Option<usize> {
    let mut cb = CellBoard::from_row(b);
    let k = ladder(&mut cb, allowed)?;
    for &(cell, di) in &cb.elims[..cb.n_elim] {
        b.forbid(cell as CellIdx, Digit::from_index(di as usize));
    }
    Some(k)
}

/// The easiest-first try-ladder over any [`LogicBoard`], shared by [`step_harder`]'s
/// cell-major transpose. Returns the first kind whose technique eliminated something.
fn ladder<B: LogicBoard>(b: &mut B, allowed: KindMask) -> Option<usize> {
    macro_rules! try_kind {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    try_kind!(NAKED_PAIR, techniques::naked_subset(b, 2, None, None));
    try_kind!(HIDDEN_PAIR, techniques::hidden_subset(b, 2, None, None));
    try_kind!(NAKED_TRIPLE, techniques::naked_subset(b, 3, None, None));
    try_kind!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3, None, None));
    try_kind!(NAKED_QUAD, techniques::naked_subset(b, 4, None, None));
    try_kind!(HIDDEN_QUAD, techniques::hidden_subset(b, 4, None, None));
    if let Some(k) = techniques::fish_step(b, allowed, None) {
        return Some(k);
    }
    if let Some(k) = techniques::wing_step(b, allowed) {
        return Some(k);
    }
    None
}

/// Candidate-only **cell-major** scratch board for [`step_harder`]'s subset/fish/wing
/// ladder: each cell's [`Mark`] stored directly, so a technique's per-cell scan reads
/// it in O(1) rather than [`RowState`]'s digit-major 9-board `get`. Built once per stall
/// from the stalled row-major board ([`from_row`](CellBoard::from_row)), it logs the
/// eliminations the fired technique makes so [`step_harder`] can replay just those onto
/// the real board. The scalar twin of the SIMT baseline's `CellMarks`.
#[derive(Clone)]
struct CellBoard {
    marks: [Mark; 81],
    /// Log of `(cell, digit-index)` removed by [`eliminate`](CellBoard::eliminate); one
    /// firing of any harder technique removes at most a cover's worth of candidates (a
    /// subset over a nine-cell unit, a fish of `size` cover lines, or a wing's common
    /// peers — all well under 32), asserted in [`eliminate`](CellBoard::eliminate).
    elims: [(u8, u8); 32],
    n_elim: usize,
}

impl CellBoard {
    /// Transpose a stalled board's live candidates into per-cell marks. Walks each
    /// digit's row-major live-candidate band (`candidates & unsolved`) by set bit and
    /// inserts the digit at each cell — cheaper than 81 digit-major `get`s, and the only
    /// pass that touches the slow representation. A solved cell holds no candidate in any
    /// band, so it reads as [`Mark::EMPTY`] (matching `RowState::get`); since the closure
    /// has drained naked singles, every still-empty cell has >= 2 candidates, so
    /// `is_empty` can be answered straight off the marks (see [`Occupancy`]).
    fn from_row(row: &RowState) -> Self {
        let unsolved = row.unsolved();
        let mut marks = [Mark::EMPTY; 81];
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut rest = row.candidates()[d] & unsolved;
            while rest.any() {
                let c = rest.first();
                rest &= !<Bands<RowMajor> as GridMask>::cell(c);
                marks[c].insert(d);
            }
        }
        CellBoard { marks, elims: [(0, 0); 32], n_elim: 0 }
    }
}

impl Marks for CellBoard {
    fn from_digits(_: &crate::repr::DigitGrid) -> Self {
        unreachable!("CellBoard is built from a stalled dual board, not a digit grid")
    }
    fn place(&mut self, _: CellIdx, _: Digit) {
        unreachable!("the harder ladder only eliminates, never places")
    }
    #[inline]
    fn get(&self, cell: CellIdx) -> Mark {
        debug_assert!(cell < CELLS, "cell index {cell} out of range");
        // SAFETY: a CellIdx is 0..81 by construction; the bare usize can't carry that,
        // so this per-cell marks index otherwise emits a bounds check it can never hit.
        unsafe { core::hint::assert_unchecked(cell < CELLS) };
        self.marks[cell]
    }
}

impl Occupancy for CellBoard {
    /// Empty iff the cell still holds candidates: the closure drained naked singles
    /// before the stall, so every empty cell has >= 2 marks and every solved cell reads
    /// [`Mark::EMPTY`] — so non-empty marks exactly identify the unsolved cells.
    #[inline]
    fn is_empty(&self, cell: CellIdx) -> bool {
        debug_assert!(cell < CELLS, "cell index {cell} out of range");
        // SAFETY: cell is a CellIdx (0..81 by construction).
        unsafe { core::hint::assert_unchecked(cell < CELLS) };
        !self.marks[cell].is_empty()
    }
}

impl Eliminate for CellBoard {
    #[inline]
    fn eliminate(&mut self, cell: CellIdx, d: Digit) {
        debug_assert!(self.n_elim < self.elims.len(), "harder-step eliminations exceeded buffer");
        debug_assert!(cell < CELLS, "cell index {cell} out of range");
        // SAFETY: cell is a CellIdx (0..81 by construction).
        unsafe { core::hint::assert_unchecked(cell < CELLS) };
        self.elims[self.n_elim] = (cell as u8, d.index() as u8);
        self.n_elim += 1;
        self.marks[cell].remove(d);
    }
}

/// A band's 9-bit triplet occupancy: bit `3*r + k` set iff triplet (band-row `r`,
/// box-column `k`) holds any candidate — three [`OCC3`] loads off the band's three
/// 9-bit lines, the index [`DROP_TRIP`] reads.
#[inline]
fn triplet_occ(band: Band) -> usize {
    OCC3[band.line(0)] as usize
        | (OCC3[band.line(1)] as usize) << 3
        | (OCC3[band.line(2)] as usize) << 6
}

// --- precomputed lookup tables (ported verbatim from `bb::layout`) -------------

/// 9-bit line -> 3-bit triplet occupancy: bit `k` set iff the line's `k`-th 3-cell
/// triplet holds any candidate.
const OCC3: [u8; 512] = {
    let mut t = [0u8; 512];
    let mut v = 0usize;
    while v < 512 {
        let mut o = 0u8;
        if v & 0b000_000_111 != 0 {
            o |= 1;
        }
        if v & 0b000_111_000 != 0 {
            o |= 2;
        }
        if v & 0b111_000_000 != 0 {
            o |= 4;
        }
        t[v] = o;
        v += 1;
    }
    t
};

/// The triplets a band drops under the within-band locked-candidates fixpoint,
/// keyed on its 9-bit triplet occupancy: `occ & !keep`, precomputed. Nonzero only
/// for the rare occupancies that actually have an LC elimination. The same table
/// serves both orientations (box↔row off a row-major band's [`triplet_occ`], box↔column
/// off the column occupancy [`pack_col_triplet_occ`] folds from the same row board).
pub(crate) const DROP_TRIP: [u32; 512] = {
    let mut t = [0u32; 512];
    let mut occ = 0usize;
    while occ < 512 {
        // Fixpoint of within-band pointing + claiming over the surviving triplets.
        let mut keep = occ as u32;
        loop {
            let mut next = keep;
            // pointing: a box-column whose occupied triplets are a single band-row
            // clears that row's other two triplets.
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
            // claiming: a band-row whose occupied triplets are a single box-column
            // clears that box's other two triplets.
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
        t[occ] = occ as u32 & !keep;
        occ += 1;
    }
    t
};
