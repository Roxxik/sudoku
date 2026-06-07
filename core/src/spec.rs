use std::num::NonZeroUsize;

use crate::curriculum::Tier;
use crate::techniques::{Family, NUM_TECHNIQUE_KINDS, REGISTRY, TechniqueKind};

/// A set of technique kinds packed into a single `u32` bitmask, indexed by
/// [`TechniqueKind::index`]. There are fewer than 32 kinds, so the whole set
/// fits in one word — replacing `HashSet<TechniqueKind>` with no allocation,
/// no hashing, and `Copy` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TechniqueSet(u32);

impl TechniqueSet {
    pub const fn new() -> Self {
        TechniqueSet(0)
    }

    #[inline]
    pub fn insert(&mut self, t: TechniqueKind) {
        self.0 |= 1u32 << t.index();
    }

    #[inline]
    pub fn contains(&self, t: TechniqueKind) -> bool {
        self.0 & (1u32 << t.index()) != 0
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Raw bitmask, for callers that want to test membership directly.
    #[inline]
    pub fn bits(&self) -> u32 {
        self.0
    }

    /// The contained kinds, in REGISTRY (difficulty) order.
    pub fn iter(&self) -> impl Iterator<Item = TechniqueKind> + '_ {
        REGISTRY
            .iter()
            .map(|d| d.kind)
            .filter(move |&k| self.contains(k))
    }
}

impl FromIterator<TechniqueKind> for TechniqueSet {
    fn from_iter<I: IntoIterator<Item = TechniqueKind>>(iter: I) -> Self {
        let mut set = TechniqueSet::new();
        for k in iter {
            set.insert(k);
        }
        set
    }
}

/// How a single technique participates in a [`Spec`]. Two predicates run over
/// these variants during verification:
///
/// - **Baseline solvability** (positive): "puzzle must be solvable with only
///   these techniques." Uses `Allowed | Forced`.
/// - **Forced irreplaceability** (negative): "Forced techniques must be
///   unavoidable." The avoid-target walk uses `Allowed | Conceded` — i.e.,
///   a hypothetical solver is granted the Conceded toolbox in addition to
///   Allowed, and must still get stuck without the Forced technique.
///
/// `Conceded` therefore appears only in the negative check. A Conceded
/// technique may fire freely if it happens to, but the puzzle is *not*
/// promised to be solvable with it, and it must not substitute for Forced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Allowed,
    Forced { count: NonZeroUsize },
    Conceded,
}

impl Usage {
    pub fn forced(n: usize) -> Self {
        Self::Forced {
            count: NonZeroUsize::new(n).expect("Forced count must be at least 1"),
        }
    }
}

/// "At least `count` total uses drawn from `kinds`." Used to express family-
/// level training requirements (e.g. "any fish must appear at least once")
/// without forcing a specific technique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireAny {
    pub kinds: TechniqueSet,
    pub count: NonZeroUsize,
}

/// Per-technique configuration for a generation/verification target.
///
/// `usages` is a dense array indexed by [`TechniqueKind::index`] rather than a
/// `HashMap`: every membership query (`is_baseline`, `is_in_scope`) is a single
/// array load with no hashing, which matters because the solver filters every
/// technique on every step. `None` means "out of scope".
#[derive(Debug, Clone)]
pub struct Spec {
    usages: [Option<Usage>; NUM_TECHNIQUE_KINDS],
    pub require_any: Vec<RequireAny>,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            usages: [None; NUM_TECHNIQUE_KINDS],
            require_any: Vec::new(),
        }
    }
}

impl Spec {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn allow_all() -> Self {
        let mut spec = Self::default();
        for def in REGISTRY {
            spec.usages[def.kind.index()] = Some(Usage::Allowed);
        }
        spec
    }

    #[allow(deprecated)] // solver_order: pending difficulty/curriculum migration
    pub fn allow_up_to(t: TechniqueKind) -> Self {
        let cap = t.solver_order();
        let mut spec = Self::default();
        for def in REGISTRY {
            if def.solver_order <= cap {
                spec.usages[def.kind.index()] = Some(Usage::Allowed);
            }
        }
        spec
    }

    pub fn allow(mut self, t: TechniqueKind) -> Self {
        self.usages[t.index()] = Some(Usage::Allowed);
        self
    }

    pub fn allow_family(mut self, family: Family) -> Self {
        for t in family.members() {
            self.usages[t.index()].get_or_insert(Usage::Allowed);
        }
        self
    }

