//! `Bands` — a set of cells in SIMD-banded form: the workhorse primitive every
//! banded grid is built from. The cell-set analogue of the digit-set
//! [`crate::repr::Mark`]; the band/bit twiddling stays inside it so callers speak
//! in cells. Where cells map to bits is the [`Banding`] geometry it is generic
//! over (see [`super::banding`]).

use super::band::Band;
use super::banding::{Banding, RowMajor};
use crate::repr::{Branchable, CELLS, CellIdx, GridMask};
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
    /// The three bands as raw 27-bit words in the low three lanes (lane 3 unused) —
    /// the SoA input the packed prober loads into one warp lane. Read-only escape
    /// hatch, paralleling [`band`](Bands::band): a [`SolverState`](crate::repr::SolverState)
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

    /// Build a set from a raw 27-bit band word broadcast into all three bands (lane 3
    /// unused). The column-broadcast the single-view column sweep needs: a 9-bit
    /// per-column mask smeared across a band's three rows (`m | m<<9 | m<<18`) is the
    /// same pattern in every band, so one word fills all three. Only the low 27 bits of
    /// `w` are meaningful; the result is AND-ed against a canonical board, which clears
    /// any junk above bit 27.
    #[inline]
    pub(crate) fn from_band_splat(w: u32) -> Self {
        Bands(Simd::from_array([w, w, w, 0]), PhantomData)
    }

    /// Extract band `I` (a compile-time constant) as a scalar [`Band`]. Same value as
    /// [`band`](Bands::band), but the `Index` form `self.0[i]` reads through
    /// [`as_array`](std::simd::Simd::as_array), which forces the vector to a stack slot —
    /// a store + indexed reload per access. Taking the lane off a by-value `to_array`
    /// with a constant index lets the optimizer keep it a register `vpextrd`, which the
    /// memory-port-bound hidden-single sweep wants when it unrolls the three bands.
    #[inline]
    pub(crate) fn band_at<const I: usize>(self) -> Band {
        Band(self.0.to_array()[I])
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
        // A CellIdx is 0..81 by construction (a shuffle of 0..81, a tzcnt over an
        // 81-bit mask, or a const peer table), but the bare usize can't carry that, so
        // the per-cell table index otherwise emits a bounds check it can never trigger.
        debug_assert!(cell < CELLS, "cell index {cell} out of range");
        // SAFETY: guaranteed by the 0..81 construction invariant asserted above.
        unsafe { core::hint::assert_unchecked(cell < CELLS) };
        Bands(Simd::from_array(B::CELL_MASKS[cell]), PhantomData)
    }
    #[inline]
    fn peers(cell: CellIdx) -> Self {
        debug_assert!(cell < CELLS, "cell index {cell} out of range");
        // SAFETY: see `cell` — a CellIdx is 0..81 by construction.
        unsafe { core::hint::assert_unchecked(cell < CELLS) };
        Bands(Simd::from_array(B::PEER_MASKS[cell]), PhantomData)
    }
    #[inline]
    fn any(self) -> bool {
        self.0.simd_ne(Simd::from_array([0; 4])).any()
    }
    #[inline]
    fn len(self) -> u32 {
        // A canonical set keeps only the low 27 bits of the three used lanes (lane 3 is
        // unused), so summing their popcounts is the cell population.
        let l = self.0.to_array();
        l[0].count_ones() + l[1].count_ones() + l[2].count_ones()
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
    #[inline]
    fn contains(self, cell: CellIdx) -> bool {
        // RowMajor packs `cell = 27*band + bit`, so `cell` lives in exactly one band
        // word — read that lane and test its bit (a scalar load of 4 bytes), not the
        // whole 16-byte register through a `simd_ne`/reduce.
        self.0[cell / 27] & (1u32 << (cell % 27)) != 0
    }
}
