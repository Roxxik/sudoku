//! `FusedLogicSolver` — the **fast path** of the logic solver: bb's dual-view fused
//! band closure ported onto the [`DualSolverState`]. The performance counterpart
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
//! Every unit is in-lane in at least one of the two bandings, so the row-major sweep
//! ([`band_update_rm`]) covers rows + boxes + box↔row LC and the column-major sweep
//! ([`band_update_cm`]) covers columns + box↔column LC; together with the
//! naked-single sieve they reach the full {singles, locked-candidates} fixpoint
//! ([`propagate`]). When that stalls, the discrete subset ladder advances one step —
//! reusing the *composable* [`super::techniques`] subsets, which already run on the
//! dual grid — and the closure re-runs. This is exactly bb's `baseline_fast`.
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

use super::{LogicSolver, Solver, techniques};
use crate::counters::counter_block;
use crate::repr::banded::{Band, Banding, Bands, ColMajor, DualSolverState, RowMajor};
use crate::repr::{Digit, GridMask, Marks};
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
//   [2] total subset steps that fired (step_subsets -> Some)
//   [3] calls that fired >= 1 subset step (i.e. reached past the cheap closure)
//   [4] calls that solved (solved == true)
//   [5] calls with counts[HIDDEN_QUAD] >= 1 (the Forced kind actually fired)
//   [6] total step_subsets invocations (incl. the None that ends an unsolvable stall)
//   [10+k] per-kind k firing totals (cheap kinds 0..4 are fired-or-not, <=1/call;
//          subset kinds 4..10 are exact)
counter_block!(FSTAT: 20, inc = fstat_inc, add = fstat_add, snapshot = fstat_snapshot, reset = fstat_reset);

/// The fused fast-path logic solver over the dual-banded grid. Stateless marker, like
/// [`LogicSolver`](super::LogicSolver); see the module docs for its precondition.
pub struct FusedLogicSolver;

