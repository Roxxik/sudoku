use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

use crate::techniques::{Family, REGISTRY, TechniqueKind};

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
    pub kinds: HashSet<TechniqueKind>,
    pub count: NonZeroUsize,
}

#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub usages: HashMap<TechniqueKind, Usage>,
    pub require_any: Vec<RequireAny>,
}

impl Spec {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn allow_all() -> Self {
        let mut usages = HashMap::new();
        for def in REGISTRY {
            usages.insert(def.kind, Usage::Allowed);
        }
        Self {
            usages,
            require_any: Vec::new(),
        }
    }

    pub fn allow_up_to(t: TechniqueKind) -> Self {
        let cap = t.difficulty();
        let mut usages = HashMap::new();
        for def in REGISTRY {
            if def.difficulty <= cap {
                usages.insert(def.kind, Usage::Allowed);
            }
        }
        Self {
            usages,
            require_any: Vec::new(),
        }
    }

    pub fn allow(mut self, t: TechniqueKind) -> Self {
        self.usages.insert(t, Usage::Allowed);
        self
    }

    pub fn allow_family(mut self, family: Family) -> Self {
        for t in family.members() {
            self.usages.entry(t).or_insert(Usage::Allowed);
        }
        self
    }

    pub fn require(mut self, t: TechniqueKind, count: usize) -> Self {
        self.usages.insert(t, Usage::forced(count));
        self
    }

    /// Concede `t` to the irreplaceability check: it may fire freely but is
    /// not part of the solvability baseline, and the Forced check assumes a
    /// hypothetical solver has access to it when proving the Forced
    /// techniques are unavoidable.
    pub fn concede(mut self, t: TechniqueKind) -> Self {
        self.usages.insert(t, Usage::Conceded);
        self
    }

    /// Require at least `count` total uses drawn from any of the given techniques.
    /// Members not already present in `usages` are added as `Allowed` — the
    /// solver must be able to use them at all in order to satisfy the constraint.
    pub fn require_one_of<I>(mut self, kinds: I, count: usize) -> Self
    where
        I: IntoIterator<Item = TechniqueKind>,
    {
        let set: HashSet<TechniqueKind> = kinds.into_iter().collect();
        assert!(!set.is_empty(), "require_one_of needs at least one technique");
        for &k in &set {
            self.usages.entry(k).or_insert(Usage::Allowed);
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

    /// Drill-mode training: only singles plus the target form the baseline,
    /// but every technique strictly between [`TechniqueKind::HiddenSingle`] and
    /// `t` (by difficulty) is conceded — the puzzle must remain unsolvable
    /// even when the avoid-target solver has the whole in-between toolbox.
    /// Generation is much harder than `train` (and may fail for some
    /// techniques) — fall back to `train` if needed.
    pub fn drill(t: TechniqueKind) -> Self {
        let low = TechniqueKind::HiddenSingle.difficulty();
        let high = t.difficulty();
        let mut spec = Self::allow_up_to(TechniqueKind::HiddenSingle).require(t, 1);
        for def in REGISTRY {
            if def.difficulty > low && def.difficulty < high {
                spec = spec.concede(def.kind);
            }
        }
        spec
    }

    /// True if `t` is in the baseline (Allowed or Forced) — i.e., counts as
    /// "the solver actually relies on this" for the positive solvability
    /// check and for the displayed solution trace.
    pub fn is_baseline(&self, t: TechniqueKind) -> bool {
        matches!(
            self.usages.get(&t),
            Some(Usage::Allowed) | Some(Usage::Forced { .. }),
        )
    }

    /// True if `t` appears in the spec under any variant.
    pub fn is_in_scope(&self, t: TechniqueKind) -> bool {
        self.usages.contains_key(&t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_up_to_includes_only_easier_or_equal() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenSingle);
        assert!(s.usages.contains_key(&TechniqueKind::NakedSingle));
        assert!(s.usages.contains_key(&TechniqueKind::HiddenSingle));
        assert!(!s.usages.contains_key(&TechniqueKind::NakedPair));
        assert!(!s.usages.contains_key(&TechniqueKind::HiddenTriple));
    }

    #[test]
    fn builder_chains() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenPair)
            .require(TechniqueKind::HiddenPair, 2);
        assert!(matches!(
            s.usages.get(&TechniqueKind::HiddenPair),
            Some(Usage::Forced { .. })
        ));
        assert!(matches!(
            s.usages.get(&TechniqueKind::NakedSingle),
            Some(Usage::Allowed)
        ));
    }

