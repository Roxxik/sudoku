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

    // ---- Cross-branch isolated builders (the future canonical train/drill) ----
    //
    // `train_isolated`/`drill_isolated` are the in-progress replacements for
    // `train`/`drill`. They differ in ONE way, and only for Expert targets: the
    // concede/isolation set is scoped by player-facing DIFFICULTY across every
    // branch, not by branch. So an X-Wing target (difficulty 38) now isolates
    // against Naked Pair (difficulty 32, the Subset branch) — which the old
    // branch-scoped builders leave out of scope entirely, letting an X-Wing puzzle
    // be solved by a Naked Pair instead. That is a bad teaching puzzle: the player
    // spots the easier technique and never needs the X-Wing. Putting the easier
    // peer in scope (Conceded) forces the target to be REQUIRED even when that peer
    // is available, so such puzzles are rejected.
    //
    // Only EASIER peers are conceded (`DIFFICULTY < target_diff`): a player at the
    // target's level does not know the harder techniques, so a harder substitute is
    // not a teaching problem and conceding it would only cost yield.
    //
    // The legacy `train`/`drill` above are deliberately UNCHANGED — a separate
    // debugging effort relies on their exact branch-scoped semantics — and for now
    // only the frontend wasm bridge calls these `_isolated` variants. When these
    // become canonical (and the legacy pair is retired), MIRROR the same change
    // into core's parallel copy `Spec::train`/`Spec::drill` in `core/src/spec.rs`,
    // which the CLI and core's curriculum/play machinery use.

    /// Cross-branch [`train`](Self::train) (future canonical). Identical to `train`,
    /// but additionally **concedes** every Expert peer *easier* than the target,
    /// regardless of branch — so a training puzzle cannot be sidestepped by an
    /// easier other-branch technique (e.g. solving a train X-Wing with a Naked
    /// Pair). The lean-on baseline (Trunk + simpler-or-equal same-branch Expert) is
    /// unchanged; the cross-branch easier peers are added as `Conceded`. See the
    /// section comment above for the rationale and the core-sync note.
    pub fn train_isolated(target: usize) -> Self {
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
            } else if tt == target_tier && DIFFICULTY[idx] < target_diff {
                // Easier same-tier (Expert) peer from another branch: not a
                // lean-on, but concede it so the target stays required against it.
                s.usage[idx] = Some(Usage::Conceded);
            }
        }
        s.force(target, 1)
    }

    /// Cross-branch [`drill`](Self::drill) (future canonical). Identical to `drill`,
    /// but in the Expert/Master ladder it **concedes** every simpler-difficulty
    /// peer regardless of branch (the legacy builder concedes only the simpler
    /// *same-branch* peers). So drilling X-Wing concedes Naked Pair, isolating the
    /// drill against every easier technique a player at that level might reach for —
    /// not just its own branch. Beginner/Intermediate behaviour is unchanged
    /// (Intermediate is already a flat, concede-every-peer tier). See the section
    /// comment above for the rationale and the core-sync note.
    pub fn drill_isolated(target: usize) -> Self {
        let target_tier = tier_of(target);
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
                    // Expert/Master ladder, isolated: concede every simpler-
                    // difficulty peer, in ANY branch (not just the target's).
                    Tier::Expert | Tier::Master => DIFFICULTY[idx] < target_diff,
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

#[cfg(test)]
mod tests {
    use super::kinds::{
        HIDDEN_PAIR, HIDDEN_SINGLE, JELLYFISH, LC_CLAIMING, NAKED_PAIR, NAKED_TRIPLE, SWORDFISH,
        X_WING, XY_WING,
    };
    use super::*;

    fn conceded(s: &Spec, idx: usize) -> bool {
        let in_scope = (s.in_scope_mask() >> idx) & 1 == 1;
        let baseline = (s.baseline_mask() >> idx) & 1 == 1;
        in_scope && !baseline
    }
    fn in_baseline(s: &Spec, idx: usize) -> bool {
        (s.baseline_mask() >> idx) & 1 == 1
    }
    fn in_scope(s: &Spec, idx: usize) -> bool {
        (s.in_scope_mask() >> idx) & 1 == 1
    }

    /// The bug the isolated builders fix: an X-Wing drill must isolate against the
    /// easier cross-branch peer (Naked Pair) so it cannot be solved by it instead.
    #[test]
    fn drill_isolated_concedes_easier_cross_branch_peer() {
        let s = Spec::drill_isolated(X_WING);
        assert!(in_baseline(&s, X_WING), "X-Wing is forced into the baseline");
        assert!(conceded(&s, NAKED_PAIR), "X-Wing drill must concede Naked Pair");
        // Harder Expert peers stay out of scope — a player at X-Wing level does not
        // know them, so they are not substitutes to guard against.
        assert!(!in_scope(&s, SWORDFISH), "harder fish stays out of scope");
        assert!(!in_scope(&s, JELLYFISH), "harder fish stays out of scope");
        assert!(!in_scope(&s, HIDDEN_PAIR), "harder subset stays out of scope");
        assert!(!in_scope(&s, XY_WING), "harder wing stays out of scope");
    }

    /// The legacy `drill` is deliberately UNCHANGED (a separate debugging effort
    /// depends on its branch-scoped semantics): it still leaves Naked Pair out of
    /// scope for an X-Wing drill — the exact bad-puzzle case the isolated builder
    /// fixes.
    #[test]
    fn legacy_drill_keeps_branch_scoped_semantics() {
        let s = Spec::drill(X_WING);
        assert!(!in_scope(&s, NAKED_PAIR), "legacy drill must NOT concede Naked Pair");
    }

    /// `train` has no concede mechanism; `train_isolated` adds the easier
    /// cross-branch peers as concedes while keeping the lean-on baseline. The
    /// legacy `train` leaves Naked Pair out of scope, same blind spot as `drill`.
    #[test]
    fn train_isolated_concedes_easier_cross_branch_peer() {
        let iso = Spec::train_isolated(X_WING);
        assert!(in_baseline(&iso, X_WING));
        assert!(conceded(&iso, NAKED_PAIR), "isolated train must concede Naked Pair");
        assert!(!in_scope(&Spec::train(X_WING), NAKED_PAIR), "legacy train leaves it out");
    }

    /// For a deeper Expert target the isolated drill pulls in the easier OTHER-
    /// branch peer (X-Wing for a Naked Triple drill) the branch-scoped drill omits,
    /// while the same-branch easier peers stay conceded in both.
    #[test]
    fn drill_isolated_expert_subset_target_concedes_cross_branch() {
        let iso = Spec::drill_isolated(NAKED_TRIPLE);
        assert!(conceded(&iso, X_WING), "easier cross-branch fish now conceded");
        assert!(!in_scope(&Spec::drill(NAKED_TRIPLE), X_WING), "legacy drill omits it");
        assert!(conceded(&iso, NAKED_PAIR), "same-branch easier peer still conceded");
        assert!(conceded(&iso, HIDDEN_PAIR), "same-branch easier peer still conceded");
    }

    /// Below Expert the isolated builders reduce to the legacy ones: no Expert peer
    /// is easier than an Intermediate target, and the Intermediate concede tier is
    /// already flat, so the masks are bit-identical.
    #[test]
    fn isolated_reduces_to_legacy_below_expert() {
        for t in [HIDDEN_SINGLE, LC_CLAIMING] {
            let (di, dl) = (Spec::drill_isolated(t), Spec::drill(t));
            assert_eq!(di.in_scope_mask(), dl.in_scope_mask(), "drill in_scope [{t}]");
            assert_eq!(di.baseline_mask(), dl.baseline_mask(), "drill baseline [{t}]");
            assert_eq!(di.forced_mask(), dl.forced_mask(), "drill forced [{t}]");
            let (ti, tl) = (Spec::train_isolated(t), Spec::train(t));
            assert_eq!(ti.in_scope_mask(), tl.in_scope_mask(), "train in_scope [{t}]");
            assert_eq!(ti.baseline_mask(), tl.baseline_mask(), "train baseline [{t}]");
            assert_eq!(ti.forced_mask(), tl.forced_mask(), "train forced [{t}]");
        }
    }
}
