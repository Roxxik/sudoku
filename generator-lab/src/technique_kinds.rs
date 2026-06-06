//! The shared technique **taxonomy** for spec-driven generation, PoC scope = the
//! ladder up to HiddenQuad: the kind enumeration (as difficulty-ordered indices),
//! their metadata ([`DIFFICULTY`]/[`NAMES`]), the [`KindMask`] set type over them,
//! and the [`SolveTrace`] a logic solve produces.
//!
//! This is the vocabulary the rest of the crate speaks — [`crate::spec`],
//! [`crate::solve`], [`crate::probe`], and [`crate::generate`] all reference it.
//! `SolveTrace` is the contract the [`solve`](crate::solve) engines return, so it
//! lives here, not with any one engine.
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

/// A set of technique *kinds* as a bitmask (`1 << kind_index`). Distinct from a
/// [`Mark`](crate::repr::Mark), the 9-bit set of digits.
pub type KindMask = u32;

/// The result of a tracked logic solve: whether the `allowed` toolbox solved the
/// board, and how many times each kind fired (for the requirement check). Returned
/// by the [`solve`](crate::solve) engines ([`LogicSolver`](crate::solve::LogicSolver)
/// and [`FusedLogicSolver`](crate::solve::FusedLogicSolver)).
pub struct SolveTrace {
    pub solved: bool,
    pub counts: [u16; NUM],
}
