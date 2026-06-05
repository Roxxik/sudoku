//! `Band` — one band's 27 candidate bits for a single digit: the scalar lane the
//! SIMD [`Bands`](super::bands::Bands) is built from, pulled out one at a time for
//! the per-band *unit* sweeps (hidden singles) that drive a whole band off one word.
//!
//! [`Bands`](super::bands::Bands) is the cell-*set* primitive — set algebra across
//! three bands in one SIMD register, the hot clone/place path. `Band` is its scalar
//! counterpart for the work the set surface can't express: reading a band's three
//! in-lane units of each kind (three lines, three boxes) as 9-bit masks, and mapping
//! a slot found inside one of those units back to a bit in the 27-bit word. That
//! band-local twiddling lives here so the prober's sweep reads in units, not shifts.
//!
//! The line/box *bit layout* is the same for both bandings — within a band the 27
//! bits run line by line (a line is a contiguous 9-bit run; a box is three bits in
//! each of the three runs), the property the banding exists to give — so the
//! decomposition is banding-independent. Only mapping a band position back to a cell
//! differs per banding, and that lives on the [`Banding`](super::banding::Banding).

/// One band's 27-bit candidate word for a single digit (lane bits 0..27; the top
/// five are always zero). A line is a contiguous 9-bit run; a box gathers the
/// `k`-th 3-cell triplet of each line.
#[derive(Clone, Copy)]
pub(crate) struct Band(pub(super) u32);

impl Band {
    /// The band's `line`-th line as a 9-bit candidate mask — one of the three
    /// contiguous 9-bit runs. (Rows in a row-major band, columns in a column-major
    /// one; the bit math is the same.)
    #[inline]
    pub(crate) fn line(self, line: usize) -> usize {
        ((self.0 >> (9 * line)) & 0x1FF) as usize
    }

    /// The band's `k`-th box as a 9-bit candidate mask, each line's `k`-th 3-cell
    /// triplet gathered into a contiguous run.
    #[inline]
    pub(crate) fn box_unit(self, k: usize) -> usize {
        (((self.0 >> (3 * k)) & 7)
            | (((self.0 >> (9 + 3 * k)) & 7) << 3)
            | (((self.0 >> (18 + 3 * k)) & 7) << 6)) as usize
    }

    /// The band-local bit position of slot `slot` (0..9) within line `line` — the
    /// inverse of [`line`](Band::line)'s extraction, to turn a hidden single found
    /// in a line back into a band position.
    #[inline]
    pub(crate) fn line_pos(line: usize, slot: usize) -> usize {
        9 * line + slot
    }

    /// The band-local bit position of slot `slot` (0..9) within box `k` — the
    /// inverse of [`box_unit`](Band::box_unit)'s gather.
    #[inline]
    pub(crate) fn box_pos(k: usize, slot: usize) -> usize {
        (slot / 3) * 9 + 3 * k + slot % 3
    }
}
