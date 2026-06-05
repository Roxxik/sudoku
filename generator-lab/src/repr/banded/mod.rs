//! Banded board representations: the same placements and candidates as the scalar
//! grids in [`super`], transposed into 27-bit **bands** packed in SIMD vectors so
//! propagation works three bands at a time. Representation and invariants only —
//! the technique engine and the existence search that *use* these boards to solve
//! live a layer up.
//!
//! - [`RowBandedDigitGrid`]: placements (one banded cell-set per digit) in the
//!   row-major banding — the banded analogue of [`super::DigitGrid`], the reopen
//!   oracle a banded strip needs.
//! - [`DualBandedMarkGrid`]: candidates held in *both* bandings at once, kept
//!   consistent — the analogue of [`super::MarkGrid`] for the baseline technique
//!   engine, which needs every unit in-lane in at least one view. It is just a
//!   row-major and a column-major [`SearchState`](super::SearchState) held together.
//!
//! The digit-major candidate board is now [`SearchState<M>`](super::SearchState),
//! generic over the cell-set packing; the banded instance is `SearchState<Bands<B>>`.
//! The two primitives every banded grid is built from live in their own modules:
//! [`bands::Bands`], the SIMD cell-set, and the [`banding::Banding`] geometry it
//! is generic over (which cell maps to which band and bit).

mod band;
mod banding;
mod bands;
mod dual_banded_mark_grid;
mod row_banded_digit_grid;

pub use dual_banded_mark_grid::DualBandedMarkGrid;
pub use row_banded_digit_grid::RowBandedDigitGrid;

// The scalar per-band primitive and the banding geometry trait, for the fused
// per-band prober sweep (`Bands::band` -> `Band`, `Banding::cell_at`).
pub(crate) use band::Band;
pub use banding::Banding;

// The banded [`crate::repr::GridMask`] cell-set and its bandings, for the generic
// fill, prober, and `SearchState` to instantiate (e.g. `Fill<Bands<RowMajor>>`).
// `Bands<RowMajor>` is the search/branch packing (it is `Branchable`);
// `Bands<ColMajor>` is the column view (a `GridMask`, used only by the baseline's
// dual store, never branched).
pub use banding::{ColMajor, RowMajor};
pub use bands::Bands;
