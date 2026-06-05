//! `BranchStrategy` — how the fill/search picks *which* cell to branch on when a
//! scan finds several still-undecided cells. The variable part of the MRV scan,
//! pulled out of [`crate::fill`] so the rule can be swapped by changing one type
//! parameter ([`Mrv`] today; a bivalue rule for the prober later) instead of
//! rewriting the scan.
//!
//! The shared scaffolding — the popcount-free candidate-count sieve, the
//! dead/solved verdicts, materializing the chosen cell's candidates — lives in the
//! free [`scan_with`] helper; each strategy's [`BranchStrategy::scan`] supplies only
//! its pick ([`Mrv::pick`] / [`Bivalue::pick`]), the choice among the live cells. It
//! is a usage layer: it reads a [`GridMask`] board to decide, not a representation.

use crate::repr::{Branchable, CellIdx, GridMask, PerDigit};
use crate::sieve::Sieve;

/// The outcome of a [`BranchStrategy::scan`] over the unsolved cells.
pub enum Scan {
    /// Every cell decided — the grid is full.
    Solved,
    /// Some unsolved cell has no candidate left — this branch is dead.
    Dead,
    /// Branch on `cell`; `candidates` bit `d` set => digit `d+1` may go there.
    Branch { cell: CellIdx, candidates: u16 },
}

/// A rule for choosing the branch cell when a scan stalls with several undecided
/// cells. A strategy that tunes its candidate-count sieve depth carries it as a const
/// parameter on the type ([`Mrv<D>`]); one with a fixed natural cap ([`Bivalue`])
/// hardcodes it. Either way the depth lives on the strategy rather than threading
/// through every downstream signature. The one method, [`scan`](BranchStrategy::scan),
/// delegates the shared scaffolding to [`scan_with`] and supplies only the pick.
pub trait BranchStrategy {
    /// One scan: build the strategy's depth sieve, return the dead/solved verdict,
    /// else this strategy's branch. Implemented by delegating to [`scan_with`] with
    /// the strategy's pick, so all the variation is the pick.
    fn scan<M: Branchable>(board: &PerDigit<M>, unsolved: M) -> Scan;
}

/// The shared scan scaffolding, parameterized by the sieve depth `D` and the
/// strategy's `pick`: build the depth-`D` sieve, return the dead/solved verdict, else
/// branch on the lowest cell of `pick`'s chosen set. `inline(always)`, not just
/// `inline`: the body is ~2 KB and the caller (`Fill::fill`) is recursive, so the
/// cost model otherwise leaves it out-of-line — a `call` per search node (~83/grid,
/// ~3% measured). Inlining it into `fill` recovers the hand-fused codegen and makes a
/// generic `Fill<M, S>` the same as a hand-written scan.
#[inline(always)]
fn scan_with<M: Branchable, const D: usize>(
    board: &PerDigit<M>,
    unsolved: M,
    pick: impl FnOnce(&PerDigit<M>, M, Sieve<M, D>) -> M,
) -> Scan {
    let tiers = Sieve::<M, D>::compute(board, unsolved);
    if (unsolved & tiers.dead()).any() {
        return Scan::Dead; // some unsolved cell has no candidate
    }
    if !unsolved.any() {
        return Scan::Solved; // every cell decided
    }
    branch(board, pick(board, unsolved, tiers))
}

/// Materialize the branch on the lowest cell of `tier`: gather its candidate digits
/// from the nine per-digit boards (slot `d` set => digit `d+1`). Needs
/// [`Branchable`] for `tier.first()`.
#[inline]
fn branch<M: Branchable>(board: &PerDigit<M>, tier: M) -> Scan {
    let cell = tier.first();
    let cb = M::cell(cell);
    let mut candidates = 0u16;
    for (d, &m) in board.each().iter().enumerate() {
        if (m & cb).any() {
            candidates |= 1 << d;
        }
    }
    Scan::Branch { cell, candidates }
}

