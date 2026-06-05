//! Banding geometry: which (band, slot) each cell maps to, and the per-cell
//! singleton masks that mapping folds into. Pure data and arithmetic — no
//! candidates, no solving, no storage (the cell-set container that *uses* this
//! geometry is [`super::bands::Bands`]).
//!
//! Sudoku splits the grid into three horizontal **bands** — the top three rows,
//! the middle three, the bottom three — each 27 cells (three boxes side by side).
//! A banding keeps a set of cells as one 27-bit word per band. In the row-major
//! banding the 27 bits of a band run row by row (its three rows occupy bits
//! 0..9, 9..18, 18..27), so a row is a contiguous 9-bit run and a box is three
//! bits in each run — the property the layout exists to give.
//!
//! Which cell goes to which (band, slot) is the *banding*: [`RowMajor`] (bands are
//! horizontal three-row strips) and [`ColMajor`] (bands are vertical three-column
//! **stacks**). The two are distinct types via the [`Banding`] marker, so the
//! dual-view grid cannot accidentally mix a row-band with a column-band — the
//! compiler rejects it.

use crate::repr::{CELLS, CellIdx, PEERS};

/// Which band a cell lives in — the top, middle, or bottom three-row strip. This
/// is also the [`Bands`](super::bands::Bands) lane that stores the band, so it
/// names a band and its lane at once.
pub(crate) type BandIdx = usize;

/// A cell's slot within its band's 27-bit word: its band-row picks one of the
/// three 9-bit runs, its column the place inside that run.
pub(crate) type BandPos = usize;

/// A banding: the scheme assigning each cell to its band and its slot within that
/// band. The implementors are the two transposed views — [`RowMajor`] (bands are
/// the horizontal three-row strips) and [`ColMajor`] (bands are the vertical
/// three-column stacks). It is a `const` trait so [`build_cell_masks`] can fold
/// the geometry into lookup tables at compile time. The `Copy + Eq` supertraits
/// (both bandings are `Copy` ZSTs) let the generic [`Bands<B>`](super::bands::Bands)
/// derive `Copy`/`Eq` without re-stating the bound at every use.
pub const trait Banding: Copy + Eq {
    /// The band cell `cell` belongs to.
    fn band_idx(cell: CellIdx) -> BandIdx;

    /// Cell `cell`'s slot within its band's 27-bit word.
    fn band_pos(cell: CellIdx) -> BandPos;

    /// The cell at band-local position `pos` in band `band` — the inverse of
    /// (`band_idx`, `band_pos`). A per-band sweep finds a hidden single as a (band,
    /// position) pair and maps it back to a cell to place; this is that map, layout-
    /// aware so the sweep stays in cells.
    fn cell_at(band: BandIdx, pos: BandPos) -> CellIdx;

    /// One singleton mask per cell for this banding (cell `c`'s lone bit in its
    /// band), folded from `band_idx`/`band_pos` at compile time. Reading it is how
    /// [`Bands`](super::bands::Bands) sets and clears single cells without
    /// re-deriving the geometry.
    const CELL_MASKS: [[u32; 4]; CELLS];

    /// One peer mask per cell for this banding: cell `c`'s 20 peers (self
    /// excluded) as set bits, the shared [`PEERS`] relation rebanded into this
    /// view. Reading it is how [`Bands`](super::bands::Bands) forbids a digit
    /// across a cell's whole unit in one masked op instead of a 20-cell walk.
    const PEER_MASKS: [[u32; 4]; CELLS];
}

/// Row-major banding: bands are the three horizontal three-row strips; within a
/// band the 27 bits run row by row, so a row and a box each lie inside one lane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RowMajor;

impl const Banding for RowMajor {
    fn band_idx(cell: CellIdx) -> BandIdx {
        (cell / 9) / 3
    }

    fn band_pos(cell: CellIdx) -> BandPos {
        ((cell / 9) % 3) * 9 + cell % 9
    }

    fn cell_at(band: BandIdx, pos: BandPos) -> CellIdx {
        // pos / 9 is the band-row, pos % 9 the column: global row 3*band + band-row.
        (3 * band + pos / 9) * 9 + pos % 9
    }

    const CELL_MASKS: [[u32; 4]; CELLS] = build_cell_masks::<RowMajor>();
    const PEER_MASKS: [[u32; 4]; CELLS] = build_peer_masks::<RowMajor>();
}

/// Column-major banding: bands are the three vertical three-column stacks; within
/// a band the 27 bits run column by column (its three columns occupy bits 0..9,
/// 9..18, 18..27), so a column and a box each lie inside one lane. The transpose
/// of [`RowMajor`] — together they put every unit in-lane in at least one view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ColMajor;

impl const Banding for ColMajor {
    fn band_idx(cell: CellIdx) -> BandIdx {
        (cell % 9) / 3
    }

    fn band_pos(cell: CellIdx) -> BandPos {
        ((cell % 9) % 3) * 9 + cell / 9
    }

    fn cell_at(band: BandIdx, pos: BandPos) -> CellIdx {
        // pos / 9 is the column-within-stack, pos % 9 the row: global col 3*band + that.
        (pos % 9) * 9 + (3 * band + pos / 9)
    }

    const CELL_MASKS: [[u32; 4]; CELLS] = build_cell_masks::<ColMajor>();
    const PEER_MASKS: [[u32; 4]; CELLS] = build_peer_masks::<ColMajor>();
}

/// Fold a banding's `band_idx`/`band_pos` into the per-cell singleton masks at
/// compile time: cell `c` becomes its one bit in its one band.
const fn build_cell_masks<B: [const] Banding>() -> [[u32; 4]; CELLS] {
    let mut m = [[0u32; 4]; CELLS];
    let mut cell = 0;
    while cell < CELLS {
        m[cell][B::band_idx(cell)] = 1u32 << B::band_pos(cell);
        cell += 1;
    }
    m
}

/// Fold the shared [`PEERS`] relation through a banding into per-cell peer masks
/// at compile time: cell `c` becomes the union of its 20 peers' bits in this
/// view's bands.
const fn build_peer_masks<B: [const] Banding>() -> [[u32; 4]; CELLS] {
    let mut m = [[0u32; 4]; CELLS];
    let mut cell = 0;
    while cell < CELLS {
        let mut k = 0;
        while k < 20 {
            let peer = PEERS[cell][k];
            m[cell][B::band_idx(peer)] |= 1u32 << B::band_pos(peer);
            k += 1;
        }
        cell += 1;
    }
    m
}
