//! The [`Digit`] type — the fundamental value primitive of the representation
//! layer, the building block of both the placements in a [`super::DigitGrid`] and
//! the candidates in a [`super::Mark`] (a set of `Digit`s).
use std::num::NonZeroU8;

/// One of the nine sudoku digits, `1..=9`. A newtype over [`NonZeroU8`] so "not
/// empty" lives in the type — a `Digit` is always a real digit, never a stand-in
/// for an empty cell. An empty cell is `Option<Digit>` (`None`); thanks to the
/// null niche, `Option<Digit>` is one byte with `None` == 0, identical in memory
/// to the old `0`-means-empty `u8` encoding.
///
/// `#[repr(transparent)]` makes that memory identity a *guarantee*, not just a
/// likelihood: `Digit` has exactly `NonZeroU8`'s layout, so `Option<Digit>` is
/// `Option<NonZeroU8>` — one byte, `None` == 0, `Some(d)` == `d.get()`. A grid of
/// `Option<Digit>` is therefore byte-for-byte a `[u8; N]` of `0`/`1..=9`, which is
/// what [`super::DigitGrid::as_bytes`] reinterprets with no copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Digit(NonZeroU8);

impl Digit {
    /// The digit `d` if `d` is in `1..=9`, else `None` — the checked constructor
    /// for untrusted input (parsing).
    pub fn new(d: u8) -> Option<Digit> {
        match d {
            1..=9 => NonZeroU8::new(d).map(Digit),
            _ => None,
        }
    }

    /// The digit at 0-based `index` (`digit = index + 1`) — the inverse of
    /// [`Digit::index`], for turning a bit position or array slot back into a
    /// digit. Caller guarantees `index < 9`.
    #[inline]
    pub fn from_index(index: usize) -> Digit {
        debug_assert!(index < 9, "digit index {index} out of range");
        // SAFETY: index + 1 is in 1..=9 by the caller's contract, so nonzero.
        Digit(unsafe { NonZeroU8::new_unchecked(index as u8 + 1) })
    }

    /// The `1..=9` value.
    #[inline]
    pub fn get(self) -> u8 {
        self.0.get()
    }

    /// The 0-based index `0..9` (`value - 1`), for array slots and bit shifts.
    /// This owns the `digit - 1` conversion so callers never spell it inline.
    #[inline]
    pub fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}