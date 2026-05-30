use std::num::NonZeroUsize;

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

    pub fn allow_up_to(t: TechniqueKind) -> Self {
        let cap = t.difficulty();
        let mut spec = Self::default();
        for def in REGISTRY {
            if def.difficulty <= cap {
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

    /// Broad-mode training: allow every technique up to and including `t`,
    /// and force `t` to appear at least once. Realistic mid-solve deadlock.
    pub fn train(t: TechniqueKind) -> Self {
        Self::allow_up_to(t).require(t, 1)
    }

    /// Drill-mode training: a small baseline plus the target, with every
    /// technique strictly between the baseline ceiling and `t` (by difficulty)
    /// conceded — the puzzle must remain unsolvable even when the avoid-target
    /// solver has the whole in-between toolbox.
    ///
    /// The baseline ceiling scales with `t`'s difficulty, on the reasoning that
    /// a solver learning a hard target already knows the easy propagation
    /// techniques reflexively:
    /// - `t` diff < 50: ceiling is HiddenSingle (singles only).
    /// - `t` diff in 50..75: ceiling is LockedCandidatesClaiming (singles + LC).
    /// - `t` diff ≥ 75: ceiling is NakedPair (singles + LC + naked pair).
    ///
    /// Generation is harder than `train` (and may still fail for some
    /// techniques) — fall back to `train` if needed.
    pub fn drill(t: TechniqueKind) -> Self {
        let ceiling = Self::drill_baseline_ceiling(t);
        let low = ceiling.difficulty();
        let high = t.difficulty();
        let mut spec = Self::allow_up_to(ceiling).require(t, 1);
        for def in REGISTRY {
            if def.difficulty > low && def.difficulty < high {
                spec = spec.concede(def.kind);
            }
        }
        spec
    }

    fn drill_baseline_ceiling(t: TechniqueKind) -> TechniqueKind {
        let d = t.difficulty();
        if d >= 75 {
            TechniqueKind::NakedPair
        } else if d >= 50 {
            TechniqueKind::LockedCandidatesClaiming
        } else {
            TechniqueKind::HiddenSingle
        }
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
    fn train_is_broad() {
        let s = Spec::train(TechniqueKind::Swordfish);
        // Includes all techniques up to and including Swordfish.
        assert!(s.is_in_scope(TechniqueKind::NakedSingle));
        assert!(s.is_in_scope(TechniqueKind::XWing));
        assert!(s.is_in_scope(TechniqueKind::Swordfish));
        // Excludes harder ones.
        assert!(!s.is_in_scope(TechniqueKind::Jellyfish));
        // And Swordfish is forced.
        assert!(matches!(
            s.usage(TechniqueKind::Swordfish),
            Some(Usage::Forced { .. })
        ));
    }

    #[test]
    fn drill_easy_target_keeps_singles_only_baseline() {
        // Target diff < 50 → baseline ceiling stays at HiddenSingle.
        let s = Spec::drill(TechniqueKind::NakedTriple);
        assert!(matches!(
            s.usage(TechniqueKind::HiddenSingle),
            Some(Usage::Allowed),
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedPair),
            Some(Usage::Conceded),
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedTriple),
            Some(Usage::Forced { .. }),
        ));
    }

    #[test]
    fn drill_mid_target_lifts_baseline_to_locked_candidates() {
        // Target diff in 50..75 → baseline includes LC, subsets still Conceded.
        let s = Spec::drill(TechniqueKind::XYWing);
        assert!(matches!(
            s.usage(TechniqueKind::LockedCandidatesPointing),
            Some(Usage::Allowed),
        ));
        assert!(matches!(
            s.usage(TechniqueKind::LockedCandidatesClaiming),
            Some(Usage::Allowed),
        ));
        assert!(matches!(
            s.usage(TechniqueKind::NakedPair),
            Some(Usage::Conceded),
        ));
        assert!(matches!(
            s.usage(TechniqueKind::XYWing),
            Some(Usage::Forced { .. }),
        ));
    }

    #[test]
    fn drill_hard_target_lifts_baseline_through_naked_pair() {
        let s = Spec::drill(TechniqueKind::Swordfish);
        // Baseline now reaches NakedPair for diff-75+ targets.
        for in_baseline in [
            TechniqueKind::NakedSingle,
            TechniqueKind::HiddenSingle,
            TechniqueKind::LockedCandidatesPointing,
            TechniqueKind::LockedCandidatesClaiming,
            TechniqueKind::NakedPair,
        ] {
            assert!(
                matches!(s.usage(in_baseline), Some(Usage::Allowed)),
                "{:?} should be Allowed in drill(Swordfish), got {:?}",
                in_baseline,
                s.usage(in_baseline),
            );
        }
        assert!(matches!(
            s.usage(TechniqueKind::Swordfish),
            Some(Usage::Forced { .. }),
        ));
        // Everything strictly between NakedPair and Swordfish stays Conceded.
        for in_between in [
            TechniqueKind::HiddenPair,
            TechniqueKind::NakedTriple,
            TechniqueKind::HiddenTriple,
            TechniqueKind::NakedQuad,
            TechniqueKind::HiddenQuad,
            TechniqueKind::XWing,
            TechniqueKind::FinnedXWing,
            TechniqueKind::XYWing,
            TechniqueKind::WWing,
        ] {
            assert!(
                matches!(s.usage(in_between), Some(Usage::Conceded)),
                "{:?} should be Conceded under drill(Swordfish), got {:?}",
                in_between,
                s.usage(in_between),
            );
        }
        assert!(!s.is_in_scope(TechniqueKind::Jellyfish));
    }
}
