//! `GridMask` — a set of the grid's 81 cells (yes/no per cell), abstract over how
//! those 81 bits are physically packed. The search state of the fill and the
//! existence prober is built entirely from these: a digit's still-legal cells, the
//! still-unsolved cells, the candidate-count tiers a scan derives — all are sets of
//! cells. The trait is the swap point between packings (the flat
//! [`FlatGridMask`](super::FlatGridMask) and the banded [`Bands`](super::banded));
//! the band/bit twiddling stays inside each impl, so the scan that uses a `GridMask`
//! reads in plain set algebra.
//!
//! The set operations are the bitwise operators (`&`/`|`/`!`) as supertrait bounds,
//! so a generic scan body is spelled exactly like the hand-written `u128` one;
//! [`GridMask::cell`]/[`peers`](GridMask::peers)/[`first`](Branchable::first) supply
//! the cell-indexed pieces a raw integer can't.

use super::CellIdx;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// A set of the grid's 81 cells. Bitwise ops are set algebra (intersection, union,
/// complement); the cell-indexed methods bridge to concrete cells.
///
/// Everything here is layout-independent — it holds for any packing of the 81 cells,
/// including both bandings. The one operation that is *not* layout-independent,
/// `first` (the lowest-indexed set cell), lives in the [`Branchable`] extension:
/// it is cheap only when cell index and bit position rise together (row-major and
/// flat), and is needed only by the search's branch step — the baseline's
/// column-major view never branches, so it is a `GridMask` but not a `Branchable`.
pub trait GridMask:
    Copy
    + PartialEq
    + BitAnd<Output = Self>
    + BitOr<Output = Self>
    + BitAndAssign
    + BitOrAssign
    + Not<Output = Self>
{
    /// The empty set.
    const EMPTY: Self;
    /// All 81 cells.
    const FULL: Self;
    /// The singleton set `{cell}`.
    fn cell(cell: CellIdx) -> Self;
    /// Cell `cell`'s 20 peers (the cells sharing its row, column, or box; self
    /// excluded) — the cells a placement at `cell` forbids its digit on.
    fn peers(cell: CellIdx) -> Self;
    /// Whether any cell is set.
    fn any(self) -> bool;
}

/// A [`GridMask`] whose cells can be ordered for branching: it can yield its
/// lowest-indexed set cell. Implemented only where that is cheap and meaningful —
/// the flat `u128` and the row-major banding, where cell index and bit position
/// rise together. The search (fill, prober) branches on the first cell, so it
/// requires `Branchable`; the candidate *store* and the baseline's unit scans only
/// require [`GridMask`], so the column-major view (where the lowest set bit is not
/// the lowest cell) need not — and does not — implement this.
pub trait Branchable: GridMask {
    /// The lowest-indexed set cell. Caller guarantees the set is non-empty.
    fn first(self) -> CellIdx;
}
