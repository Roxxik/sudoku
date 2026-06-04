//! Minimal generation spec, faithful to core's `spec::Spec` for the up-to-
//! HiddenQuad ladder. Compact on purpose — this is the actual spec, not the
//! play-time bloat: a per-kind [`Usage`] array plus the `train`/`drill`
//! builders, and the three masks the generator/verify gates read.
//!
//! `require_any` (family-level "any fish" constraints) is intentionally omitted:
//! no spec in PoC scope uses it (train/drill(HiddenQuad) only Force a single
//! kind). Add it when scope grows past HiddenSubset.

use crate::techniques::{DIFFICULTY, HIDDEN_SINGLE, KindMask, NUM};

/// How a technique participates in a spec — mirrors core's `Usage`.
///
/// - `Allowed`/`Forced` form the **baseline** (the positive solvability set).
/// - `Forced{count}` additionally requires `count` firings in the baseline trace.
/// - `Conceded` is granted ONLY to verify's avoid-target walk: a hypothetical
///   solver may use it when proving a Forced technique unavoidable, but the
///   puzzle is not promised solvable with it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Allowed,
    Forced(u16),
    Conceded,
}

/// Per-technique configuration, dense array indexed by kind.
#[derive(Clone)]
pub struct Spec {
    usage: [Option<Usage>; NUM],
}

impl Spec {
    fn empty() -> Self {
        Spec { usage: [None; NUM] }
    }

    /// Start an explicit, empty spec and layer kinds with [`Spec::allow`] /
    /// [`Spec::force`] / [`Spec::concede`]. The generator must accept every
    /// label combination, not just `train`/`drill`; this is how callers (and the
    /// equivalence tests) build arbitrary specs to exercise the baseline gate.
    pub fn explicit() -> Self {
        Self::empty()
    }

    /// Mark kind `idx` `Allowed` (part of the baseline toolbox).
    pub fn allow(mut self, idx: usize) -> Self {
        self.usage[idx] = Some(Usage::Allowed);
        self
    }

    /// Mark kind `idx` `Forced` with the given required firing `count`.
    pub fn force(mut self, idx: usize, count: u16) -> Self {
        self.usage[idx] = Some(Usage::Forced(count));
        self
    }

    /// Mark kind `idx` `Conceded` (in scope for verify's avoid-walk, but not part
    /// of the baseline solvability toolbox).
    pub fn concede(mut self, idx: usize) -> Self {
        self.usage[idx] = Some(Usage::Conceded);
        self
    }

    /// Allow every kind with difficulty <= `target`'s difficulty.
    fn allow_up_to(target: usize) -> Self {
        let cap = DIFFICULTY[target];
        let mut s = Self::empty();
        for idx in 0..NUM {
            if DIFFICULTY[idx] <= cap {
                s.usage[idx] = Some(Usage::Allowed);
            }
        }
        s
    }

    /// Broad-mode training: allow everything up to and including `target`, force
    /// `target` to appear at least once. `Spec::train` in core.
    pub fn train(target: usize) -> Self {
        Self::allow_up_to(target).force(target, 1)
    }

    /// Drill-mode: a small baseline plus the target, with every kind strictly
    /// between the baseline ceiling and `target` (by difficulty) conceded — the
    /// puzzle must remain unsolvable without `target` even when the avoid-target
    /// solver has the whole in-between toolbox. `Spec::drill` in core.
    ///
    /// PoC ceiling: HiddenQuad's difficulty (45) is < 50, so the baseline
    /// ceiling is HiddenSingle (singles only). (Core scales the ceiling up for
    /// harder targets; out of PoC scope.)
    pub fn drill(target: usize) -> Self {
        let ceiling = HIDDEN_SINGLE; // valid for any target with difficulty < 50
        let low = DIFFICULTY[ceiling];
        let high = DIFFICULTY[target];
        let mut s = Self::allow_up_to(ceiling).force(target, 1);
        for idx in 0..NUM {
            if DIFFICULTY[idx] > low && DIFFICULTY[idx] < high {
                s = s.concede(idx);
            }
        }
        s
    }

    /// Baseline toolbox (Allowed | Forced): the strip-loop solvability gate and
    /// verify's positive check.
    pub fn baseline_mask(&self) -> KindMask {
        let mut m = 0;
        for idx in 0..NUM {
            if matches!(self.usage[idx], Some(Usage::Allowed) | Some(Usage::Forced(_))) {
                m |= 1 << idx;
            }
        }
        m
    }

    /// Everything in scope (Allowed | Forced | Conceded): verify's avoid-target
    /// walk toolbox.
    pub fn in_scope_mask(&self) -> KindMask {
        let mut m = 0;
        for idx in 0..NUM {
            if self.usage[idx].is_some() {
                m |= 1 << idx;
            }
        }
        m
    }

    /// Membership bitmask of the Forced kinds — `1 << idx` set iff kind `idx`
    /// is `Forced`. The baseline engine reads this to choose a strategy: it must
    /// count every Forced kind exactly (the requirement check reads those counts),
    /// so a Forced kind can never be folded into the batched fast-path closure.
    pub fn forced_mask(&self) -> KindMask {
        let mut m = 0;
        for idx in 0..NUM {
            if matches!(self.usage[idx], Some(Usage::Forced(_))) {
                m |= 1 << idx;
            }
        }
        m
    }

    /// `(kind_index, required_count)` for each Forced kind.
    pub fn forced(&self) -> impl Iterator<Item = (usize, u16)> + '_ {
        (0..NUM).filter_map(move |idx| match self.usage[idx] {
            Some(Usage::Forced(n)) => Some((idx, n)),
            _ => None,
        })
    }

    /// True iff a baseline trace with these per-kind `counts` meets every Forced
    /// requirement — core's `requirement_met` (PoC: no `require_any`).
    pub fn requirement_met(&self, counts: &[u16; NUM]) -> bool {
        self.forced().all(|(idx, need)| counts[idx] >= need)
    }
}
