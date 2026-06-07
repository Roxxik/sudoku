//! `SolverState<M>` — the digit-major candidate board: per digit, the cells it may
//! still occupy ([`PerDigit<M>`]), plus the still-empty cell mask. The state every
//! search and solver works on — the fill, the existence/uniqueness prober, and (via
//! its two bandings) the baseline's dual view — generic over the [`GridMask`]
//! packing `M`.
//!
//! It is the transpose of the cell-major [`MarkGrid`](super::MarkGrid): candidates
//! stored *by digit* (cheap `place` = two masked ops, popcount-free sieves) rather
//! than a [`Mark`] per cell. At `M = FlatGridMask` it is the flat reference; at
//! `M = Bands<RowMajor>` the row-major banded board the prober branches on; at
//! `M = Bands<ColMajor>` the column view (a `GridMask` but not `Branchable` — the
//! search never branches it). A [`super::banded::DualSolverState`] is just the
//! row and column `SolverState`s held together.
//!
//! A decided cell's stale bits in the per-digit boards are never cleared; `unsolved`
//! gates them everywhere, so the candidates of *unsolved* cells stay exactly the
//! naked candidates a scalar `MarkGrid` holds — the same convention the fill uses.
//! It is `Clone` but not `Copy`: a search branch duplicates ~160 bytes, and making
//! that an explicit `.clone()` keeps the per-branch cost visible (the cell-set
//! primitives it's built from are the small `Copy` values).

use super::{Branchable, CELLS, CellIdx, Digit, DigitGrid, GridMask, Mark, Marks, Occupancy, PerDigit};

/// Per-digit candidate cells in packing `M`, plus the empty-cell mask gating them.
#[derive(Clone, PartialEq, Eq)]
pub struct SolverState<M: GridMask> {
    candidates: PerDigit<M>,
    unsolved: M,
}

impl<M: GridMask> SolverState<M> {
    /// The per-digit candidate cell-sets — what a [`scan`](crate::scan) reads.
    #[inline]
    pub fn candidates(&self) -> &PerDigit<M> {
        &self.candidates
    }

    /// The still-empty cells — the gate over [`candidates`](SolverState::candidates).
    #[inline]
    pub fn unsolved(&self) -> M {
        self.unsolved
    }

    /// Forbid digit `d` at `cell` — drop `cell` from `d`'s candidate board, leaving
    /// the cell unsolved with one fewer candidate. The uniqueness gate's lever: after
    /// removing a clue whose true digit is `d`, forbidding `d` restricts the cell to
    /// its *alternates*, so an existence probe of the restricted state asks "does some
    /// other digit also complete?" — i.e. did stripping the clue lose uniqueness —
    /// without re-deriving the known `d`-completion (bb's `any_alt_solves`).
    #[inline]
    pub fn forbid(&mut self, cell: CellIdx, d: Digit) {
        self.candidates[d] &= !M::cell(cell);
    }

    /// Place a wave of naked singles whose lone candidate is `d` (`group`) given the
    /// already-accumulated `peers` mask (the union of the group cells' peer masks):
    /// decide the non-conflicting singles and forbid `d` across every peer, in two
    /// masked ops — bb's `place_singles` tail. Split from
    /// [`place_single_group`](SolverState::place_single_group) so a caller that
    /// enumerates the cells itself (e.g. the dual grid, whose column view is not
    /// [`Branchable`] and so cannot walk the set) can still batch the placement.
    ///
    /// Two singles of `d` that are peers is a contradiction: such a cell lands in
    /// `peers` (a peer excludes only itself), so it is left out of the decided set and
    /// `&= !peers` strips its lone candidate, leaving it unsolved with zero candidates
    /// for the next [`Sieve`](crate::scan::sieve) to read as dead.
    #[inline]
    pub fn place_group_with(&mut self, d: Digit, group: M, peers: M) {
        self.unsolved &= !(group & !peers);
        self.candidates[d] &= !peers;
    }

    /// Reopen `cell` (now empty) with naked candidate set `cand`: mark it unsolved, then
    /// per digit set its candidate bit at `cell` iff it survives, else clear it. The
    /// per-view body of a clue clear — the survival set `cand` is banding-independent (a
    /// property of the clue map), so the dual grid computes it once and applies it to
    /// each view with that view's `cellmask`.
    #[inline]
    pub(crate) fn open_cell(&mut self, cellmask: M, cand: Mark) {
        self.unsolved |= cellmask;
        for e in 0..9 {
            let dig = Digit::from_index(e);
            if cand.contains(dig) {
                self.candidates[dig] |= cellmask;
            } else {
                self.candidates[dig] &= !cellmask;
            }
        }
    }

