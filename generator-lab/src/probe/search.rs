//! The scan/sieve completion search — the fill's machinery read for
//! existence/uniqueness. Same [`SolverState`] + `scan -> branch -> recurse` shape as
//! [`Fill`](crate::fill), but seeded from a puzzle and counting completions instead
//! of returning the first. Generic over the [`GridMask`](crate::repr::GridMask)
//! packing **and** the [`BranchStrategy`] `S`, so `Mrv` vs `Bivalue` (the prober's
//! rule) plug in and bench — `count_completions::<M, Mrv>` vs `::<M, Bivalue>`.
//!
//! Branching copies the `SolverState` (it is `Copy` — no heap clone). Naked-single +
//! dead/solved verdicts come from the [`Sieve`](crate::scan::sieve) inside `scan`;
//! completeness comes from the branch, so this is correct for any `S` (a strategy
//! only changes which cell is branched, never the count). Hidden-single propagation
//! (bb's fused `band_update`) is the optional pruning that slots in later.

use super::propagate::Fixpoint;
use super::Propagate;
use crate::repr::{Digit, Marks, SolverState};
use crate::scan::{BranchStrategy, Scan};

/// The number of completions of `state`, counting only up to `cap`, branching by
/// strategy `S` (which carries its own sieve-depth cap — that only changes which
/// cell is branched, never the count).
pub fn count_completions<M, S>(state: SolverState<M>, cap: usize) -> usize
where
    M: Propagate,
    S: BranchStrategy,
{
    let mut found = 0;
    let mut state = state;
    count::<M, S>(&mut state, cap, &mut found);
    found
}

fn count<M, S>(state: &mut SolverState<M>, cap: usize, found: &mut usize)
where
    M: Propagate,
    S: BranchStrategy,
{
    super::pbump(2); // node tally (no-op without feature "count")
    loop {
        // Drain forced cells in place before branching — bb's rule, and the difference
        // between Bivalue working and degenerating (after propagation the board is
        // stuck, so a bivalue branch holds the factor at two). The verdict lets us skip
        // the branch scan on a terminal board (bb only runs `branch_cell` on a stuck
        // board): only `Stuck` falls through to it.
        match M::propagate(state) {
            Fixpoint::Solved => {
                *found += 1;
                return;
            }
            Fixpoint::Dead => return,
            Fixpoint::Stuck => {}
        }
        // A stuck board is neither solved nor dead, so the scan returns `Branch` (its
        // dead/solved checks re-confirm that); it computes the depth-`S` sieve only to
        // pick the branch cell.
        let Scan::Branch { cell, candidates } = S::scan(state.candidates(), state.unsolved())
        else {
            return;
        };
        let mut m = candidates;
        loop {
            let d = Digit::from_index(m.trailing_zeros() as usize);
            m &= m - 1;
            if m == 0 {
                // Last candidate: place it into *this* state and re-loop rather than
                // cloning — bb's `solve_first` spine. The clone is the per-branch cost
                // (a 160-byte memcpy that dominates `count`), so cloning only for the
                // earlier candidates removes it from the search's hot path.
                state.place(cell, d);
                break;
            }
            let mut child = state.clone();
            child.place(cell, d);
            count::<M, S>(&mut child, cap, found);
            if *found >= cap {
                return;
            }
        }
    }
}

/// The first completion of `state` the search reaches (DFS order), or `None` if the
/// board has none — the **witness** twin of [`count_completions`] at `cap = 1`: the same
/// `scan -> branch -> recurse` spine and node order, but it returns the solved
/// [`SolverState`] instead of only tallying. At a complete grid every cell is decided and
/// — because each cell's non-placed digits were cleared by their unit-mates — the
/// per-digit candidate boards are stale-free, so `candidates()[d]` is exactly the cells
/// holding `d`. The UA-witness diagnostic reads that out to recover the alternate
/// completion a non-unique strip admits (its cell-diff against the known solution is an
/// emptied unavoidable set). Counts nodes into `PCTR` identically to [`count`].
pub fn first_completion<M, S>(state: SolverState<M>) -> Option<SolverState<M>>
where
    M: Propagate,
    S: BranchStrategy,
{
    let mut state = state;
    first::<M, S>(&mut state)
}

