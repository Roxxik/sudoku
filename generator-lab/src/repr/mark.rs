//! `Mark` — a set of digits: one cell's pencil marks (its candidates). The
//! element type of a [`super::MarkGrid`], paralleling how [`super::Digit`] is the
//! element of a [`super::DigitGrid`].

use super::Digit;
use std::fmt;

/// A set of digits, stored as a 9-bit mask: bit `d-1` set means digit `d` is in
/// the set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mark(u16);

impl fmt::Debug for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries((1..=9u8).filter(|&d| self.0 & (1u16 << (d - 1)) != 0))
            .finish()
    }
}

impl Mark {
    /// The empty set.
    pub const EMPTY: Mark = Mark(0);
    /// All nine digits.
    pub const ALL: Mark = Mark(0x1FF);

    /// The singleton set `{d}`.
    #[inline]
    pub fn single(d: Digit) -> Mark {
        Mark(1u16 << d.index())
    }

    /// Whether digit `d` is in the set.
    #[inline]
    pub fn contains(self, d: Digit) -> bool {
        self.0 & (1u16 << d.index()) != 0
    }

    /// The number of digits in the set (a cell's candidate count: 1 is a naked
    /// single, 0 a filled or contradicted cell).
    #[inline]
    pub fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Whether the set is empty.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate the digits in the set in ascending order.
    #[inline]
    pub fn iter(self) -> impl Iterator<Item = Digit> {
        let bits = self.0;
        (0..9u8)
            .filter(move |i| bits & (1u16 << i) != 0)
            .map(|i| Digit::from_index(i as usize))
    }

    /// Add digit `d` to the set (no-op if present).
    #[inline]
    pub fn insert(&mut self, d: Digit) {
        self.0 |= 1u16 << d.index();
    }

    /// Remove digit `d` from the set (no-op if absent).
    #[inline]
    pub fn remove(&mut self, d: Digit) {
        self.0 &= !(1u16 << d.index());
    }

    /// The digits in `self` but not in `other` (set difference) — the subset
    /// techniques' "remove the kept digits, eliminate the rest" step.
    #[inline]
    pub fn without(self, other: Mark) -> Mark {
        Mark(self.0 & !other.0)
    }
}

/// Union of two digit sets — the subset techniques build a combo's candidate union
/// this way (`a | b`), the digit-set analogue of cell-set [`super::GridMask`] `|`.
impl std::ops::BitOr for Mark {
    type Output = Mark;
    #[inline]
    fn bitor(self, rhs: Mark) -> Mark {
        Mark(self.0 | rhs.0)
    }
}

/// Intersection of two digit sets — a naked subset eliminates exactly `cell & union`.
impl std::ops::BitAnd for Mark {
    type Output = Mark;
    #[inline]
    fn bitand(self, rhs: Mark) -> Mark {
        Mark(self.0 & rhs.0)
    }
}
