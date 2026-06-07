//! Minimal generation spec, faithful to core's `spec::Spec` for the up-to-
//! HiddenQuad ladder. Compact on purpose — this is the actual spec, not the
//! play-time bloat: a per-kind [`Usage`] array plus the `train`/`drill`
//! builders, and the three masks the generator/verify gates read.
//!
//! `require_any` (family-level "any fish" constraints) is intentionally omitted:
//! no spec in PoC scope uses it (train/drill(HiddenQuad) only Force a single
//! kind). Add it when scope grows past HiddenSubset.
//!
//! [`kinds`] holds the shared technique taxonomy this spec is built over — the kind
//! indices, the [`KindMask`](kinds::KindMask) set type, and the
//! [`SolveTrace`](kinds::SolveTrace) the solve engines return.

pub mod kinds;

use kinds::{DIFFICULTY, KindMask, NUM, Tier, branch_of, tier_of};

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

    /// Broad-mode training for `target`: force it, and allow what the player may
    /// lean on to reach it. The whole Trunk (Beginner + Intermediate) is allowed
    /// unconditionally; within `target`'s own branch, the simpler-or-equal Expert
    /// techniques are allowed too. **Branch-scoped** — training a fish never enables
    /// subsets, and vice versa. Mirrors core's `Spec::train` (see `CURRICULUM.md`).
    pub fn train(target: usize) -> Self {
        let target_tier = tier_of(target);
        let target_branch = branch_of(target);
        let target_diff = DIFFICULTY[target];
        let mut s = Self::empty();
        for idx in 0..NUM {
            let tt = tier_of(idx);
            if tt > target_tier {
                continue;
            }
            let allowed = if tt <= Tier::Intermediate {
                // Trunk: always available, up to the target's tier.
                true
            } else {
                // Branch technique: same branch as the target, simpler-or-equal.
                branch_of(idx) == target_branch && DIFFICULTY[idx] <= target_diff
            };
            if allowed {
                s.usage[idx] = Some(Usage::Allowed);
            }
        }
        s.force(target, 1)
    }

    /// Drill-mode for `target`: force it, allow every *easier tier* in full, and
    /// **concede** its in-tier peers — the rest of the flat Intermediate tier, or
    /// the simpler same-branch techniques in Expert. Conceded techniques may fire
    /// but must not substitute for the target, so the drill isolates `target`
    /// against its immediate neighbours. Harder-or-other-branch peers are out of
    /// scope entirely. Mirrors core's `Spec::drill` (see `CURRICULUM.md`).
    ///
    /// Because the concede set is branch-scoped, drilling a fish concedes only the
    /// simpler fish (not subsets), and drilling a subset concedes only the simpler
    /// subsets (not fish) — even though X-Wing's difficulty interleaves between
    /// Naked Pair and Hidden Pair.
    pub fn drill(target: usize) -> Self {
        let target_tier = tier_of(target);
        let target_branch = branch_of(target);
        let target_diff = DIFFICULTY[target];
        let mut s = Self::empty();
        for idx in 0..NUM {
            if idx == target {
                continue;
            }
            let tt = tier_of(idx);
            if tt < target_tier {
                // Easier tiers are allowed in full.
                s.usage[idx] = Some(Usage::Allowed);
            } else if tt == target_tier {
                let concede = match target_tier {
                    // Beginner is train-only; nothing to concede.
                    Tier::Beginner => false,
                    // Flat tier: concede every other Intermediate technique.
                    Tier::Intermediate => true,
                    // Branch ladder: concede the simpler same-branch peers.
                    Tier::Expert | Tier::Master => {
                        branch_of(idx) == target_branch && DIFFICULTY[idx] < target_diff
                    }
                };
                if concede {
                    s.usage[idx] = Some(Usage::Conceded);
                }
            }
            // tt > target_tier: out of scope.
        }
        s.force(target, 1)
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