    /// Reopen digit `d` on a just-cleared cell's empty peers — `peers` that are still
    /// empty and not `blocked` (the cells some other present `d`-clue still forbids `d`
    /// on). The per-view body of the clue clear's digit reopen; the dual grid supplies
    /// each view's `peers`/`blocked`.
    #[inline]
    pub(crate) fn open_digit_on_peers(&mut self, d: Digit, peers: M, blocked: M) {
        self.candidates[d] |= peers & self.unsolved & !blocked;
    }

    /// Decide `cell` as digit `d` (restore a clue): out of `unsolved`, every candidate
    /// bit at `cell` cleared, `d` removed from `peers`. The per-view body of a clue
    /// restore.
    #[inline]
    pub(crate) fn close_cell(&mut self, cellmask: M, d: Digit, peers: M) {
        self.unsolved &= !cellmask;
        for e in 0..9 {
            self.candidates[Digit::from_index(e)] &= !cellmask;
        }
        self.candidates[d] &= !peers;
    }
}

/// The incremental clue board — the strip's persistent state, mutated in place across
/// the 81 removal attempts instead of rebuilt with [`from_digits`](Marks::from_digits)
/// each step. bb's `apply_clear`/`apply_place` on the new representation: a `clue` map
/// (which cell holds each digit — the one thing the candidate boards can't reconstruct,
/// since a decided cell's candidate bits are stale) rides alongside, and a clue is
/// removed or restored with band ops over the cell and its peers, not an 81-cell sweep.
///
/// The set-bit walk in [`clear_clue`](SolverState::clear_clue)'s peer-reopen needs the
/// lowest set cell, so this rides on [`Branchable`] (the row-major prober packing and
/// the flat reference, both of which the strip uses).
impl<M: Branchable> SolverState<M> {
    /// Place a wave of naked singles that all take digit `d` — `group` is the cells
    /// whose sole candidate is `d`. Batched after bb's `place_singles`: accumulate the
    /// group's peer mask once, decide the cells, and forbid `d` across all their peers
    /// in a single board op, instead of a `place` (with its per-cell digit search) each.
    ///
    /// Two singles of digit `d` that are peers is a contradiction. Such a cell lands in
    /// the accumulated peer mask (a peer excludes only *itself*), so it is left out of
    /// the decided set and `&= !peers` strips `d` — its lone candidate — leaving it
    /// unsolved with zero candidates for the next [`Sieve`](crate::scan::sieve) to read as
    /// dead. (Propagation reports no verdict; the following scan does.)
    pub fn place_single_group(&mut self, d: Digit, group: M) {
        let mut peers = M::EMPTY;
        let mut rest = group;
        while rest.any() {
            let c = rest.first();
            rest &= !M::cell(c);
            peers |= M::peers(c);
        }
        self.place_group_with(d, group, peers);
    }

    /// The per-digit clue cells of `digits` — the seed `clue` map for an incremental
    /// strip, paired with a `from_digits` of the same grid.
    pub fn clue_map(digits: &DigitGrid) -> PerDigit<M> {
        let mut clue = PerDigit::new([M::EMPTY; 9]);
        for cell in 0..CELLS {
            if let Some(d) = digits.get(cell) {
                clue[d] |= M::cell(cell);
            }
        }
        clue
    }

