//! The shared technique **taxonomy** for spec-driven generation, PoC scope = the
//! ladder up to HiddenQuad: the kind enumeration (as difficulty-ordered indices),
//! their metadata ([`DIFFICULTY`]/[`NAMES`]), the [`KindMask`] set type over them,
//! and the [`SolveTrace`] a baseline solve produces.
//!
//! This is the vocabulary the rest of the crate speaks — [`crate::spec`],
//! [`crate::bb`], and [`crate::generator`] all reference it without needing the
//! scalar reference engine in [`crate::techniques`]. `SolveTrace` is the shared
//! contract returned by *both* [`crate::techniques::solve_tracked`] (scalar) and
//! [`crate::bb::BitBoard::baseline`] (bitboard), so it lives here, not with either
//! engine.
//!
//! Kinds are indexed in difficulty order; the index IS the kind and the bit
//! `1 << index` is its membership bit in a [`KindMask`].

/// Number of technique kinds in PoC scope (NakedSingle .. HiddenQuad).
pub const NUM: usize = 10;

// Kind indices, difficulty order. The index is the kind; `1 << index` is its
// membership bit in a `KindMask`.
pub const NAKED_SINGLE: usize = 0;
pub const HIDDEN_SINGLE: usize = 1;
pub const LC_POINTING: usize = 2;
pub const LC_CLAIMING: usize = 3;
pub const NAKED_PAIR: usize = 4;
pub const HIDDEN_PAIR: usize = 5;
pub const NAKED_TRIPLE: usize = 6;
pub const HIDDEN_TRIPLE: usize = 7;
pub const NAKED_QUAD: usize = 8;
pub const HIDDEN_QUAD: usize = 9;

/// Difficulty of each kind, matching core's REGISTRY.
pub const DIFFICULTY: [u32; NUM] = [10, 15, 20, 25, 30, 33, 36, 39, 42, 45];

/// Human-readable names, for bench/diagnostic output.
pub const NAMES: [&str; NUM] = [
    "naked-single",
    "hidden-single",
    "lc-pointing",
    "lc-claiming",
    "naked-pair",
    "hidden-pair",
    "naked-triple",
    "hidden-triple",
    "naked-quad",
    "hidden-quad",
];

/// A set of technique *kinds* as a bitmask (`1 << kind_index`). Distinct from
/// [`crate::grid::DigitMask`], the 9-bit set of digits.
pub type KindMask = u32;

/// The result of a tracked baseline solve: whether the `allowed` toolbox solved
/// the board, and how many times each kind fired (for the requirement check).
/// Returned by both the scalar [`crate::techniques::solve_tracked`] and the
/// bitboard [`crate::bb::BitBoard::baseline`].
pub struct SolveTrace {
    pub solved: bool,
    pub counts: [u16; NUM],
}