    pub fn require(mut self, t: TechniqueKind, count: usize) -> Self {
        self.usages[t.index()] = Some(Usage::forced(count));
        self
    }

    /// Concede `t` to the irreplaceability check: it may fire freely but is
    /// not part of the solvability baseline, and the Forced check assumes a
    /// hypothetical solver has access to it when proving the Forced
    /// techniques are unavoidable.
    pub fn concede(mut self, t: TechniqueKind) -> Self {
        self.usages[t.index()] = Some(Usage::Conceded);
        self
    }

    /// Require at least `count` total uses drawn from any of the given techniques.
    /// Members not already present in `usages` are added as `Allowed` — the
    /// solver must be able to use them at all in order to satisfy the constraint.
    pub fn require_one_of<I>(mut self, kinds: I, count: usize) -> Self
    where
        I: IntoIterator<Item = TechniqueKind>,
    {
        let set: TechniqueSet = kinds.into_iter().collect();
        assert!(!set.is_empty(), "require_one_of needs at least one technique");
        for k in set.iter() {
            self.usages[k.index()].get_or_insert(Usage::Allowed);
        }
        self.require_any.push(RequireAny {
            kinds: set,
            count: NonZeroUsize::new(count).expect("require_one_of count must be at least 1"),
        });
        self
    }

    /// Require at least `count` uses from any technique in `family`. Implies
    /// `allow_family(family)`.
    pub fn require_family(self, family: Family, count: usize) -> Self {
        let members = family.members();
        assert!(
            !members.is_empty(),
            "family {:?} has no implemented members yet",
            family
        );
        self.require_one_of(members, count)
    }

    /// Broad-mode training for `target`: force it, and allow what the player may
    /// lean on to reach it. The whole Trunk (Beginner + Intermediate) is allowed
    /// unconditionally; within `target`'s own branch, the simpler-or-equal
    /// Expert/Master techniques are allowed too. Branch-scoped — training a fish
    /// never enables subsets. See `CURRICULUM.md`.
    pub fn train(target: TechniqueKind) -> Self {
        let target_tier = Tier::of(target);
        let target_branch = target.branch();
        let target_diff = target.difficulty();
        let mut spec = Self::default();
        for def in REGISTRY {
            let tt = Tier::of(def.kind);
            if tt > target_tier {
                continue;
            }
            let allowed = if tt <= Tier::Intermediate {
                // Trunk: always available, up to the target's tier.
                true
            } else {
                // Branch technique: same branch as the target, simpler-or-equal.
                def.kind.branch() == target_branch && def.difficulty <= target_diff
            };
            if allowed {
                spec.usages[def.kind.index()] = Some(Usage::Allowed);
            }
        }
        spec.require(target, 1)
    }

    /// Drill-mode training for `target`: force it, allow every *easier tier* in
    /// full, and *concede* its in-tier peers — the rest of the flat Intermediate
    /// tier, or the simpler same-branch techniques in Expert/Master. Conceded
    /// techniques may fire but must not substitute for the target, so the drill
    /// isolates `target` against its immediate neighbours. Harder-or-other-branch
    /// peers are out of scope entirely. See `CURRICULUM.md`.
    ///
    /// Drill yields fewer puzzles per attempt than `train` (it concedes rather
    /// than allows the in-tier peers), but generates fine. In particular,
    /// conceding naked single in the flat Intermediate tier — so a baseline solve
    /// runs on hidden singles + target alone — is not the obstacle it looks like:
    /// a naked single is rarely the *only* route to its cell (the same placement
    /// is usually also a hidden single in one of its units), so hidden singles
    /// substitute freely and a naked-single-free solve almost always exists.
    pub fn drill(target: TechniqueKind) -> Self {
        let target_tier = Tier::of(target);
        let target_branch = target.branch();
        let target_diff = target.difficulty();
        let mut spec = Self::default();
        for def in REGISTRY {
            if def.kind == target {
                continue;
            }
            let tt = Tier::of(def.kind);
            if tt < target_tier {
                // Easier tiers are allowed in full.
                spec.usages[def.kind.index()] = Some(Usage::Allowed);
            } else if tt == target_tier {
                let concede = match target_tier {
                    // Beginner is train-only; nothing to concede.
                    Tier::Beginner => false,
                    // Flat tier: concede every other Intermediate technique.
                    Tier::Intermediate => true,
                    // Branch ladder: concede the simpler same-branch peers.
                    Tier::Expert | Tier::Master => {
                        def.kind.branch() == target_branch && def.difficulty < target_diff
                    }
                };
                if concede {
                    spec.usages[def.kind.index()] = Some(Usage::Conceded);
                }
            }
            // tt > target_tier: out of scope.
        }
        spec.require(target, 1)
    }