    /// Remove the clue digit `d` at `cell`, reopening candidates — the inverse of
    /// [`place_clue`](SolverState::place_clue). Mirrors bb's `apply_clear`: drop `cell`
    /// from `clue[d]`, then (1) make `cell` empty with its naked candidates — digit `e`
    /// survives iff no still-present peer holds it — and (2) return `d` to every empty
    /// peer no *other* present `d`-clue still blocks (the union of the surviving
    /// `d`-clues' peer masks). All band ops; keeps `self == from_digits(digits)`.
    ///
    /// Returns `cell`'s naked candidates (computed in step 1) — the strip's `alts`
    /// source, so it reads them off the reopen instead of a second `get(cell)` scan.
    pub fn clear_clue(&mut self, clue: &mut PerDigit<M>, cell: CellIdx, d: Digit) -> Mark {
        let cb = M::cell(cell);
        let pm = M::peers(cell);
        clue[d] &= !cb; // cell stops being a clue before peer occupancy is read

        // (1) cell -> empty; candidate digit `e` survives iff no present peer holds it.
        let mut cand = Mark::EMPTY;
        for e in 0..9 {
            let dig = Digit::from_index(e);
            if !(clue[dig] & pm).any() {
                cand.insert(dig);
            }
        }
        self.open_cell(cb, cand);

        // (2) reopen `d` on cell's empty peers outside the union of the other present
        // `d`-clues' peer masks (the cells those clues still forbid `d` on).
        let mut blocked = M::EMPTY;
        let mut rest = clue[d];
        while rest.any() {
            let q = rest.first();
            rest &= !M::cell(q);
            blocked |= M::peers(q);
        }
        self.open_digit_on_peers(d, pm, blocked);
        cand
    }

    /// Restore the clue digit `d` at `cell` — the strip's revert when removing it lost
    /// uniqueness. Mirrors bb's `apply_place`: `cell` becomes decided (out of
    /// `unsolved`, all its candidate bits cleared), `d` leaves every peer, and `cell`
    /// is a `d`-clue again.
    pub fn place_clue(&mut self, clue: &mut PerDigit<M>, cell: CellIdx, d: Digit) {
        clue[d] |= M::cell(cell);
        self.close_cell(M::cell(cell), d, M::peers(cell));
    }
}

impl<M: GridMask> Marks for SolverState<M> {
    fn from_digits(digits: &DigitGrid) -> Self {
        let mut unsolved = M::EMPTY;
        for cell in 0..CELLS {
            if digits.is_empty(cell) {
                unsolved |= M::cell(cell);
            }
        }
        // Every empty cell could hold any digit; each placed digit then leaves the
        // empty peers it rules out. Decided cells are already out of `unsolved`.
        let mut board = PerDigit::new([unsolved; 9]);
        for cell in 0..CELLS {
            if let Some(d) = digits.get(cell) {
                board[d] &= !M::peers(cell);
            }
        }
        SolverState { candidates: board, unsolved }
    }

    #[inline]
    fn place(&mut self, cell: CellIdx, d: Digit) {
        self.unsolved &= !M::cell(cell);
        self.candidates[d] &= !M::peers(cell);
    }

    fn get(&self, cell: CellIdx) -> Mark {
        let cb = M::cell(cell);
        if !(self.unsolved & cb).any() {
            return Mark::EMPTY;
        }
        let mut m = Mark::EMPTY;
        for (i, &cand) in self.candidates.each().iter().enumerate() {
            if (cand & cb).any() {
                m.insert(Digit::from_index(i));
            }
        }
        m
    }
}

impl<M: GridMask> Occupancy for SolverState<M> {
    #[inline]
    fn is_empty(&self, cell: CellIdx) -> bool {
        (self.unsolved & M::cell(cell)).any()
    }
}

#[cfg(test)]
mod tests {
    use super::SolverState;
    use crate::repr::banded::{Bands, ColMajor, RowMajor};
    use crate::repr::{CELLS, Digit, DigitGrid, FlatGridMask, MarkGrid, Marks};

    const GRID: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    /// Every packing's candidate derivation + an incremental `place` must match the
    /// scalar `MarkGrid` cell for cell — flat, both bandings, the lot.
    fn agrees_with_scalar<M: crate::repr::GridMask>() {
        let grid = DigitGrid::parse(GRID).unwrap();
        let mut subject = SolverState::<M>::from_digits(&grid);
        let mut reference = MarkGrid::from_digits(&grid);
        let cell = 2;
        let d = Digit::new(1).unwrap();
        assert!(reference.get(cell).contains(d));
        subject.place(cell, d);
        reference.place(cell, d);
        for c in 0..CELLS {
            assert_eq!(subject.get(c), reference.get(c), "cell {c}");
        }
    }

    #[test]
    fn matches_scalar_across_packings() {
        agrees_with_scalar::<FlatGridMask>();
        agrees_with_scalar::<Bands<RowMajor>>();
        agrees_with_scalar::<Bands<ColMajor>>();
    }
}
