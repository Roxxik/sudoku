use smallvec::SmallVec;

use crate::board::{Board, CellIdx, Digit, UnitKind};

/// A step's deductions/focus cells are len==1 ~99.7% of the time (singles), so
/// they live inline; only the rare multi-elimination (fish/subset) spills to
/// the heap. Same in-memory size as `Vec`, but no allocation on the hot path.
pub type Deductions = SmallVec<[Deduction; 1]>;
pub type FocusCells = SmallVec<[CellIdx; 1]>;

pub mod fish;
pub mod hidden_single;
pub mod hidden_subset;
pub mod locked_candidates;
pub mod naked_single;
pub mod naked_subset;
pub mod phistomefel;
pub mod turbot;
pub mod w_wing;
pub mod xy_wing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deduction {
    Place { cell: CellIdx, digit: Digit },
    Eliminate { cell: CellIdx, digit: Digit },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TechniqueKind {
    NakedSingle,
    HiddenSingle,
    LockedCandidatesPointing,
    LockedCandidatesClaiming,
    NakedPair,
    HiddenPair,
    NakedTriple,
    HiddenTriple,
    NakedQuad,
    HiddenQuad,
    XWing,
    Skyscraper,
    TwoStringKite,
    EmptyRectangle,
    FinnedXWing,
    XYWing,
    XYZWing,
    WWing,
    Swordfish,
    FinnedSwordfish,
    Jellyfish,
    FinnedJellyfish,
    PhistomefelRing,
}

/// Pedagogical grouping of techniques. Used by `Spec::allow_family` and
/// `Spec::require_family` to express training scopes independent of the flat
/// difficulty ladder (e.g. "drill any fish" without enabling wings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Singles,
    LockedCandidates,
    NakedSubset,
    HiddenSubset,
    Fish,
    TurbotFish,
    Wing,
    FinnedFish,
    SetEquality,
    // Future families (not yet populated by any TechniqueDef):
    //   SingleDigitColoring  (Simple Coloring, Multi-Coloring)
    //   Chain                (XY-Chain, Remote Pairs, AIC, Grouped AIC)
    //   Uniqueness           (UR Type 1-6, Hidden UR, BUG)
    //   ALS                  (ALS-XZ, ALS-XY-Wing, Death Blossom)
    //   ForcingChain         (Cell/Unit/Digit forcing chains and nets)
}

pub struct TechniqueDef {
    pub kind: TechniqueKind,
    pub name: &'static str,
    pub cli_name: &'static str,
    pub family: Family,
    pub difficulty: u32,
    pub find_all: fn(&Board) -> Vec<Step>,
    pub find_first: fn(&Board) -> Option<Step>,
}

