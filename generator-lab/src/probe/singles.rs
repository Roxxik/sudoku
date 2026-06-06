//! The composable completion search — the reference/fallback prober. It drives the
//! generic [`technique`](super::technique) singles (naked + hidden, over the
//! per-cell [`Marks`](crate::repr::Marks) contract) to a fixpoint, then branches.
//! Generic over any [`SolveView`] (candidates + occupancy + clone), so it runs on
//! the scalar [`Board`](crate::repr::Board), a [`SearchState`](crate::repr::SearchState),
//! or the dual banded grid.
//!
//! Slower than [`super::search`] (get-based, all 27 units, hand-rolled MRV branch),
//! but it uses the full technique set and is dead simple — the correctness oracle
//! the faster prober is checked against, and the "composable always works" baseline.
//! Completeness comes from the branch, so the techniques are pure pruning.

use crate::repr::{CELLS, CellIdx, SolveView};
use super::technique::{hidden_singles, naked_singles};

enum Status {
    Solved,
    Contradiction,
    Stuck,
}

/// Drive naked + hidden singles to a joint fixpoint, then report the verdict. A
/// `Contradiction` is an empty cell with zero candidates (missing-digit
/// contradictions surface here too, by pigeonhole).
fn propagate<V: SolveView>(view: &mut V) -> Status {
    loop {
        let changed = naked_singles(view) | hidden_singles(view);
        let mut open = false;
        for c in 0..CELLS {
            if view.is_empty(c) {
                open = true;
                if view.get(c).is_empty() {
                    return Status::Contradiction;
                }
            }
        }
        if !open {
            return Status::Solved;
        }
        if !changed {
            return Status::Stuck;
        }
    }
}

/// Minimum-remaining-values branch cell: the empty cell with the fewest candidates
/// (singles drained by [`propagate`], so ≥ 2). The caller guarantees an empty cell
/// exists (`Stuck`).
fn branch_cell<V: SolveView>(view: &V) -> CellIdx {
    let mut best = 0;
    let mut best_len = u32::MAX;
    for c in 0..CELLS {
        if view.is_empty(c) {
            let len = view.get(c).len();
            if len < best_len {
                best_len = len;
                best = c;
            }
        }
    }
    best
}

/// The number of completions of `view`, counting only up to `cap`.
pub fn count_completions<V: SolveView>(view: V, cap: usize) -> usize {
    let mut found = 0;
    count(view, cap, &mut found);
    found
}

fn count<V: SolveView>(mut view: V, cap: usize, found: &mut usize) {
    match propagate(&mut view) {
        Status::Contradiction => return,
        Status::Solved => {
            *found += 1;
            return;
        }
        Status::Stuck => {}
    }
    let cell = branch_cell(&view);
    for d in view.get(cell).iter() {
        let mut child = view.clone();
        child.place(cell, d);
        count(child, cap, found);
        if *found >= cap {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::count_completions;
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{Board, DigitGrid, Marks, SearchState};

    const PUZZLE: &str = "\
        53..7....\
        6..195...\
        .98....6.\
        8...6...3\
        4..8.3..1\
        7...2...6\
        .6....28.\
        ...419..5\
        ....8..79";

    /// The composable prober must give the same count as the scan/sieve one on the
    /// scalar `Board` and a banded `SearchState` — the cross-engine, cross-packing
    /// oracle.
    #[test]
    fn agrees_with_search_prober() {
        let grid = DigitGrid::parse(PUZZLE).unwrap();
        let board = Board::from_digits(&grid);
        let state = SearchState::<Bands<RowMajor>>::from_digits(&grid);
        let via_board = count_completions(board, 2);
        let via_state = count_completions(state, 2);
        let via_search =
            crate::probe::search::count_completions::<Bands<RowMajor>, crate::scan::Bivalue>(
                SearchState::<Bands<RowMajor>>::from_digits(&grid),
                2,
            );
        assert_eq!(via_board, 1);
        assert_eq!(via_state, 1);
        assert_eq!(via_search, 1);
    }
}
