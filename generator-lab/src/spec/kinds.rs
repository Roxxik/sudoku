//! The shared technique **taxonomy** for spec-driven generation: the kind
//! enumeration, their metadata ([`DIFFICULTY`]/[`NAMES`]/[`BRANCH`]), the
//! [`KindMask`] set type over them, and the [`SolveTrace`] a logic solve produces.
//! Scope = the Trunk (singles + locked candidates), the Subset branch
//! (naked/hidden pair..quad), the Single-digit / Fish branch (X-Wing,
//! Swordfish, Jellyfish), and the Bivalue-chain branch (XY-Wing, XYZ-Wing,
//! W-Wing).
//!
//! This is the vocabulary the rest of the crate speaks — [`crate::spec`],
//! [`crate::solve`], [`crate::probe`], and [`crate::generate`] all reference it.
//! `SolveTrace` is the contract the [`solve`](crate::solve) engines return, so it
//! lives here, not with any one engine.
//!
//! **The index is identity, not a solving order.** A kind's index addresses its
//! per-kind array slot and its `1 << index` bit in a [`KindMask`]; nothing derives
//! a try-order from it. The player-facing [`DIFFICULTY`] (the curriculum axis) and
//! the [`BRANCH`] strand are what `train`/`drill` scope on; each solve engine is
//! free to try techniques in whatever order is cheapest *for it* (see
//! `CURRICULUM.md`). Indices below follow core's ordering only for familiarity.

/// Number of technique kinds in scope (Trunk + Subset + Fish + Bivalue branches).
pub const NUM: usize = 16;

// Kind indices — identity only (array slot + `1 << index` membership bit), NOT a
// solving order. Numbered after core's kind ordering for familiarity.
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
pub const X_WING: usize = 10;
pub const SWORDFISH: usize = 11;
pub const JELLYFISH: usize = 12;
pub const XY_WING: usize = 13;
pub const XYZ_WING: usize = 14;
pub const W_WING: usize = 15;

/// Player-facing **difficulty** score per kind — the curriculum axis (see
/// `CURRICULUM.md`), matching core's `TechniqueDef::difficulty`. INTERLEAVES the
/// branches (X-Wing 38 sits between Naked Pair 32 and Hidden Pair 44), so it is
/// intentionally non-monotonic in index order. `train`/`drill` scope on this plus
/// [`BRANCH`]; it is NOT a solver try-order.
pub const DIFFICULTY: [u32; NUM] = [
    15, // naked-single
    5,  // hidden-single
    22, // lc-pointing
    26, // lc-claiming
    32, // naked-pair
    44, // hidden-pair
    50, // naked-triple
    62, // hidden-triple
    82, // naked-quad
    92, // hidden-quad
    38, // x-wing
    56, // swordfish
    86, // jellyfish
    68, // xy-wing
    72, // xyz-wing
    76, // w-wing
];

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
    "x-wing",
    "swordfish",
    "jellyfish",
    "xy-wing",
    "xyz-wing",
    "w-wing",
];

/// Coarse pedagogical strand a kind trains down — the axis `train`/`drill` scope
/// Expert targets on. Mirrors core's `Branch` (the subset of it this lab models):
/// the Beginner/Intermediate [`Trunk`](Branch::Trunk) every branch grows from, the
/// [`Subset`](Branch::Subset) branch, the single-digit [`Fish`](Branch::Fish)
/// branch, and the [`Bivalue`](Branch::Bivalue) chains branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// Singles + locked candidates: the Beginner/Intermediate trunk.
    Trunk,
    /// Single-digit logic: basic fish (X-Wing, Swordfish, Jellyfish).
    Fish,
    /// Naked/hidden subsets of size 2-4.
    Subset,
    /// Bivalue chains: the wing family (XY-Wing, XYZ-Wing, W-Wing).
    Bivalue,
}

/// The [`Branch`] each kind belongs to, indexed by kind.
pub const BRANCH: [Branch; NUM] = [
    Branch::Trunk,  // naked-single
    Branch::Trunk,  // hidden-single
    Branch::Trunk,  // lc-pointing
    Branch::Trunk,  // lc-claiming
    Branch::Subset, // naked-pair
    Branch::Subset, // hidden-pair
    Branch::Subset, // naked-triple
    Branch::Subset, // hidden-triple
    Branch::Subset, // naked-quad
    Branch::Subset, // hidden-quad
    Branch::Fish,    // x-wing
    Branch::Fish,    // swordfish
    Branch::Fish,    // jellyfish
    Branch::Bivalue, // xy-wing
    Branch::Bivalue, // xyz-wing
    Branch::Bivalue, // w-wing
];

/// The pedagogical branch of kind `idx`.
#[inline]
pub fn branch_of(idx: usize) -> Branch {
    BRANCH[idx]
}

/// Player-facing difficulty tier, matching `CURRICULUM.md` (and core's `Tier`).
/// `Ord` runs Beginner < Intermediate < Expert < Master, so "easier tier" is a
/// plain comparison. The cut-points follow SudokuExplainer's thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Hidden singles only.
    Beginner,
    /// Naked single + locked candidates.
    Intermediate,
    /// The trainable branches: fish, subsets, bivalue chains.
    Expert,
    /// Advanced set logic (unused in this lab, kept for the cut's upper bound).
    Master,
}

impl Tier {
    const INTERMEDIATE_FLOOR: u32 = 15; // naked single
    const EXPERT_FLOOR: u32 = 30; // naked pair (SE 3.0)
    const MASTER_FLOOR: u32 = 100; // SE ~5.4

    /// The tier a player-facing `difficulty` score falls into.
    pub fn of_difficulty(d: u32) -> Tier {
        if d < Self::INTERMEDIATE_FLOOR {
            Tier::Beginner
        } else if d < Self::EXPERT_FLOOR {
            Tier::Intermediate
        } else if d < Self::MASTER_FLOOR {
            Tier::Expert
        } else {
            Tier::Master
        }
    }
}

/// The [`Tier`] of kind `idx`, by its player-facing difficulty.
#[inline]
pub fn tier_of(idx: usize) -> Tier {
    Tier::of_difficulty(DIFFICULTY[idx])
}

/// A set of technique *kinds* as a bitmask (`1 << kind_index`). Distinct from a
/// [`Mark`](crate::repr::Mark), the 9-bit set of digits.
pub type KindMask = u32;

/// The result of a tracked logic solve: whether the `allowed` toolbox solved the
/// board, and how many times each kind fired (for the requirement check). Returned
/// by the [`solve`](crate::solve) engines ([`LogicSolver`](crate::solve::LogicSolver)
/// and [`FusedLogicSolver`](crate::solve::FusedLogicSolver)).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SolveTrace {
    pub solved: bool,
    pub counts: [u16; NUM],
}