    /// True if `t` is in the baseline (Allowed or Forced) — i.e., counts as
    /// "the solver actually relies on this" for the positive solvability
    /// check and for the displayed solution trace.
    #[inline]
    pub fn is_baseline(&self, t: TechniqueKind) -> bool {
        matches!(
            self.usages[t.index()],
            Some(Usage::Allowed) | Some(Usage::Forced { .. }),
        )
    }

    /// True if `t` appears in the spec under any variant.
    #[inline]
    pub fn is_in_scope(&self, t: TechniqueKind) -> bool {
        self.usages[t.index()].is_some()
    }

    /// The configured [`Usage`] for `t`, or `None` if `t` is out of scope.
    #[inline]
    pub fn usage(&self, t: TechniqueKind) -> Option<Usage> {
        self.usages[t.index()]
    }

    /// Iterate the in-scope `(technique, usage)` pairs, in REGISTRY (difficulty)
    /// order. Replaces iterating a `HashMap` — and unlike the old map, the order
    /// is now deterministic.
    pub fn iter_usages(&self) -> impl Iterator<Item = (TechniqueKind, Usage)> + '_ {
        REGISTRY
            .iter()
            .filter_map(move |def| self.usages[def.kind.index()].map(|u| (def.kind, u)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_up_to_includes_only_easier_or_equal() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenSingle);
        assert!(s.is_in_scope(TechniqueKind::NakedSingle));
        assert!(s.is_in_scope(TechniqueKind::HiddenSingle));
        assert!(!s.is_in_scope(TechniqueKind::NakedPair));
        assert!(!s.is_in_scope(TechniqueKind::HiddenTriple));
    }

