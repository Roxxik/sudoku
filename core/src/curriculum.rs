use crate::spec::Spec;
use crate::techniques::TechniqueKind;

/// Coarse difficulty buckets that cap the techniques a `Spec` will allow.
/// Each tier corresponds to a difficulty ceiling on the flat ladder defined
/// in `techniques::REGISTRY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Singles only.
    Easy,
    /// Through hidden quads.
    Medium,
    /// Through wings (but no advanced fish: Swordfish/Jellyfish/Finned).
    Hard,
    /// Everything implemented, no cap.
    Master,
}

impl Tier {
    /// The technique whose difficulty acts as the inclusive ceiling for this
    /// tier. `None` for [`Tier::Master`], which has no cap.
    pub fn ceiling(self) -> Option<TechniqueKind> {
        match self {
            Tier::Easy => Some(TechniqueKind::HiddenSingle),
            Tier::Medium => Some(TechniqueKind::HiddenQuad),
            Tier::Hard => Some(TechniqueKind::WWing),
            Tier::Master => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tier::Easy => "easy",
            Tier::Medium => "medium",
            Tier::Hard => "hard",
            Tier::Master => "master",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "easy" => Some(Tier::Easy),
            "medium" => Some(Tier::Medium),
            "hard" => Some(Tier::Hard),
            "master" => Some(Tier::Master),
            _ => None,
        }
    }

    pub const ALL: &'static [Tier] = &[Tier::Easy, Tier::Medium, Tier::Hard, Tier::Master];
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

/// Linear training curriculum: one stage per implemented technique, ordered
/// from easiest to hardest. Used by `--list-stages` and the `--stage` flag.
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
        key: "skyscraper",
        label: "Skyscraper",
        focus: TechniqueKind::Skyscraper,
    },
    Stage {
        key: "two-string-kite",
        label: "2-String Kite",
        focus: TechniqueKind::TwoStringKite,
    },
    Stage {
        key: "empty-rectangle",
        label: "Empty Rectangle",
        focus: TechniqueKind::EmptyRectangle,
    },
    Stage {
        key: "finned-x-wing",
        label: "Finned X-Wing",
        focus: TechniqueKind::FinnedXWing,
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
        key: "finned-swordfish",
        label: "Finned Swordfish",
        focus: TechniqueKind::FinnedSwordfish,
    },
    Stage {
        key: "jellyfish",
        label: "Jellyfish",
        focus: TechniqueKind::Jellyfish,
    },
    Stage {
        key: "finned-jellyfish",
        label: "Finned Jellyfish",
        focus: TechniqueKind::FinnedJellyfish,
    },
];

pub fn stage_by_key(key: &str) -> Option<&'static Stage> {
    CURRICULUM.iter().find(|s| s.key == key)
}

impl Spec {
    /// All techniques whose difficulty is at or below this tier's ceiling.
    /// `Tier::Master` returns [`Spec::allow_all`].
    pub fn tier(t: Tier) -> Self {
        match t.ceiling() {
            Some(cap) => Spec::allow_up_to(cap),
            None => Spec::allow_all(),
        }
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
    fn curriculum_covers_every_registered_technique() {
        let focused: std::collections::HashSet<_> =
            CURRICULUM.iter().map(|s| s.focus).collect();
        for d in REGISTRY {
            assert!(
                focused.contains(&d.kind),
                "no curriculum stage for {:?}",
                d.kind
            );
        }
    }

    #[test]
    fn curriculum_is_difficulty_sorted() {
        let mut prev: Option<u32> = None;
        for s in CURRICULUM {
            let d = s.focus.difficulty();
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
    fn tier_easy_only_singles() {
        let s = Spec::tier(Tier::Easy);
        assert!(s.is_in_scope(TechniqueKind::NakedSingle));
        assert!(s.is_in_scope(TechniqueKind::HiddenSingle));
        assert!(!s.is_in_scope(TechniqueKind::NakedPair));
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
}
