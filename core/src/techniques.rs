use crate::board::{Board, CellIdx, Digit, UnitKind};

pub mod fish;
pub mod hidden_single;
pub mod hidden_subset;
pub mod locked_candidates;
pub mod naked_single;
pub mod naked_subset;
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
    XYWing,
    XYZWing,
    WWing,
    Skyscraper,
    TwoStringKite,
    Swordfish,
    EmptyRectangle,
    FinnedXWing,
    Jellyfish,
    FinnedSwordfish,
    FinnedJellyfish,
}

pub struct TechniqueDef {
    pub kind: TechniqueKind,
    pub name: &'static str,
    pub cli_name: &'static str,
    pub difficulty: u32,
    pub find_all: fn(&Board) -> Vec<Step>,
}

pub const REGISTRY: &[TechniqueDef] = &[
    TechniqueDef {
        kind: TechniqueKind::NakedSingle,
        name: "naked single",
        cli_name: "naked-single",
        difficulty: 10,
        find_all: naked_single::find_all,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenSingle,
        name: "hidden single",
        cli_name: "hidden-single",
        difficulty: 15,
        find_all: hidden_single::find_all,
    },
    TechniqueDef {
        kind: TechniqueKind::LockedCandidatesPointing,
        name: "locked candidates (pointing)",
        cli_name: "pointing",
        difficulty: 20,
        find_all: locked_candidates::find_pointing,
    },
    TechniqueDef {
        kind: TechniqueKind::LockedCandidatesClaiming,
        name: "locked candidates (claiming)",
        cli_name: "claiming",
        difficulty: 25,
        find_all: locked_candidates::find_claiming,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedPair,
        name: "naked pair",
        cli_name: "naked-pair",
        difficulty: 30,
        find_all: naked_subset::find_pairs,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenPair,
        name: "hidden pair",
        cli_name: "hidden-pair",
        difficulty: 35,
        find_all: hidden_subset::find_pairs,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedTriple,
        name: "naked triple",
        cli_name: "naked-triple",
        difficulty: 40,
        find_all: naked_subset::find_triples,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenTriple,
        name: "hidden triple",
        cli_name: "hidden-triple",
        difficulty: 45,
        find_all: hidden_subset::find_triples,
    },
    TechniqueDef {
        kind: TechniqueKind::NakedQuad,
        name: "naked quad",
        cli_name: "naked-quad",
        difficulty: 50,
        find_all: naked_subset::find_quads,
    },
    TechniqueDef {
        kind: TechniqueKind::HiddenQuad,
        name: "hidden quad",
        cli_name: "hidden-quad",
        difficulty: 55,
        find_all: hidden_subset::find_quads,
    },
    TechniqueDef {
        kind: TechniqueKind::XWing,
        name: "X-Wing",
        cli_name: "x-wing",
        difficulty: 60,
        find_all: fish::find_x_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::XYWing,
        name: "XY-Wing",
        cli_name: "xy-wing",
        difficulty: 65,
        find_all: xy_wing::find_all,
    },
    TechniqueDef {
        kind: TechniqueKind::XYZWing,
        name: "XYZ-Wing",
        cli_name: "xyz-wing",
        difficulty: 66,
        find_all: xy_wing::find_xyz_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::WWing,
        name: "W-Wing",
        cli_name: "w-wing",
        difficulty: 67,
        find_all: w_wing::find_all,
    },
    TechniqueDef {
        kind: TechniqueKind::Skyscraper,
        name: "Skyscraper",
        cli_name: "skyscraper",
        difficulty: 68,
        find_all: turbot::find_skyscraper,
    },
    TechniqueDef {
        kind: TechniqueKind::TwoStringKite,
        name: "2-String Kite",
        cli_name: "two-string-kite",
        difficulty: 69,
        find_all: turbot::find_two_string_kite,
    },
    TechniqueDef {
        kind: TechniqueKind::Swordfish,
        name: "Swordfish",
        cli_name: "swordfish",
        difficulty: 70,
        find_all: fish::find_swordfish,
    },
    TechniqueDef {
        kind: TechniqueKind::EmptyRectangle,
        name: "Empty Rectangle",
        cli_name: "empty-rectangle",
        difficulty: 71,
        find_all: turbot::find_empty_rectangle,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedXWing,
        name: "Finned X-Wing",
        cli_name: "finned-x-wing",
        difficulty: 73,
        find_all: fish::find_finned_x_wing,
    },
    TechniqueDef {
        kind: TechniqueKind::Jellyfish,
        name: "Jellyfish",
        cli_name: "jellyfish",
        difficulty: 75,
        find_all: fish::find_jellyfish,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedSwordfish,
        name: "Finned Swordfish",
        cli_name: "finned-swordfish",
        difficulty: 78,
        find_all: fish::find_finned_swordfish,
    },
    TechniqueDef {
        kind: TechniqueKind::FinnedJellyfish,
        name: "Finned Jellyfish",
        cli_name: "finned-jellyfish",
        difficulty: 80,
        find_all: fish::find_finned_jellyfish,
    },
];

impl TechniqueKind {
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
    pub deductions: Vec<Deduction>,
    pub focus_cells: Vec<CellIdx>,
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
}
