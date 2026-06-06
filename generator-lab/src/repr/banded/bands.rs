//! `Bands` — a set of cells in SIMD-banded form: the workhorse primitive every
//! banded grid is built from. The cell-set analogue of the digit-set
//! [`crate::repr::Mark`]; the band/bit twiddling stays inside it so callers speak
//! in cells. Where cells map to bits is the [`Banding`] geometry it is generic
//! over (see [`super::banding`]).

use super::band::Band;
use super::banding::{Banding, RowMajor};
use crate::repr::{Branchable, CellIdx, GridMask};
use std::marker::PhantomData;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};
use std::simd::Simd;
use std::simd::cmp::SimdPartialEq;

/// A set of cells in a given [`Banding`] `B`: three 27-bit bands held in the low
/// three lanes of a SIMD vector (lane 3 unused). The cell-set analogue of the
/// digit-set [`crate::repr::Mark`] — a digit's candidate cells, the still-empty
/// mask, a peer set are all just sets of cells. The band/bit twiddling stays
/// inside these methods; callers speak in cells. `B` makes a row-banded set a
/// different type from a column-banded one, so the two views can't be confused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bands<B: Banding>(Simd<u32, 4>, PhantomData<B>);

impl<B: Banding> Bands<B> {
    /// The empty set.
    pub(super) const EMPTY: Bands<B> = Bands(Simd::from_array([0; 4]), PhantomData);

    /// Add cell `cell` to the set.
    pub(super) fn insert(&mut self, cell: CellIdx) {
        self.0 |= Simd::from_array(B::CELL_MASKS[cell]);
    }

    /// Remove cell `cell` from the set (no-op if absent).
    pub(super) fn remove(&mut self, cell: CellIdx) {
        self.0 &= !Simd::from_array(B::CELL_MASKS[cell]);
    }

    /// The three bands as raw 27-bit words in the low three lanes (lane 3 unused) —
    /// the SoA input the packed prober loads into one warp lane. Read-only escape
    /// hatch, paralleling [`band`](Bands::band): a [`SearchState`](crate::repr::SearchState)
    /// row view exports its per-digit candidate bands this way so the native warp can
    /// pack eight probes across SIMD lanes without re-deriving them cell by cell.
    #[inline]
    pub(crate) fn to_lanes(self) -> [u32; 4] {
        self.0.to_array()
    }

    /// Extract band `i` (0..3) as a scalar [`Band`] for a per-band unit sweep. The
    /// escape hatch the fused hidden-single prober needs: a band's 27 bits read off
    /// one lane drive a whole band's row/box scan, work the cell-at-a-time
    /// [`GridMask`] surface can't express. Read-only — placements still go through
    /// the set algebra, so the bands stay canonical.
    #[inline]
    pub(crate) fn band(self, i: usize) -> Band {
        Band(self.0[i])
    }
}

// Set algebra over the bands — banding-independent, so generic over `B`. These
// back the [`GridMask`] supertrait bounds, letting a generic scan read in plain
// `&`/`|`/`!`. The `!` flips the unused high bits (bits 27..32 of each lane, lane
// 3), but a complement is only ever AND-ed back against a valid mask, which clears
// the junk — so stored bands stay canonical.
impl<B: Banding> BitAnd for Bands<B> {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Bands(self.0 & rhs.0, PhantomData)
    }
}
impl<B: Banding> BitOr for Bands<B> {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Bands(self.0 | rhs.0, PhantomData)
    }
}
impl<B: Banding> BitAndAssign for Bands<B> {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}
impl<B: Banding> BitOrAssign for Bands<B> {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl<B: Banding> Not for Bands<B> {
    type Output = Self;
    #[inline]
    fn not(self) -> Self {
        Bands(!self.0, PhantomData)
    }
}

/// All 27 cells of one band, the low 27 bits of a lane.
const BAND_FULL: u32 = (1u32 << 27) - 1;

/// The banded [`GridMask`] — generic over the banding `B`, since the cell-set
/// operations (empty/full/singleton/peers/any) are all layout-independent: they
/// read `B`'s per-cell mask tables, and "all 81 cells set" is the same three full
/// bands whatever the banding. The win that motivates the banded packing: the three
/// bands ride in one SIMD register, so the scan's sieve touches all three per
/// instruction. The layout-*dependent* `first` lives in the [`Branchable`] impl,
/// only for [`RowMajor`].
impl<B: Banding> GridMask for Bands<B> {
    const EMPTY: Self = Bands(Simd::from_array([0; 4]), PhantomData);
    const FULL: Self = Bands(Simd::from_array([BAND_FULL, BAND_FULL, BAND_FULL, 0]), PhantomData);
    #[inline]
    fn cell(cell: CellIdx) -> Self {
        Bands(Simd::from_array(B::CELL_MASKS[cell]), PhantomData)
    }
    #[inline]
    fn peers(cell: CellIdx) -> Self {
        Bands(Simd::from_array(B::PEER_MASKS[cell]), PhantomData)
    }
    #[inline]
    fn any(self) -> bool {
        self.0.simd_ne(Simd::from_array([0; 4])).any()
    }
}

/// `first` is correct (and a cheap `trailing_zeros`) only for [`RowMajor`], which
/// packs `cell = 27*lane + bit` — contiguous in cell order, so the lowest set cell
/// is the lowest non-empty lane's lowest bit, the same cell the flat `u128` picks.
/// Column-major scrambles cell order vs bit order, and the search never branches on
/// it, so it is intentionally not `Branchable`.
impl Branchable for Bands<RowMajor> {
    #[inline]
    fn first(self) -> CellIdx {
        if self.0[0] != 0 {
            self.0[0].trailing_zeros() as usize
        } else if self.0[1] != 0 {
            27 + self.0[1].trailing_zeros() as usize
        } else {
            54 + self.0[2].trailing_zeros() as usize
        }
    }
}
