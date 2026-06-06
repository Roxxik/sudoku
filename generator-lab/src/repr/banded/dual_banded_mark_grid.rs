//! `DualBandedMarkGrid` — the candidates in *both* bandings at once: a row-major
//! [`SearchState`] (rows and boxes in-lane) and a column-major one (columns and
//! boxes in-lane), kept consistent at every mutation. The banded analogue of the
//! scalar [`MarkGrid`](super::super::MarkGrid) for the *baseline technique engine*,
//! mirroring the old dual-view `BitBoard`.
//!
//! ## Why two views
//!
//! A unit is cheap to scan only when it is *in-lane* — a contiguous run inside one
//! 27-bit band. Row-major puts rows and boxes in-lane but columns straddle all
//! three bands; column-major is the transpose. Holding both copies puts *every*
//! unit in-lane in at least one view, so the engine never has to scan a unit that
//! crosses bands. The lean prober needs only one view (it reaches columns by
//! branching, not by scanning), so it uses a single
//! [`SearchState<Bands<RowMajor>>`]; the baseline genuinely needs every unit and
//! pays for the second copy.
//!
//! Placements sync for free — each view clears its own peer mask — so maintaining
//! the pair is just doing the single-view mutation twice, once per banding. The
//! engine that *reads* the two views to run techniques lives a layer up.

use super::banding::{ColMajor, RowMajor};
use super::bands::Bands;
use crate::repr::{
    Branchable, CellIdx, Digit, DigitGrid, GridMask, Mark, Marks, Occupancy, PerDigit, SearchState,
};

/// Candidates held in both bandings, kept consistent. The two views always encode
/// the same candidate set (transposed), so reads answer from the row-major copy;
/// mutations touch both.
#[derive(Clone, PartialEq, Eq)]
pub struct DualBandedMarkGrid {
    row: SearchState<Bands<RowMajor>>,
    col: SearchState<Bands<ColMajor>>,
}

impl DualBandedMarkGrid {
    /// The row-major view (rows and boxes in-lane) — read access for the fused
    /// technique engine's per-band sweeps, which read one view's candidate bands a
    /// band at a time. Mutation still goes through [`place`](Marks::place) /
    /// [`forbid`](DualBandedMarkGrid::forbid), which keep both views in sync.
    #[inline]
    pub fn row(&self) -> &SearchState<Bands<RowMajor>> {
        &self.row
    }

    /// The column-major view (columns and boxes in-lane) — the transpose of
    /// [`row`](DualBandedMarkGrid::row), so column units are in-lane too.
    #[inline]
    pub fn col(&self) -> &SearchState<Bands<ColMajor>> {
        &self.col
    }

    /// Forbid digit `d` at `cell` in both views — the candidate-elimination
    /// primitive the technique engine needs (locked candidates and subsets prune
    /// without placing). The two views clear their own per-cell mask, so they stay
    /// in sync with no cross-view transpose.
    #[inline]
    pub fn forbid(&mut self, cell: CellIdx, d: Digit) {
        self.row.forbid(cell, d);
        self.col.forbid(cell, d);
    }

    /// Place a wave of naked singles whose lone candidate is digit `d` (`group`, a
    /// row-major cell set) into both views — bb's batched `place_singles`. ONE
    /// set-bit walk accumulates each view's group and peer masks (the column view is
    /// not [`Branchable`], so it can't walk its own set; the row-major group can,
    /// supplying the cells for both), then each view decides its cells in a single
    /// masked op via [`SearchState::place_group_with`] — no per-cell `place`.
    pub fn place_single_group(&mut self, d: Digit, group: Bands<RowMajor>) {
        let mut row_peers = <Bands<RowMajor> as GridMask>::EMPTY;
        let mut col_group = <Bands<ColMajor> as GridMask>::EMPTY;
        let mut col_peers = <Bands<ColMajor> as GridMask>::EMPTY;
        let mut rest = group;
        while rest.any() {
            let c = rest.first();
            rest &= !<Bands<RowMajor> as GridMask>::cell(c);
            row_peers |= <Bands<RowMajor> as GridMask>::peers(c);
            col_group |= <Bands<ColMajor> as GridMask>::cell(c);
            col_peers |= <Bands<ColMajor> as GridMask>::peers(c);
        }
        self.row.place_group_with(d, group, row_peers);
        self.col.place_group_with(d, col_group, col_peers);
    }

    /// The per-digit clue cells of `digits` — the row-major seed `clue` map for an
    /// incremental dual strip, paired with a `from_digits` of the same grid. The map is
    /// row-major because the clue-survival/blocked logic ([`clear_clue`]) reads it
    /// banding-independently; only the candidate boards are kept in both views.
    pub fn clue_map(digits: &DigitGrid) -> PerDigit<Bands<RowMajor>> {
        SearchState::<Bands<RowMajor>>::clue_map(digits)
    }

