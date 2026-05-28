use std::collections::HashMap;
use std::num::NonZeroUsize;

use crate::techniques::{REGISTRY, TechniqueKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    UseFreely,
    Forced { count: NonZeroUsize },
}

impl Usage {
    pub fn forced(n: usize) -> Self {
        Self::Forced {
            count: NonZeroUsize::new(n).expect("Forced count must be at least 1"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub usages: HashMap<TechniqueKind, Usage>,
}

impl Spec {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn allow_all() -> Self {
        let mut usages = HashMap::new();
        for def in REGISTRY {
            usages.insert(def.kind, Usage::UseFreely);
        }
        Self { usages }
    }

    pub fn allow_up_to(t: TechniqueKind) -> Self {
        let cap = t.difficulty();
        let mut usages = HashMap::new();
        for def in REGISTRY {
            if def.difficulty <= cap {
                usages.insert(def.kind, Usage::UseFreely);
            }
        }
        Self { usages }
    }

    pub fn allow(mut self, t: TechniqueKind) -> Self {
        self.usages.insert(t, Usage::UseFreely);
        self
    }

    pub fn require(mut self, t: TechniqueKind, count: usize) -> Self {
        self.usages.insert(t, Usage::forced(count));
        self
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
            Some(Usage::UseFreely)
        ));
    }

    #[test]
    fn empty_spec_lists_nothing() {
        let s = Spec::empty();
        assert!(s.usages.is_empty());
    }

    #[test]
    #[should_panic]
    fn forced_zero_panics() {
        let _ = Usage::forced(0);
    }
}