// Difficulty ladder, monotonic within each family band:
//
//   10  NakedSingle              Singles
//   15  HiddenSingle             Singles
//   20  LockedCandidatesPointing LockedCandidates
//   25  LockedCandidatesClaiming LockedCandidates
//   30  NakedPair                NakedSubset
//   33  HiddenPair               HiddenSubset
//   36  NakedTriple              NakedSubset
//   39  HiddenTriple             HiddenSubset
//   42  NakedQuad                NakedSubset
//   45  HiddenQuad               HiddenSubset
//   50  XWing                    Fish
//   55  Skyscraper               TurbotFish
//   57  TwoStringKite            TurbotFish
//   60  EmptyRectangle           TurbotFish
//   65  FinnedXWing              FinnedFish
//   70  XYWing                   Wing
//   72  XYZWing                  Wing
//   75  WWing                    Wing
//   80  Swordfish                Fish
//   85  FinnedSwordfish          FinnedFish
//   90  Jellyfish                Fish
//   95  FinnedJellyfish          FinnedFish
//  135  PhistomefelRing          SetEquality
//
// Slots reserved for unimplemented techniques (insert at roughly these
// difficulties, in the listed family, when adding):
//   ~62  SimpleColoring         SingleDigitColoring
//   ~64  MultiColoring          SingleDigitColoring
//   ~73  RemotePair             Chain
//   ~74  UniqueRectangleType1   Uniqueness
//   ~76  UniqueRectangleType2-6 Uniqueness
//   ~76  BUG                    Uniqueness
//   ~78  XYChain                Chain
//   ~82  WXYZWing               Wing       (extension of XYZ-Wing)
//   ~100 AIC                    Chain
//   ~110 GroupedAIC             Chain
//   ~115 ALSXZ                  ALS
//   ~120 ALSXYWing              ALS
//   ~125 SueDeCoq               (cross-family; bands with ALS/Coloring)
//   ~130 DeathBlossom           ALS
//   ~140 ForcingChain           ForcingChain
//   ~150 ForcingNet             ForcingChain
pub const REGISTRY: &[TechniqueDef] = &[
    TechniqueDef {
        kind: TechniqueKind::NakedSingle,
        name: "naked single",
        cli_name: "naked-single",
        family: Family::Singles,
        difficulty: 10,
        find_all: naked_single::find_all,
        find_first: naked_single::find_first,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenSingle,
        name: "hidden single",
        cli_name: "hidden-single",
        family: Family::Singles,
        difficulty: 15,
        find_all: hidden_single::find_all,
        find_first: hidden_single::find_first,
    },
    TechniqueDef {
        kind: TechniqueKind::LockedCandidatesPointing,
        name: "locked candidates (pointing)",
        cli_name: "pointing",
        family: Family::LockedCandidates,
        difficulty: 20,
        find_all: locked_candidates::find_pointing,
        find_first: locked_candidates::find_first_pointing,
    },
    TechniqueDef {
        kind: TechniqueKind::LockedCandidatesClaiming,
        name: "locked candidates (claiming)",
        cli_name: "claiming",
        family: Family::LockedCandidates,
        difficulty: 25,
        find_all: locked_candidates::find_claiming,
        find_first: locked_candidates::find_first_claiming,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedPair,
        name: "naked pair",
        cli_name: "naked-pair",
        family: Family::NakedSubset,
        difficulty: 30,
        find_all: naked_subset::find_pairs,
        find_first: naked_subset::find_first_pair,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenPair,
        name: "hidden pair",
        cli_name: "hidden-pair",
        family: Family::HiddenSubset,
        difficulty: 33,
        find_all: hidden_subset::find_pairs,
        find_first: hidden_subset::find_first_pair,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedTriple,
        name: "naked triple",
        cli_name: "naked-triple",
        family: Family::NakedSubset,
        difficulty: 36,
        find_all: naked_subset::find_triples,
        find_first: naked_subset::find_first_triple,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenTriple,
        name: "hidden triple",
        cli_name: "hidden-triple",
        family: Family::HiddenSubset,
        difficulty: 39,
        find_all: hidden_subset::find_triples,
        find_first: hidden_subset::find_first_triple,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedQuad,
        name: "naked quad",
        cli_name: "naked-quad",
        family: Family::NakedSubset,
        difficulty: 42,
        find_all: naked_subset::find_quads,
        find_first: naked_subset::find_first_quad,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenQuad,
        name: "hidden quad",
        cli_name: "hidden-quad",
        family: Family::HiddenSubset,
        difficulty: 45,
        find_all: hidden_subset::find_quads,
        find_first: hidden_subset::find_first_quad,
    },
    TechniqueDef {
        kind: TechniqueKind::XWing,
        name: "X-Wing",
        cli_name: "x-wing",
        family: Family::Fish,
        difficulty: 50,
        find_all: fish::find_x_wing,
        find_first: fish::find_first_x_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::Skyscraper,
        name: "Skyscraper",
        cli_name: "skyscraper",
        family: Family::TurbotFish,
        difficulty: 55,
        find_all: turbot::find_skyscraper,
        find_first: turbot::find_first_skyscraper,
    },
    TechniqueDef {
        kind: TechniqueKind::TwoStringKite,
        name: "2-String Kite",
        cli_name: "two-string-kite",
        family: Family::TurbotFish,
        difficulty: 57,
        find_all: turbot::find_two_string_kite,
        find_first: turbot::find_first_two_string_kite,
    },
    TechniqueDef {
        kind: TechniqueKind::EmptyRectangle,
        name: "Empty Rectangle",
        cli_name: "empty-rectangle",
        family: Family::TurbotFish,
        difficulty: 60,
        find_all: turbot::find_empty_rectangle,
        find_first: turbot::find_first_empty_rectangle,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedXWing,
        name: "Finned X-Wing",
        cli_name: "finned-x-wing",
        family: Family::FinnedFish,
        difficulty: 65,
        find_all: fish::find_finned_x_wing,
        find_first: fish::find_first_finned_x_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::XYWing,
        name: "XY-Wing",
        cli_name: "xy-wing",
        family: Family::Wing,
        difficulty: 70,
        find_all: xy_wing::find_all,
        find_first: xy_wing::find_first,
    },
    TechniqueDef {
        kind: TechniqueKind::XYZWing,
        name: "XYZ-Wing",
        cli_name: "xyz-wing",
        family: Family::Wing,
        difficulty: 72,
        find_all: xy_wing::find_xyz_wing,
        find_first: xy_wing::find_first_xyz_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::WWing,
        name: "W-Wing",
        cli_name: "w-wing",
        family: Family::Wing,
        difficulty: 75,
        find_all: w_wing::find_all,
        find_first: w_wing::find_first,
    },
    TechniqueDef {
        kind: TechniqueKind::Swordfish,
        name: "Swordfish",
        cli_name: "swordfish",
        family: Family::Fish,
        difficulty: 80,
        find_all: fish::find_swordfish,
        find_first: fish::find_first_swordfish,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedSwordfish,
        name: "Finned Swordfish",
        cli_name: "finned-swordfish",
        family: Family::FinnedFish,
        difficulty: 85,
        find_all: fish::find_finned_swordfish,
        find_first: fish::find_first_finned_swordfish,
    },
    TechniqueDef {
        kind: TechniqueKind::Jellyfish,
        name: "Jellyfish",
        cli_name: "jellyfish",
        family: Family::Fish,
        difficulty: 90,
        find_all: fish::find_jellyfish,
        find_first: fish::find_first_jellyfish,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedJellyfish,
        name: "Finned Jellyfish",
        cli_name: "finned-jellyfish",
        family: Family::FinnedFish,
        difficulty: 95,
        find_all: fish::find_finned_jellyfish,
        find_first: fish::find_first_finned_jellyfish,
    },
    TechniqueDef {
        kind: TechniqueKind::PhistomefelRing,
        name: "Phistomefel Ring",
        cli_name: "phistomefel-ring",
        family: Family::SetEquality,
        difficulty: 135,
        find_all: phistomefel::find_all,
        find_first: phistomefel::find_first,
    },
];

