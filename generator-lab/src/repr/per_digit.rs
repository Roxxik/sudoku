//! `PerDigit` — nine values, one per digit, indexed by the `1..=9` [`Digit`].
//!
//! The SoA shape shared across the representation: a value *per digit*, not a digit
//! per cell. The banded grids hold a [`Bands`](super::banded) per digit; the
//! generic fill holds a [`GridMask`](super::GridMask) per digit. This is where the
//! one-based digit -> 0-based slot conversion lives — once, behind `Index`/
//! `IndexMut` — so no access site spells `(d - 1)`.

use super::Digit;
use std::ops::{Index, IndexMut};

/// Nine `T`, one per digit, indexed by a [`Digit`] (slot `i` is digit `i + 1`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PerDigit<T>([T; 9]);

impl<T> PerDigit<T> {
    /// Wrap nine values in digit order (slot `0` = digit 1, …, slot `8` = digit 9).
    pub fn new(values: [T; 9]) -> Self {
        PerDigit(values)
    }

    /// The nine values as an array, in digit order — for the digit-agnostic passes
    /// (folds, scans) that touch every digit's value without caring *which* digit
    /// it is. Slot `i` is digit `i + 1`, so `.each().iter().enumerate()` yields the
    /// 0-based digit index alongside each value. (Fixed arity, unlike a dynamic
    /// `iter()`; digit-keyed access goes through `Index<Digit>` instead.)
    #[inline]
    pub fn each(&self) -> &[T; 9] {
        &self.0
    }
}

impl<T> Index<Digit> for PerDigit<T> {
    type Output = T;

    #[inline]
    fn index(&self, d: Digit) -> &T {
        &self.0[d.index()]
    }
}

impl<T> IndexMut<Digit> for PerDigit<T> {
    #[inline]
    fn index_mut(&mut self, d: Digit) -> &mut T {
        &mut self.0[d.index()]
    }
}