    #[test]
    fn empty_spec_lists_nothing() {
        let s = Spec::empty();
        assert!(s.usages.is_empty());
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
        assert!(s.usages.contains_key(&TechniqueKind::XYWing));
        assert!(s.usages.contains_key(&TechniqueKind::XYZWing));
        assert!(s.usages.contains_key(&TechniqueKind::WWing));
        // Non-Wing techniques should not be added.
        assert!(!s.usages.contains_key(&TechniqueKind::Swordfish));
    }

    #[test]
    fn allow_family_does_not_clobber_forced() {
        let s = Spec::empty()
            .require(TechniqueKind::XYWing, 1)
            .allow_family(Family::Wing);
        // Forced status must be preserved (entry not overwritten by Allowed).
        assert!(matches!(
            s.usages.get(&TechniqueKind::XYWing),
            Some(Usage::Forced { .. })
        ));
    }

    #[test]
    fn require_family_creates_require_any() {
        let s = Spec::allow_up_to(TechniqueKind::HiddenQuad).require_family(Family::Fish, 1);
        assert_eq!(s.require_any.len(), 1);
        let r = &s.require_any[0];
        assert!(r.kinds.contains(&TechniqueKind::XWing));
        assert!(r.kinds.contains(&TechniqueKind::Swordfish));
        assert!(r.kinds.contains(&TechniqueKind::Jellyfish));
        assert_eq!(r.count.get(), 1);
        // Fishes that weren't in allow_up_to(HiddenQuad) are now allowed.
        assert!(s.usages.contains_key(&TechniqueKind::Swordfish));
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
        assert!(s.usages.contains_key(&TechniqueKind::NakedSingle));
        assert!(s.usages.contains_key(&TechniqueKind::XWing));
        assert!(s.usages.contains_key(&TechniqueKind::Swordfish));
        // Excludes harder ones.
        assert!(!s.usages.contains_key(&TechniqueKind::Jellyfish));
        // And Swordfish is forced.
        assert!(matches!(
            s.usages.get(&TechniqueKind::Swordfish),
            Some(Usage::Forced { .. })
        ));
    }

    #[test]
    fn drill_baseline_is_singles_plus_target_in_betweens_conceded() {
        let s = Spec::drill(TechniqueKind::Swordfish);
        // Baseline: singles and the target.
        assert!(matches!(
            s.usages.get(&TechniqueKind::NakedSingle),
            Some(Usage::Allowed),
        ));
        assert!(matches!(
            s.usages.get(&TechniqueKind::HiddenSingle),
            Some(Usage::Allowed),
        ));
        assert!(matches!(
            s.usages.get(&TechniqueKind::Swordfish),
            Some(Usage::Forced { .. }),
        ));
        // Everything strictly between HiddenSingle and Swordfish is conceded.
        for in_between in [
            TechniqueKind::NakedPair,
            TechniqueKind::HiddenPair,
            TechniqueKind::XWing,
            TechniqueKind::FinnedXWing,
            TechniqueKind::XYWing,
            TechniqueKind::WWing,
        ] {
            assert!(
                matches!(s.usages.get(&in_between), Some(Usage::Conceded)),
                "{:?} should be Conceded under drill(Swordfish), got {:?}",
                in_between,
                s.usages.get(&in_between),
            );
        }
        // Techniques harder than the target are still out of scope.
        assert!(!s.usages.contains_key(&TechniqueKind::Jellyfish));
    }
}