/// Number of distinct [`TechniqueKind`] variants. Every kind has exactly one
/// REGISTRY entry (asserted in tests), so this is both the registry length and
/// the exclusive upper bound of `kind as usize` — the index space [`Spec`] uses
/// for its dense, allocation-free technique tables.
pub const NUM_TECHNIQUE_KINDS: usize = REGISTRY.len();

impl TechniqueKind {
    /// Dense index in `0..NUM_TECHNIQUE_KINDS`, the enum discriminant. Used to
    /// address per-technique arrays and bitmasks without hashing.
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn def(self) -> &'static TechniqueDef {
        REGISTRY
            .iter()
            .find(|d| d.kind == self)
            .expect("every TechniqueKind has a registry entry")
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    pub fn cli_name(self) -> &'static str {
        self.def().cli_name
    }

    pub fn difficulty(self) -> u32 {
        self.def().difficulty
    }

    pub fn family(self) -> Family {
        self.def().family
    }
}

impl Family {
    /// All technique kinds currently implemented under this family, in
    /// difficulty order. Empty for unimplemented families.
    pub fn members(self) -> Vec<TechniqueKind> {
        REGISTRY
            .iter()
            .filter(|d| d.family == self)
            .map(|d| d.kind)
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HouseRef {
    pub kind: UnitKind,
    pub index: u8,
}

impl HouseRef {
    pub fn describe(&self) -> String {
        let label = match self.kind {
            UnitKind::Row => "row",
            UnitKind::Col => "column",
            UnitKind::Box => "box",
        };
        format!("{} {}", label, self.index + 1)
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub technique: TechniqueKind,
    pub deductions: Deductions,
    pub focus_cells: FocusCells,
    pub focus_house: Option<HouseRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_kinds_unique() {
        let mut seen = HashSet::new();
        for d in REGISTRY {
            assert!(seen.insert(d.kind), "duplicate REGISTRY entry for {:?}", d.kind);
        }
    }

    #[test]
    fn registry_cli_names_unique() {
        let mut seen = HashSet::new();
        for d in REGISTRY {
            assert!(seen.insert(d.cli_name), "duplicate cli_name {:?}", d.cli_name);
        }
    }

    #[test]
    fn registry_is_difficulty_sorted() {
        let mut prev: Option<u32> = None;
        for d in REGISTRY {
            if let Some(p) = prev {
                assert!(p < d.difficulty, "REGISTRY must be sorted by difficulty");
            }
            prev = Some(d.difficulty);
        }
    }

    #[test]
    fn every_family_has_at_least_one_member_or_is_future() {
        // Implemented families must have at least one technique mapped to them.
        let implemented = [
            Family::Singles,
            Family::LockedCandidates,
            Family::NakedSubset,
            Family::HiddenSubset,
            Family::Fish,
            Family::TurbotFish,
            Family::Wing,
            Family::FinnedFish,
            Family::SetEquality,
        ];
        for f in implemented {
            assert!(!f.members().is_empty(), "family {:?} has no members", f);
        }
    }
}