fn first<M, S>(state: &mut SolverState<M>) -> Option<SolverState<M>>
where
    M: Propagate,
    S: BranchStrategy,
{
    super::pbump(2); // node tally, same as `count`
    loop {
        match M::propagate(state) {
            Fixpoint::Solved => return Some(state.clone()),
            Fixpoint::Dead => return None,
            Fixpoint::Stuck => {}
        }
        let Scan::Branch { cell, candidates } = S::scan(state.candidates(), state.unsolved())
        else {
            return None;
        };
        let mut m = candidates;
        loop {
            let d = Digit::from_index(m.trailing_zeros() as usize);
            m &= m - 1;
            if m == 0 {
                state.place(cell, d);
                break;
            }
            let mut child = state.clone();
            child.place(cell, d);
            if let Some(sol) = first::<M, S>(&mut child) {
                return Some(sol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::count_completions;
    use crate::repr::banded::{Bands, RowMajor};
    use crate::repr::{DigitGrid, FlatGridMask, Marks, SolverState};
    use crate::scan::{Bivalue, Mrv};

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
    const SOLUTION: &str = "\
        534678912\
        672195348\
        198342567\
        859761423\
        426853791\
        713924856\
        961537284\
        287419635\
        345286179";

    fn state<M: crate::repr::GridMask>(s: &str) -> SolverState<M> {
        SolverState::from_digits(&DigitGrid::parse(s).unwrap())
    }

    #[test]
    fn counts_known_puzzles() {
        // Bivalue's default depth (3) is its natural cap — it only reads exactly(2).
        assert_eq!(count_completions::<FlatGridMask, Bivalue>(state(PUZZLE), 2), 1);
        assert_eq!(count_completions::<FlatGridMask, Bivalue>(state(SOLUTION), 2), 1);
        assert_eq!(count_completions::<FlatGridMask, Bivalue>(state(&".".repeat(81)), 2), 2);
    }

    /// The count is a property of the puzzle, independent of packing, branch
    /// strategy, and sieve depth — flat/banded, Mrv/Bivalue, and depths 2/4/9 must
    /// all agree.
    #[test]
    fn packing_strategy_and_depth_agnostic() {
        for s in [PUZZLE, SOLUTION, &".".repeat(81)] {
            let baseline = count_completions::<FlatGridMask, Bivalue>(state(s), 2);
            assert_eq!(count_completions::<FlatGridMask, Mrv>(state(s), 2), baseline);
            assert_eq!(count_completions::<Bands<RowMajor>, Bivalue>(state(s), 2), baseline);
            assert_eq!(count_completions::<Bands<RowMajor>, Mrv<2>>(state(s), 2), baseline);
            assert_eq!(count_completions::<Bands<RowMajor>, Mrv<9>>(state(s), 2), baseline);
        }
    }

    /// [`first_completion`](super::first_completion) must (1) agree with the cap-1 count
    /// on existence and (2) return a completion whose *filled* cells reconstruct to a
    /// valid grid: each empty cell lands in exactly one digit board (the stale-free
    /// property the witness diff relies on — givens are in no board, which is why the diff
    /// is taken over the empty mask), and the full grid has no peer conflict.
    #[test]
    fn first_completion_reconstructs_a_valid_solution() {
        use crate::repr::{CELLS, GridMask, PEERS};
        type M = Bands<RowMajor>;
        for s in [PUZZLE, &".".repeat(81)] {
            let puz = DigitGrid::parse(s).unwrap();
            let exists = count_completions::<M, Bivalue>(SolverState::<M>::from_digits(&puz), 1) >= 1;
            let comp = super::first_completion::<M, Bivalue>(SolverState::<M>::from_digits(&puz));
            assert_eq!(comp.is_some(), exists, "verdict mismatch");
            let Some(alt) = comp else { continue };
            assert!(!alt.unsolved().any(), "completion not fully solved");
            let mut grid = [usize::MAX; CELLS];
            for c in 0..CELLS {
                if let Some(d) = puz.get(c) {
                    grid[c] = d.index(); // given: in no candidate board, read from the puzzle
                    continue;
                }
                for d in 0..9 {
                    if (alt.candidates().each()[d] & M::cell(c)).any() {
                        assert_eq!(grid[c], usize::MAX, "empty cell {c} in two digit boards (stale)");
                        grid[c] = d;
                    }
                }
                assert_ne!(grid[c], usize::MAX, "empty cell {c} in no digit board");
            }
            for c in 0..CELLS {
                for &p in &PEERS[c] {
                    assert_ne!(grid[c], grid[p], "peer conflict at {c}");
                }
            }
        }
    }
}