/// Minimum-remaining-values: branch on a cell with the fewest candidates (ties →
/// lowest index). The fill's rule — taking the most-constrained cell first keeps
/// the search shallow and reproduces core's node order. The lowest non-empty
/// exactly-`k` tier is the minimum count; its lowest cell is the lowest-index cell
/// achieving it, byte-identical to the scalar fill's `n < bn` pick.
///
/// `D` is the sieve-depth cap. Default four because 82.8% of fill scans have a
/// minimum count ≤ 3, so the cap resolves the pick directly on all but the rare
/// all-`≥4` node; a depth-4 sieve is 7 ops/digit vs the full 17. The fill can
/// override it (`Mrv<D>`) and the prober bench sweeps it.
pub struct Mrv<const D: usize = 4>;

impl<const D: usize> BranchStrategy for Mrv<D> {
    #[inline]
    fn scan<M: Branchable>(board: &PerDigit<M>, unsolved: M) -> Scan {
        scan_with::<M, D>(board, unsolved, Self::pick::<M>)
    }
}

impl<const D: usize> Mrv<D> {
    /// The capped-MRV tier; on the rare all-`≥D` node, the true minimum from a
    /// recomputed full-depth sieve.
    #[inline]
    fn pick<M: GridMask>(board: &PerDigit<M>, unsolved: M, t: Sieve<M, D>) -> M {
        if let Some(tier) = capped_min_tier::<M, D>(&t) {
            return tier;
        }
        // Rare: every unsolved cell has ≥ D candidates (the capped tier was hit).
        // Recompute the full depth-9 sieve and take the lowest tier from D upward.
        let full = Sieve::<M, 9>::compute(board, unsolved);
        for k in D..=9 {
            let tier = full.exactly(k);
            if tier.any() {
                return tier;
            }
        }
        unreachable!("unsolved non-empty but no tier matched")
    }
}

/// The lowest non-empty exactly-`k` tier within the cap (`1 ≤ k < D`), or `None` if
/// every unsolved cell has `≥ D` candidates (the capped top tier was hit). The MRV
/// core shared by [`Mrv`] (which then recomputes a full sieve for the true minimum)
/// and [`LooseMrv`] (which gives up there and lets the branch take whatever). `D` is
/// a const, so the loop unrolls back to the hand-fused `exactly(1); exactly(2); …`
/// chain; the returned tier's lowest cell is the lowest-index cell achieving the
/// minimum count, byte-identical to the scalar fill's `n < bn` pick.
#[inline]
fn capped_min_tier<M: GridMask, const D: usize>(t: &Sieve<M, D>) -> Option<M> {
    for k in 1..D {
        let tier = t.exactly(k);
        if tier.any() {
            return Some(tier);
        }
    }
    None
}

/// Capped minimum-remaining-values: like [`Mrv`] within the depth-`D` sieve, but on
/// the rare all-`≥D` node it does *not* recompute a full-depth sieve to rank the
/// over-cap cells — it gives up on MRV there and lets the branch take whatever's
/// lowest, exactly as [`Bivalue`] falls back. The bet is that the full-sieve
/// recompute costs more than the slightly-worse branch choice on those rare nodes
/// buys back; benched head-to-head against [`Mrv`].
///
/// As a *prober* it is verdict-identical to every other strategy — the completion
/// count is pick-independent (completeness comes from the branch) — so it strips to
/// the same puzzles; only the search *order* (and thus a fill's grid) differs.
pub struct LooseMrv<const D: usize = 4>;

impl<const D: usize> BranchStrategy for LooseMrv<D> {
    #[inline]
    fn scan<M: Branchable>(board: &PerDigit<M>, unsolved: M) -> Scan {
        scan_with::<M, D>(board, unsolved, Self::pick::<M>)
    }
}

impl<const D: usize> LooseMrv<D> {
    /// The capped-MRV tier, else any unsolved cell (let the branch handle it).
    #[inline]
    fn pick<M: GridMask>(_board: &PerDigit<M>, unsolved: M, t: Sieve<M, D>) -> M {
        capped_min_tier::<M, D>(&t).unwrap_or(unsolved)
    }
}

