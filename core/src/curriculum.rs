use crate::spec::Spec;
use crate::techniques::{REGISTRY, TechniqueKind};

/// Player-facing difficulty tiers, matching `CURRICULUM.md`. A technique's tier
/// is the band its player-facing `difficulty` score falls into; the cut-points
/// follow SudokuExplainer's thresholds. `Ord` runs Beginner < Intermediate <
/// Expert < Master, so "easier tier" is a plain comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// Hidden singles only (SE ~1.0-1.5).
    Beginner,
    /// Naked single and locked candidates (SE < 3.0).
    Intermediate,
    /// The three trainable branches: single-digit, bivalue, subsets (SE 3.0-~5.4).
    Expert,
    /// Advanced set logic and beyond (SE above ~5.4).
    Master,
}

impl Tier {
    /// Player-difficulty cut-points (upper-exclusive). A score below the next
    /// tier's floor belongs to the lower tier. See `CURRICULUM.md`.
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

    /// The tier a technique belongs to, by its player-facing difficulty.
    pub fn of(kind: TechniqueKind) -> Tier {
        Self::of_difficulty(kind.difficulty())
    }

    /// A representative hardest technique in this tier, for display. `None` for
    /// [`Tier::Master`], which has no cap.
    pub fn ceiling(self) -> Option<TechniqueKind> {
        match self {
            Tier::Beginner => Some(TechniqueKind::HiddenSingle),
            Tier::Intermediate => Some(TechniqueKind::LockedCandidatesClaiming),
            Tier::Expert => Some(TechniqueKind::HiddenQuad),
            Tier::Master => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tier::Beginner => "beginner",
            Tier::Intermediate => "intermediate",
            Tier::Expert => "expert",
            Tier::Master => "master",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "beginner" => Some(Tier::Beginner),
            "intermediate" => Some(Tier::Intermediate),
            "expert" => Some(Tier::Expert),
            "master" => Some(Tier::Master),
            _ => None,
        }
    }

    pub const ALL: &'static [Tier] =
        &[Tier::Beginner, Tier::Intermediate, Tier::Expert, Tier::Master];
}

/// A single training stage in the curriculum. Maps a CLI/UI key to a focus
/// technique; the resulting `Spec` enables every easier technique and forces
/// the focus to appear (Broad mode — see [`Spec::train`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage {
    pub key: &'static str,
    pub label: &'static str,
    pub focus: TechniqueKind,
}

impl Stage {
    pub fn broad_spec(&self) -> Spec {
        Spec::train(self.focus)
    }

    pub fn drill_spec(&self) -> Spec {
        Spec::drill(self.focus)
    }
}

/// Linear training curriculum: the player-surfaced techniques, ordered from
/// easiest to hardest. A curated subset of `REGISTRY` — turbot and finned fish
/// are implemented but not surfaced here (see `CURRICULUM.md`). Used by
/// `--list-stages` and the `--stage` flag.
pub const CURRICULUM: &[Stage] = &[
    Stage {
        key: "singles-naked",
        label: "Naked Singles",
        focus: TechniqueKind::NakedSingle,
    },
    Stage {
        key: "singles-hidden",
        label: "Hidden Singles",
        focus: TechniqueKind::HiddenSingle,
    },
    Stage {
        key: "lc-pointing",
        label: "Locked Candidates: Pointing",
        focus: TechniqueKind::LockedCandidatesPointing,
    },
    Stage {
        key: "lc-claiming",
        label: "Locked Candidates: Claiming",
        focus: TechniqueKind::LockedCandidatesClaiming,
    },
    Stage {
        key: "pair-naked",
        label: "Naked Pairs",
        focus: TechniqueKind::NakedPair,
    },
    Stage {
        key: "pair-hidden",
        label: "Hidden Pairs",
        focus: TechniqueKind::HiddenPair,
    },
    Stage {
        key: "triple-naked",
        label: "Naked Triples",
        focus: TechniqueKind::NakedTriple,
    },
    Stage {
        key: "triple-hidden",
        label: "Hidden Triples",
        focus: TechniqueKind::HiddenTriple,
    },
    Stage {
        key: "quad-naked",
        label: "Naked Quads",
        focus: TechniqueKind::NakedQuad,
    },
    Stage {
        key: "quad-hidden",
        label: "Hidden Quads",
        focus: TechniqueKind::HiddenQuad,
    },
    Stage {
        key: "x-wing",
        label: "X-Wing",
        focus: TechniqueKind::XWing,
    },
    Stage {
        key: "xy-wing",
        label: "XY-Wing",
        focus: TechniqueKind::XYWing,
    },
    Stage {
        key: "xyz-wing",
        label: "XYZ-Wing",
        focus: TechniqueKind::XYZWing,
    },
    Stage {
        key: "w-wing",
        label: "W-Wing",
        focus: TechniqueKind::WWing,
    },
    Stage {
        key: "swordfish",
        label: "Swordfish",
        focus: TechniqueKind::Swordfish,
    },
    Stage {
        key: "jellyfish",
        label: "Jellyfish",
        focus: TechniqueKind::Jellyfish,
    },
];

pub fn stage_by_key(key: &str) -> Option<&'static Stage> {
    CURRICULUM.iter().find(|s| s.key == key)
}