    /// Remove the clue digit `d` at `cell`, reopening candidates in **both** views — the
    /// dual analogue of [`SearchState::clear_clue`], so the strip can carry a single
    /// incrementally-maintained dual board (the baseline gate reads it with no per-gate
    /// rebuild) exactly as bb's `apply_clear` does. Mirrors bb: drop `cell` from the
    /// (row-major) `clue` map, reopen `cell`'s naked candidates, and return `d` to the
    /// empty peers no other present `d`-clue still blocks. The clue-survival decision and
    /// the blocked-cell set are banding-independent (computed once off the row clue map);
    /// each view then applies them with its own cell/peer masks. Returns `cell`'s naked
    /// candidates (the strip's `alts` source). Keeps both views `== from_digits(digits)`.
    pub fn clear_clue(
        &mut self,
        clue: &mut PerDigit<Bands<RowMajor>>,
        cell: CellIdx,
        d: Digit,
    ) -> Mark {
        let r_cell = <Bands<RowMajor> as GridMask>::cell(cell);
        let r_peers = <Bands<RowMajor> as GridMask>::peers(cell);
        clue[d] &= !r_cell; // cell stops being a clue before peer occupancy is read

        // (1) cell -> empty. Survival is a property of the clue map (does a present peer
        // hold the digit?), so it is the same set in both bandings — compute it once.
        let mut cand = Mark::EMPTY;
        for e in 0..9 {
            let dig = Digit::from_index(e);
            if !(clue[dig] & r_peers).any() {
                cand.insert(dig);
            }
        }
        self.row.open_cell(r_cell, cand);
        self.col.open_cell(<Bands<ColMajor> as GridMask>::cell(cell), cand);

        // (2) reopen `d` on cell's empty peers outside the union of the other present
        // `d`-clues' peer masks — accumulated per view from the same row-major clue cells.
        let mut r_blocked = <Bands<RowMajor> as GridMask>::EMPTY;
        let mut c_blocked = <Bands<ColMajor> as GridMask>::EMPTY;
        let mut rest = clue[d];
        while rest.any() {
            let q = rest.first();
            rest &= !<Bands<RowMajor> as GridMask>::cell(q);
            r_blocked |= <Bands<RowMajor> as GridMask>::peers(q);
            c_blocked |= <Bands<ColMajor> as GridMask>::peers(q);
        }
        self.row.open_digit_on_peers(d, r_peers, r_blocked);
        self.col
            .open_digit_on_peers(d, <Bands<ColMajor> as GridMask>::peers(cell), c_blocked);
        cand
    }

    /// Restore the clue digit `d` at `cell` in both views — the strip's revert, the dual
    /// analogue of [`SearchState::place_clue`] and the inverse of [`clear_clue`].
    pub fn place_clue(&mut self, clue: &mut PerDigit<Bands<RowMajor>>, cell: CellIdx, d: Digit) {
        clue[d] |= <Bands<RowMajor> as GridMask>::cell(cell);
        self.row.close_cell(
            <Bands<RowMajor> as GridMask>::cell(cell),
            d,
            <Bands<RowMajor> as GridMask>::peers(cell),
        );
        self.col.close_cell(
            <Bands<ColMajor> as GridMask>::cell(cell),
            d,
            <Bands<ColMajor> as GridMask>::peers(cell),
        );
    }
}

impl Occupancy for DualBandedMarkGrid {
    /// Answered from the row view's `unsolved` mask (both views agree).
    fn is_empty(&self, cell: CellIdx) -> bool {
        self.row.is_empty(cell)
    }
}

impl Marks for DualBandedMarkGrid {
    /// Derive both views from a digit grid — the same per-view derivation run once
    /// per banding.
    fn from_digits(grid: &DigitGrid) -> Self {
        DualBandedMarkGrid {
            row: SearchState::from_digits(grid),
            col: SearchState::from_digits(grid),
        }
    }

    /// Place digit `d` at `cell` in both views: decide the cell and forbid `d` on
    /// its peers in each banding. The peer masks are per-view, so the two stay in
    /// sync with no cross-view transpose.
    fn place(&mut self, cell: CellIdx, d: Digit) {
        self.row.place(cell, d);
        self.col.place(cell, d);
    }

    /// The naked candidate set of cell `cell`. Both views agree, so this reads the
    /// row-major copy.
    fn get(&self, cell: CellIdx) -> Mark {
        self.row.get(cell)
    }
}

#[cfg(test)]
mod tests {
    use super::DualBandedMarkGrid;
    use crate::repr::{CELLS, Digit, DigitGrid, MarkGrid, Marks};

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

    /// Both views must agree with each other AND match the scalar `MarkGrid`, on
    /// derivation and after an incremental `place`. Checking the column view (not
    /// just the row view `get` exposes) exercises the column-major banding and its
    /// peer masks.
    #[test]
    fn both_views_match_scalar_through_place() {
        let check = |dual: &DualBandedMarkGrid, scalar: &MarkGrid| {
            for cell in 0..CELLS {
                let s = scalar.get(cell);
                assert_eq!(dual.row.get(cell), s, "row cell {cell}");
                assert_eq!(dual.col.get(cell), s, "col cell {cell}");
            }
        };

        let grid = DigitGrid::parse(GRID).unwrap();
        let mut dual = DualBandedMarkGrid::from_digits(&grid);
        let mut scalar = MarkGrid::from_digits(&grid);
        check(&dual, &scalar);

        // Cell 2 (row 0, col 2) is empty in GRID; place a digit it admits.
        let cell = 2;
        let d = Digit::new(1).unwrap();
        assert!(scalar.get(cell).contains(d));
        dual.place(cell, d);
        scalar.place(cell, d);
        check(&dual, &scalar);
    }
}