/// Minimum-remaining-values that, on the rare all-`≥D` node, does *not* recompute a
/// full-depth sieve: it walks just the over-cap cells and counts each one's
/// candidates directly, keeping the lowest. It picks the byte-identical branch cell
/// to [`Mrv`] (same verdict, same search order), so it is a pure performance A/B of
/// "recount the stuck cells" vs "recompute the whole sieve" for the over-cap policy.
///
/// Expected to lose — the per-cell `any()` walk forfeits the sieve's branchless,
/// all-cells-at-once SIMD, the very thing the recompute keeps — but the trait makes
/// the comparison cheap to run, and the over-cap node is rare enough that it might
/// not matter at production depths.
pub struct MrvRecount<const D: usize = 4>;

impl<const D: usize> BranchStrategy for MrvRecount<D> {
    #[inline]
    fn scan<M: Branchable>(board: &PerDigit<M>, unsolved: M) -> Scan {
        scan_with::<M, D>(board, unsolved, Self::pick::<M>)
    }
}

impl<const D: usize> MrvRecount<D> {
    /// The capped-MRV tier; on the all-`≥D` node, the lowest-count over-cap cell
    /// found by a direct per-cell recount (no full-sieve recompute).
    #[inline]
    fn pick<M: Branchable>(board: &PerDigit<M>, unsolved: M, t: Sieve<M, D>) -> M {
        if let Some(tier) = capped_min_tier::<M, D>(&t) {
            return tier;
        }
        // Every unsolved cell has ≥ D candidates. Walk them in index order and count
        // each cell's candidates across the nine boards, keeping the lowest count.
        // The strict `<` over the ascending walk keeps the lowest-index cell on ties,
        // so the chosen cell is byte-identical to Mrv's full-recompute pick.
        let mut best = unsolved;
        let mut best_count = usize::MAX;
        let mut rest = unsolved;
        while rest.any() {
            let cb = M::cell(rest.first());
            rest &= !cb;
            let mut count = 0;
            for &cand in board.each() {
                if (cand & cb).any() {
                    count += 1;
                }
            }
            if count < best_count {
                best_count = count;
                best = cb;
            }
        }
        best
    }
}

/// Bivalue: branch on a cell with exactly two candidates if one exists, else the
/// lowest unsolved cell. The existence prober's rule — after propagation the board
/// is stuck, so a bivalue branch holds the branching factor at two. For the *fill*
/// it is a poor heuristic (early on no cell is bivalue, so it degenerates toward
/// naive lowest-index branching — MRV is the fill's rule); it lives here to exercise
/// the swappable [`BranchStrategy`] and as the rule the prober will reuse.
///
/// Unlike [`Mrv`], it carries no depth knob: it only ever reads `exactly(2)`, so a
/// depth-3 sieve is both the minimum for a precise count and all it can use — there
/// is nothing to tune, so the depth is hardcoded rather than a type parameter.
pub struct Bivalue;

impl BranchStrategy for Bivalue {
    #[inline]
    fn scan<M: Branchable>(board: &PerDigit<M>, unsolved: M) -> Scan {
        scan_with::<M, 3>(board, unsolved, Self::pick::<M>)
    }
}

impl Bivalue {
    /// The bivalue cells, else any unsolved cell.
    #[inline]
    fn pick<M: GridMask>(_board: &PerDigit<M>, unsolved: M, t: Sieve<M, 3>) -> M {
        // The sieve tiers are already restricted to unsolved cells, so `exactly(2)`
        // is exactly the bivalue cells (depth 3 makes that count precise). Fallback:
        // any unsolved cell (non-empty here — `scan` returned neither Solved nor Dead).
        let bivalue = t.exactly(2);
        if bivalue.any() { bivalue } else { unsolved }
    }
}