/// Whether a curriculum technique (addressed by its [`lab::kinds`](crate::lab::kinds)
/// index) is worth offering a separate **Drill** mode alongside Train.
///
/// Train always exists. Drill is dropped wherever it would be indistinguishable
/// from Train:
///   - **Beginner** (hidden singles) is train-only — there is nothing easier to
///     concede, so a drill is the same puzzle as a train.
///   - The **easiest technique of each Expert branch** has no easier same-branch
///     peer for a drill to set apart from train, so its drill collapses onto its
///     train; only Train is surfaced (e.g. X-Wing, Naked Pair, XY-Wing).
///
/// The build-time `gen_curriculum` example and the wasm `curriculum()` export both
/// read this, so the Home tree's per-technique mode buttons can't drift.
pub fn lab_kind_has_drill(idx: usize) -> bool {
    use crate::lab::kinds::{self, Tier};
    let tier = kinds::tier_of(idx);
    // Beginner is train-only; an Expert branch's easiest kind drills the same as
    // it trains. Everything else (Intermediate, deeper Expert kinds) gets a drill.
    if tier == Tier::Beginner {
        return false;
    }
    !(tier == Tier::Expert && first_in_branch(idx))
}

/// Whether `idx` is the easiest kind in its [`Branch`](crate::lab::kinds::Branch),
/// by player-facing difficulty (no same-branch kind is strictly easier).
fn first_in_branch(idx: usize) -> bool {
    use crate::lab::kinds;
    let branch = kinds::branch_of(idx);
    let d = kinds::DIFFICULTY[idx];
    !(0..kinds::NUM).any(|j| kinds::branch_of(j) == branch && kinds::DIFFICULTY[j] < d)
}

impl Spec {
    /// Allow every technique in this tier and below, by player-facing
    /// difficulty. `Tier::Master` allows everything implemented.
    pub fn tier(t: Tier) -> Self {
        let mut spec = Self::default();
        for def in REGISTRY {
            if Tier::of(def.kind) <= t {
                spec = spec.allow(def.kind);
            }
        }
        spec
    }

    /// Broad-mode training spec for the named stage.
    pub fn for_stage(stage: &Stage) -> Self {
        stage.broad_spec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::techniques::REGISTRY;

    #[test]
    fn every_curriculum_key_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in CURRICULUM {
            assert!(seen.insert(s.key), "duplicate stage key: {}", s.key);
        }
    }

    #[test]
    #[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
    fn curriculum_is_difficulty_sorted() {
        let mut prev: Option<u32> = None;
        for s in CURRICULUM {
            let d = s.focus.solver_order();
            if let Some(p) = prev {
                assert!(p < d, "curriculum must be sorted by difficulty");
            }
            prev = Some(d);
        }
    }

    #[test]
    fn tier_master_allows_all() {
        let s = Spec::tier(Tier::Master);
        for d in REGISTRY {
            assert!(s.is_in_scope(d.kind), "missing {:?}", d.kind);
        }
    }

    #[test]
    fn tier_beginner_only_hidden_single() {
        // Beginner is hidden singles only; naked single is Intermediate.
        let s = Spec::tier(Tier::Beginner);
        assert!(s.is_in_scope(TechniqueKind::HiddenSingle));
        assert!(!s.is_in_scope(TechniqueKind::NakedSingle));
        assert!(!s.is_in_scope(TechniqueKind::NakedPair));
    }

    #[test]
    fn tier_classification_matches_cut_points() {
        assert_eq!(Tier::of(TechniqueKind::HiddenSingle), Tier::Beginner); // 5
        assert_eq!(Tier::of(TechniqueKind::NakedSingle), Tier::Intermediate); // 15
        assert_eq!(
            Tier::of(TechniqueKind::LockedCandidatesClaiming),
            Tier::Intermediate
        ); // 26
        assert_eq!(Tier::of(TechniqueKind::NakedPair), Tier::Expert); // 32
        assert_eq!(Tier::of(TechniqueKind::Jellyfish), Tier::Expert); // 86
        assert_eq!(Tier::of(TechniqueKind::Skyscraper), Tier::Master); // 100 (SE-scored)
        assert_eq!(Tier::of(TechniqueKind::PhistomefelRing), Tier::Master); // 110
    }

    #[test]
    fn tier_round_trip() {
        for &t in Tier::ALL {
            assert_eq!(Tier::from_key(t.key()), Some(t));
        }
    }

    #[test]
    fn stage_by_key_lookup() {
        let s = stage_by_key("swordfish").expect("swordfish stage exists");
        assert_eq!(s.focus, TechniqueKind::Swordfish);
    }

    #[test]
    fn drill_dropped_for_beginner_and_expert_branch_firsts() {
        use crate::lab::kinds;
        // Beginner (hidden single) is train-only.
        assert!(!lab_kind_has_drill(kinds::HIDDEN_SINGLE));
        // Each Expert branch's easiest kind drills the same as it trains.
        assert!(!lab_kind_has_drill(kinds::NAKED_PAIR)); // Subset
        assert!(!lab_kind_has_drill(kinds::X_WING)); // Fish
        assert!(!lab_kind_has_drill(kinds::XY_WING)); // Bivalue
        // Intermediate and deeper Expert kinds keep their drill.
        assert!(lab_kind_has_drill(kinds::NAKED_SINGLE)); // Intermediate
        assert!(lab_kind_has_drill(kinds::LC_POINTING)); // Intermediate
        assert!(lab_kind_has_drill(kinds::HIDDEN_PAIR)); // Subset, not first
        assert!(lab_kind_has_drill(kinds::SWORDFISH)); // Fish, not first
        assert!(lab_kind_has_drill(kinds::W_WING)); // Bivalue, not first
    }
}