impl Solver<DualSolverState> for FusedLogicSolver {
    fn solve_tracked(board: &DualSolverState, allowed: KindMask) -> SolveTrace {
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
    fn min_target_uses(board: &DualSolverState, scope: KindMask, target: KindMask) -> usize {
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
fn fused_solve<const LC: bool>(board: &DualSolverState, allowed: KindMask) -> SolveTrace {
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
        match step_subsets(&mut b, allowed) {
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
fn propagate<const LC: bool>(b: &mut DualSolverState, fired: &mut u32) -> Prop {
    loop {
        match drain_naked_singles(b, fired) {
            Prop::Stuck => {}
            terminal => return terminal,
        }
        let mut changed = band_update_rm::<LC>(b, fired);
        changed |= band_update_cm::<LC>(b, fired);
        if !changed {
            return Prop::Stuck;
        }
    }
}

/// Place every naked single in bit-parallel waves until none remain, classifying the
/// board each pass. The depth-2 raw sieve over the row view surfaces dead cells
/// (`unsolved & dead`), the solved verdict (`!unsolved`), and the singles
/// (`unsolved & exactly(1)`) popcount-free; placement syncs both views. On a
/// uniquely-solvable board naked singles are forced and never conflict, so a wave
/// places them all without a contradiction check.
fn drain_naked_singles(b: &mut DualSolverState, fired: &mut u32) -> Prop {
    loop {
        let sieve = Sieve::<Bands<RowMajor>, 2>::compute_raw(b.row().candidates());
        let unsolved = b.row().unsolved();
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
            let group = singles & b.row().candidates()[d];
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
fn band_update_rm<const LC: bool>(b: &mut DualSolverState, fired: &mut u32) -> bool {
    let mut changed = false;
    for band in 0..3 {
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut live = (b.row().candidates()[d] & b.row().unsolved()).band(band);
            if LC {
                let dropped = DROP_TRIP[triplet_occ(live)];
                if dropped != 0 {
                    changed = true;
                    *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
                    drop_triplets::<RowMajor>(b, d, band, dropped);
                    live = (b.row().candidates()[d] & b.row().unsolved()).band(band);
                }
            }
            // Hidden singles in the three rows (each a contiguous 9-bit run).
            for line in 0..3 {
                if let Some(slot) = lone(live.line(line)) {
                    b.place(RowMajor::cell_at(band, Band::line_pos(line, slot)), d);
                    changed = true;
                    *fired |= 1 << HIDDEN_SINGLE;
                    live = (b.row().candidates()[d] & b.row().unsolved()).band(band);
                }
            }
            // Hidden singles in the three boxes (each gathered into a 9-bit run).
            for k in 0..3 {
                if let Some(slot) = lone(live.box_unit(k)) {
                    b.place(RowMajor::cell_at(band, Band::box_pos(k, slot)), d);
                    changed = true;
                    *fired |= 1 << HIDDEN_SINGLE;
                    live = (b.row().candidates()[d] & b.row().unsolved()).band(band);
                }
            }
        }
    }
    changed
}

/// One fused column-major sweep: the transpose of [`band_update_rm`] on the column
/// view, giving box↔column locked candidates and hidden singles in the three
/// columns. Boxes are already covered row-major, so only the lines (columns) are
/// swept. Mirror of bb's `band_update_cm`.
fn band_update_cm<const LC: bool>(b: &mut DualSolverState, fired: &mut u32) -> bool {
    let mut changed = false;
    for band in 0..3 {
        for di in 0..9 {
            let d = Digit::from_index(di);
            let mut live = (b.col().candidates()[d] & b.col().unsolved()).band(band);
            if LC {
                let dropped = DROP_TRIP[triplet_occ(live)];
                if dropped != 0 {
                    changed = true;
                    *fired |= (1 << LC_POINTING) | (1 << LC_CLAIMING);
                    drop_triplets::<ColMajor>(b, d, band, dropped);
                    live = (b.col().candidates()[d] & b.col().unsolved()).band(band);
                }
            }
            for line in 0..3 {
                if let Some(slot) = lone(live.line(line)) {
                    b.place(ColMajor::cell_at(band, Band::line_pos(line, slot)), d);
                    changed = true;
                    *fired |= 1 << HIDDEN_SINGLE;
                    live = (b.col().candidates()[d] & b.col().unsolved()).band(band);
                }
            }
        }
    }
    changed
}

/// Clear digit `d` from every cell of each dropped triplet, in both views. A
/// triplet `t = 3*a + g` is band-row (or column-within-stack) `a` × box-column (or
/// box-row) `g`; its three cells sit at band positions `9*a + 3*g + j`. The
/// [`Banding`] `B` maps each back to a cell, and [`DualSolverState::forbid`]
/// clears it in both bandings — so the row-major and column-major triplet drops
/// share one routine.
fn drop_triplets<B: Banding>(b: &mut DualSolverState, d: Digit, band: usize, mut dropped: u32) {
    while dropped != 0 {
        let t = dropped.trailing_zeros() as usize;
        dropped &= dropped - 1;
        let (a, g) = (t / 3, t % 3);
        for j in 0..3 {
            b.forbid(B::cell_at(band, a * 9 + 3 * g + j), d);
        }
    }
}

/// The discrete subset ladder, easiest-first, gated by `allowed`: the first subset
/// technique that fires, or `None`. Reuses the composable [`super::techniques`]
/// subsets (they run on any board, the dual grid included); singles and LC are
/// already drained by the fused closure, so only NakedPair..HiddenQuad remain.
fn step_subsets(b: &mut DualSolverState, allowed: KindMask) -> Option<usize> {
    macro_rules! try_subset {
        ($bit:expr, $call:expr) => {
            if allowed & (1 << $bit) != 0 && $call {
                return Some($bit);
            }
        };
    }
    try_subset!(NAKED_PAIR, techniques::naked_subset(b, 2));
    try_subset!(HIDDEN_PAIR, techniques::hidden_subset(b, 2));
    try_subset!(NAKED_TRIPLE, techniques::naked_subset(b, 3));
    try_subset!(HIDDEN_TRIPLE, techniques::hidden_subset(b, 3));
    try_subset!(NAKED_QUAD, techniques::naked_subset(b, 4));
    try_subset!(HIDDEN_QUAD, techniques::hidden_subset(b, 4));
    None
}

/// The lone candidate's slot in a 9-bit unit mask, or `None` for zero or more than
/// one candidate — one [`SINGLE9`] load.
#[inline]
fn lone(unit: usize) -> Option<usize> {
    let s = SINGLE9[unit];
    (s != 0xFF).then_some(s as usize)
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

/// Hidden-single lookup: the lone bit's index of a 9-bit unit mask, else `0xFF`.
const SINGLE9: [u8; 512] = {
    let mut t = [0xFFu8; 512];
    let mut v = 1usize;
    while v < 512 {
        if v & (v - 1) == 0 {
            t[v] = v.trailing_zeros() as u8;
        }
        v += 1;
    }
    t
};

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
/// serves both views (box↔row in row-major bands, box↔column in column-major).
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
