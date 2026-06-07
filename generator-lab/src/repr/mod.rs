//! `repr` — the sudoku representation layer: the foundational data types that
//! represent a puzzle's state, with the solvers, probers, and generators built on
//! top. It holds only the data and the operations that keep each representation's
//! invariant; anything that merely *uses* a representation to solve or generate
//! (technique engine, unit scans, arbitrary mark eliminations) lives a layer up,
//! not here.
//!
//! Value primitives:
//! - [`Digit`]: one of the nine digits, `1..=9` (an empty cell is `Option<Digit>`).
//! - [`Mark`]: a set of digits — one cell's pencil marks (candidates).
//!
//! Grids:
//! - [`DigitGrid`]: the placements — a [`Digit`] or empty per cell. A puzzle and
//!   a solution are both just this.
//! - [`Puzzle`] / [`Solution`]: a `DigitGrid` tagged with intent.
//! - [`MarkGrid`]: the candidates — one [`Mark`] per cell, derived from a
//!   `DigitGrid`.
//! - [`Board`]: a `DigitGrid` plus its `MarkGrid`, kept consistent at every
//!   mutation.
//!
//! Banded forms (in [`banded`]): the same placements and candidates transposed
//! into SIMD bands for the wide solver/prober paths.
//!
//! Structure: [`CELLS`] / [`CellIdx`] index cells; [`PEERS`] is the row/col/box
//! peer relation. The full unit-scan machinery is not here yet.

mod board;
mod digit;
mod digit_grid;
mod flat_grid_mask;
mod grid_mask;
mod mark;
mod mark_grid;
mod marks;
mod solver_state;
mod solve_view;
mod per_digit;
mod types;
mod unit;
pub mod banded;

pub use board::Board;
pub use digit::Digit;
pub use digit_grid::DigitGrid;
pub use flat_grid_mask::FlatGridMask;
pub use grid_mask::{Branchable, GridMask};
pub use mark::Mark;
pub use mark_grid::MarkGrid;
pub use marks::Marks;
pub use solver_state::SolverState;
pub use solve_view::{Occupancy, SolveView};
pub use per_digit::PerDigit;
pub use types::{Puzzle, Solution};
pub use unit::{PEER_MASK, PEERS, UNITS};

/// The number of cells in a sudoku grid.
pub const CELLS: usize = 81;

/// A cell index, `0..81` (row-major).
pub type CellIdx = usize;