    #[test]
    fn builder_chains() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenPair)
            .require(TechniqueKind::HiddenPair, 2);
        assert!(matches!(
            s.usage(TechniqueKind::HiddenPair),
            Some(Usage::Forced { .. })
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedSingle),
            Some(Usage::Allowed)
        ));
    }

    #[test]
    fn empty_spec_lists_nothing() {
        let s = Spec::empty();
        assert!(s.iter_usages().next().is_none());
        assert!(s.require_any.is_empty());
    }

    #[test]
    #[should_panic]
    fn forced_zero_panics() {
        let _ = Usage::forced(0);
    }

    #[test]
    fn allow_family_adds_all_members() {
        let s = Spec::empty().allow_family(Family::Wing);
        // Wing family contains XYWing, XYZWing, WWing in the current registry.
        assert!(s.is_in_scope(TechniqueKind::XYWing));
        assert!(s.is_in_scope(TechniqueKind::XYZWing));
        assert!(s.is_in_scope(TechniqueKind::WWing));
        // Non-Wing techniques should not be added.
        assert!(!s.is_in_scope(TechniqueKind::Swordfish));
    }

    #[test]
    fn allow_family_does_not_clobber_forced() {
        let s = Spec::empty()
            .require(TechniqueKind::XYWing, 1)
            .allow_family(Family::Wing);
        // Forced status must be preserved (entry not overwritten by Allowed).
        assert!(matches!(
            s.usage(TechniqueKind::XYWing),
            Some(Usage::Forced { .. })
        ));
    }

    #[test]
    fn require_family_creates_require_any() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenQuad).require_family(Family::Fish, 1);
        assert_eq!(s.require_any.len(), 1);
        let r = &s.require_any[0];
        assert!(r.kinds.contains(TechniqueKind::XWing));
        assert!(r.kinds.contains(TechniqueKind::Swordfish));
        assert!(r.kinds.contains(TechniqueKind::Jellyfish));
        assert_eq!(r.count.get(), 1);
        // Fishes that weren't in allow_up_to(HiddenQuad) are now allowed.
        assert!(s.is_in_scope(TechniqueKind::Swordfish));
    }

    #[test]
    #[should_panic]
    fn require_one_of_empty_panics() {
        let _ = Spec::empty().require_one_of(std::iter::empty(), 1);
    }

    #[test]
    fn train_is_branch_scoped() {
        let s = Spec::train(TechniqueKind::Swordfish);
        // Whole Trunk (Beginner + Intermediate) is allowed unconditionally.
        assert!(s.is_in_scope(TechniqueKind::NakedSingle));
        assert!(s.is_in_scope(TechniqueKind::HiddenSingle));
        assert!(s.is_in_scope(TechniqueKind::LockedCandidatesClaiming));
        // Same branch (single-digit), simpler.
        assert!(s.is_in_scope(TechniqueKind::XWing));
        // Other Expert branches are excluded even when simpler than the target.
        assert!(!s.is_in_scope(TechniqueKind::HiddenPair)); // subset, diff 44 < 56
        assert!(!s.is_in_scope(TechniqueKind::XYWing)); // bivalue
        // Harder same-branch excluded.
        assert!(!s.is_in_scope(TechniqueKind::Jellyfish));
        // Target forced.
        assert!(matches!(
            s.usage(TechniqueKind::Swordfish),
            Some(Usage::Forced { .. })
        ));
    }

    #[test]
    fn drill_expert_concedes_simpler_same_branch() {
        // NakedTriple: Expert, Subset branch, diff 50.
        let s = Spec::drill(TechniqueKind::NakedTriple);
        // Easier tiers allowed in full.
        assert!(matches!(
            s.usage(TechniqueKind::HiddenSingle),
            Some(Usage::Allowed)
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedSingle),
            Some(Usage::Allowed)
        ));
        // Simpler same-branch (subset) peers conceded.
        assert!(matches!(
            s.usage(TechniqueKind::NakedPair),
            Some(Usage::Conceded)
        ));
        assert!(matches!(
            s.usage(TechniqueKind::HiddenPair),
            Some(Usage::Conceded)
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedTriple),
            Some(Usage::Forced { .. })
        ));
        // Other-branch (X-Wing) and harder same-branch (Naked Quad) out of scope.
        assert!(!s.is_in_scope(TechniqueKind::XWing));
        assert!(!s.is_in_scope(TechniqueKind::NakedQuad));
    }

    #[test]
    fn drill_intermediate_concedes_other_intermediate() {
        // Intermediate is flat: drill concedes every other Intermediate technique.
        let s = Spec::drill(TechniqueKind::LockedCandidatesClaiming);
        assert!(matches!(
            s.usage(TechniqueKind::HiddenSingle),
            Some(Usage::Allowed)
        )); // Beginner allowed
        assert!(matches!(
            s.usage(TechniqueKind::NakedSingle),
            Some(Usage::Conceded)
        ));
        assert!(matches!(
            s.usage(TechniqueKind::LockedCandidatesPointing),
            Some(Usage::Conceded)
        ));
        assert!(matches!(
            s.usage(TechniqueKind::LockedCandidatesClaiming),
            Some(Usage::Forced { .. })
        ));
        assert!(!s.is_in_scope(TechniqueKind::NakedPair)); // Expert, out of scope
    }

    #[test]
    fn drill_expert_isolates_other_branches() {
        // Swordfish: Expert, single-digit branch, diff 56.
        let s = Spec::drill(TechniqueKind::Swordfish);
        // Easier tiers allowed in full.
        for allowed in [
            TechniqueKind::HiddenSingle,
            TechniqueKind::NakedSingle,
            TechniqueKind::LockedCandidatesPointing,
            TechniqueKind::LockedCandidatesClaiming,
        ] {
            assert!(
                matches!(s.usage(allowed), Some(Usage::Allowed)),
                "{:?} should be Allowed, got {:?}",
                allowed,
                s.usage(allowed),
            );
        }
        // Only the simpler same-branch fish is conceded.
        assert!(matches!(s.usage(TechniqueKind::XWing), Some(Usage::Conceded)));
        assert!(matches!(
            s.usage(TechniqueKind::Swordfish),
            Some(Usage::Forced { .. })
        ));
        // Other-branch Expert peers and harder same-branch fish are out of scope.
        for out in [
            TechniqueKind::NakedPair,
            TechniqueKind::HiddenPair,
            TechniqueKind::NakedTriple,
            TechniqueKind::XYWing,
            TechniqueKind::WWing,
            TechniqueKind::Jellyfish,
        ] {
            assert!(
                !s.is_in_scope(out),
                "{:?} should be out of scope under drill(Swordfish)",
                out,
            );
        }
    }
}
