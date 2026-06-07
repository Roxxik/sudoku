//! `FlatGridMask` — the flat `u128` packing of the [`GridMask`](super::GridMask)
//! cell-set: bit `c` set iff cell `c` is in the set. The identity layout (cell
//! index *is* bit position, no banding, no transpose) and the original fill
//! representation, the scalar counterpart of the banded [`Bands`](super::banded).

use super::{Branchable, CELLS, CellIdx, GridMask, PEERS};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// Flat `u128` packing: bit `c` set iff cell `c` is in the set (cells `0..81`; the
/// top 47 bits stay zero). The identity layout — cell index *is* bit position, no
/// banding, no transpose — and the original fill representation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FlatGridMask(u128);

/// All 81 cell bits set, the rest of the `u128` zero.
const ALL_CELLS: u128 = (1u128 << CELLS) - 1;

/// `PEER_MASK[c]` has a bit set for each of cell `c`'s 20 peers (self excluded), as
/// an 81-bit value in a `u128` — so a placement forbids a digit on every peer in
/// one AND-NOT instead of a 20-cell walk.
const PEER_MASK: [u128; CELLS] = {
    let mut m = [0u128; CELLS];
    let mut i = 0;
    while i < CELLS {
        let mut k = 0;
        while k < 20 {
            m[i] |= 1u128 << PEERS[i][k];
            k += 1;
        }
        i += 1;
    }
    m
};

impl GridMask for FlatGridMask {
    const EMPTY: Self = FlatGridMask(0);
    const FULL: Self = FlatGridMask(ALL_CELLS);
    #[inline]
    fn cell(cell: CellIdx) -> Self {
        FlatGridMask(1u128 << cell)
    }
    #[inline]
    fn peers(cell: CellIdx) -> Self {
        FlatGridMask(PEER_MASK[cell])
    }
    #[inline]
    fn any(self) -> bool {
        self.0 != 0
    }
}

impl Branchable for FlatGridMask {
    #[inline]
    fn first(self) -> CellIdx {
        self.0.trailing_zeros() as usize
    }
    #[inline]
    fn contains(self, cell: CellIdx) -> bool {
        // Identity layout: cell index is bit position.
        self.0 & (1u128 << cell) != 0
    }
}

impl BitAnd for FlatGridMask {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        FlatGridMask(self.0 & rhs.0)
    }
}
impl BitOr for FlatGridMask {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        FlatGridMask(self.0 | rhs.0)
    }
}
impl BitAndAssign for FlatGridMask {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}
impl BitOrAssign for FlatGridMask {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl Not for FlatGridMask {
    type Output = Self;
    #[inline]
    fn not(self) -> Self {
        FlatGridMask(!self.0)
    }
}
