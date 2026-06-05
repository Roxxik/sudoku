//! `Occupancy` and `SolveView` — the capability traits a search/solver reads a
//! board through, layered on the candidate store [`Marks`](super::Marks).
//!
//! [`Occupancy`] adds the one fact candidates alone can't carry: which cells are
//! still empty. An empty mark is ambiguous — a *filled* cell and a *contradicted*
//! one both have no candidates — so `Marks::get` can't answer it; [`Board`] derives
//! it from its placements, the banded grids from their explicit `unsolved` mask.
//! (This is why bare [`MarkGrid`](super::MarkGrid), candidates only, is *not* an
//! `Occupancy` — the line we want.)
//!
//! [`SolveView`] is the bundle a solve runs through: candidates + occupancy +
//! [`Clone`]. It is a capability *view* over a representation, not a representation
//! itself — hence `View`, not `Board`.

use super::{CellIdx, Marks};

/// Knows which cells are still empty — the fact [`Marks`] can't supply on its own.
pub trait Occupancy {
    /// Whether `cell` holds no digit yet.
    fn is_empty(&self, cell: CellIdx) -> bool;

    /// Whether `cell` holds a digit — the complement of
    /// [`is_empty`](Occupancy::is_empty).
    #[inline]
    fn is_occupied(&self, cell: CellIdx) -> bool {
        !self.is_empty(cell)
    }
}

/// The board a search/solver operates through: candidates ([`Marks`]) + occupancy
/// ([`Occupancy`]) + [`Clone`]. A marker bundle with a blanket impl, so any type
/// with the three *is* a `SolveView` automatically and a probe/solve signature
/// reads `<B: SolveView>` rather than the full triple.
///
/// `Clone` is in the bundle because every solve works on a copy: the prober forks
/// it per branch, and even a non-branching baseline solves on a clone so the
/// caller's (strip) board survives the gate. There is no consumer that wants
/// candidates + occupancy *without* `Clone`, and every impl is `Clone`, so the
/// split into a separate "branching" trait would buy nothing — fold it in here.
pub trait SolveView: Marks + Occupancy + Clone {}

impl<T: Marks + Occupancy + Clone> SolveView for T {}
