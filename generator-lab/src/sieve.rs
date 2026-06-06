//! `Sieve<M, D>` — the depth-generic candidate-count sieve, the fill scan's core.
//!
//! A depth-generic unification of the hand-rolled candidate-count sieves that used
//! to exist at fixed depths:
//!   - the depth-2 naked-single `Sieve { ones, twos }` (`bb::sieve`, the prober),
//!   - the fill's MRV cap (was the depth-4 `Tiers { ones..fours }` in `scan`),
//!   - the depth-9 `[M; 11]` fallback array that lived inside `scan::Mrv::pick`.
//!
//! All run the identical symmetric sieve — for each per-digit candidate board `b`,
//! push `tier[k] |= tier[k-1] & b` from the top down, then `tier[0] |= b` —
//! differing only in how many levels they keep. That count is a compile-time
//! constant everywhere (a fixed list of named fields, or an array literal), never
//! a runtime value, so it lifts cleanly to a const generic `D`. With `D` known the
//! `for k in (1..D).rev()` loop has a static trip count and unrolls back into the
//! hand-fused `tiers[3] |= tiers[2] & b; ...` chain — the same codegen the depth-4
//! `Tiers` emitted, now monomorphized per call site.
//!
//! Each [`scan::BranchStrategy`](crate::scan::BranchStrategy) carries its sieve depth
//! `D` and resolves a `Sieve<M, D>` per node; the fill's [`Mrv`](crate::scan::Mrv)
//! picks from the exact tiers `1..D` and recomputes a depth-9 sieve on the rare
//! all-`>=D` node.
//!
//! Cap semantics: `D` is the deepest level resolved. A cell with `>= D` candidates
//! reads as [`exactly`](Sieve::exactly)`(D)` — the top tier lumps "D or more"
//! together. To distinguish deeper counts, compute a deeper `Sieve` (this is what
//! `Mrv` does: depth-`D` fast path, then a depth-9 recompute on the rare all-`>=D`
//! scan).
//!
//! NOTE vs the depth-2 prober sieve: that one sieves the *raw* candidate boards,
//! trusting the prober's board to hold candidates only on live cells, so it pays no
//! `& unsolved` per digit. This one masks (matching the fill's old `Tiers`); an
//! unmasked constructor would be the prober-path variant. Folding both without
//! regressing the prober's hot loop is the open integration question — so the
//! prober keeps its own `bb::sieve` for now.

use crate::repr::{GridMask, PerDigit};

/// The candidate-count sieve over the nine per-digit boards, resolved to `D`
/// levels. `tiers[i]` is the set of (masked) cells with **at least `i + 1`**
/// candidates, so `tiers[0]` ⊇ `tiers[1]` ⊇ … ⊇ `tiers[D-1]`. The cells with
/// *exactly* `k` candidates are `at_least(k) & !at_least(k + 1)`, with
/// `at_least(D + 1)` treated as empty (the cap).
#[derive(Clone, Copy)]
pub struct Sieve<M, const D: usize> {
    /// `tiers[i]` = cells with at least `i + 1` candidates. `D` must be `>= 1`.
    pub tiers: [M; D],
}

impl<M: GridMask, const D: usize> Sieve<M, D> {
    /// The symmetric sieve over `grid`, each digit board masked by `unsolved`.
    /// `D - 1` AND-OR pairs per digit (`D = 4` → 7 ops/digit, the `Tiers` cost).
    #[inline]
    pub fn compute(grid: &PerDigit<M>, unsolved: M) -> Self {
        let mut tiers = [M::EMPTY; D];
        for &candidates in grid.each() {
            let mask = candidates & unsolved;
            // Top-down so each level reads the previous level's *pre-update* state.
            for k in (1..D).rev() {
                tiers[k] |= tiers[k - 1] & mask;
            }
            tiers[0] |= mask;
        }
        Sieve { tiers }
    }

    /// The symmetric sieve over the **raw** per-digit boards — no `& unsolved` per
    /// digit (bb's prober sieve). A decided cell carries stale candidate bits, so its
    /// tier counts are garbage; this is sound *only* where the consumer re-masks the
    /// result with `unsolved`. The prober's naked-single drain does exactly that
    /// (`unsolved & exactly(1)`), so it drops the one AND per digit that [`compute`]
    /// pays. The scan's MRV pick reads tiers directly (unmasked) to rank cells, so it
    /// must keep [`compute`].
    #[inline]
    pub fn compute_raw(grid: &PerDigit<M>) -> Self {
        let mut tiers = [M::EMPTY; D];
        for &mask in grid.each() {
            for k in (1..D).rev() {
                tiers[k] |= tiers[k - 1] & mask;
            }
            tiers[0] |= mask;
        }
        Sieve { tiers }
    }

    /// Cells with at least `k` candidates. `k = 0` → all (vacuous); `k > D` →
    /// empty (the cap). For `1 <= k <= D` this is the stored `tiers[k - 1]`.
    #[inline]
    pub fn at_least(&self, k: usize) -> M {
        if k == 0 {
            M::FULL
        } else if k <= D {
            self.tiers[k - 1]
        } else {
            M::EMPTY
        }
    }

    /// Cells with exactly `k` candidates — for `k = D` this is the capped tier
    /// (cells with `D` *or more*), since `at_least(D + 1)` is empty.
    #[inline]
    pub fn exactly(&self, k: usize) -> M {
        self.at_least(k) & !self.at_least(k + 1)
    }

    /// The masked cells with no candidate at all — `!at_least(1)`. (A scan reads
    /// `unsolved & dead()` to detect a dead branch.)
    #[inline]
    pub fn dead(&self) -> M {
        !self.at_least(1)
    }
}
