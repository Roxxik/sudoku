# Curriculum

This document defines the **player-facing** curriculum: the tiers, branches, and
techniques we show to the player and use for coaching/training. It assigns a
player-facing **difficulty score** to each technique.

This says nothing about *solver* difficulty. The solver is free to reorder the
technique ladder however it likes for efficiency; the order here is purely
pedagogical and independent of the numeric difficulties the solver uses
internally.

Technique names match `TechniqueKind` in `core/src/techniques.rs` so the
curriculum maps cleanly onto code. Not every technique named here is implemented
yet (marked *planned*), and core implements more techniques than appear here
(e.g. turbot and finned fish) — those slot into the branches below when we
surface them.

## Scoring

Scores are uncapped integers, informed by (but not copied from) SudokuExplainer
(SE). Beginner→Expert runs roughly 1–100; Master is 100+. Within a branch scores
increase monotonically; across branches they interleave (see the linear view).

Two ordering subtleties worth knowing:

- **The naked/hidden order flips between singles and subsets.** For *singles*,
  hidden is *easier* than naked (you scan a unit for a digit's only spot; naked
  needs full pencil marks first — SE rates hidden single 1.2–1.5, naked single
  2.3). For *subsets*, naked is *easier* than hidden (you see N cells holding N
  candidates directly; hidden requires inferring the complement — SE: Naked Pair
  3.0 < Hidden Pair 3.4, and so on).
- **The tree view is not globally difficulty-sorted; only the linear view is.**
  Tier cuts fall on SE thresholds, so the *tiers* don't invert spotting-difficulty
  (every Intermediate technique is below every Expert one). But the three Expert
  *branches* interleave: training down one branch skips past lower-scored
  techniques in the others — e.g. the Fish branch jumps X-Wing (38) straight to
  Swordfish (56), past Hidden Pair (44) and Naked Triple (50) in the Subsets
  branch. Within a branch you climb a concept; across branches, score order only
  shows up in the linear/freeplay view.

## Tiers

We follow SE's difficulty thresholds for the tier cut points.

| Tier             | SE band      | Scope                                                        |
|------------------|--------------|-------------------------------------------------------------|
| **Beginner**     | ~1.0–1.5     | Hidden singles only.                                         |
| **Intermediate** | < 3.0        | Naked single, locked candidates.                            |
| **Expert**       | 3.0 – ~5.4   | Three branches: single-digit (fish), bivalue chains, subsets. |
| **Master**       | above ~5.4   | Advanced set logic. Mostly empty for now; ALS lands here, and the Phistomefel ring. |

## Two views

- **Tree / graph view** — the player trains down a single branch: each technique
  unlocks from its simpler same-branch predecessors. This is the structured
  coaching path.
- **Linear / freeplay view** — all techniques laid out by difficulty score,
  interleaving the branches. This is the freeplay/mixing mode that lets the
  player move between branches by difficulty rather than by branch.

## Techniques

### Beginner

| Score | Technique     | `TechniqueKind` |
|-------|---------------|-----------------|
| 5     | Hidden Single | `HiddenSingle`  |

### Intermediate

| Score | Technique                    | `TechniqueKind`            |
|-------|------------------------------|----------------------------|
| 15    | Naked Single                 | `NakedSingle`              |
| 22    | Locked Candidates (Pointing) | `LockedCandidatesPointing` |
| 26    | Locked Candidates (Claiming) | `LockedCandidatesClaiming` |

### Expert

Three independent branches. The tree view trains within a branch; the linear
view interleaves them by score.

**Branch A — Single-digit (Fish).** One digit constrained across rows/columns.

| Score | Technique  | `TechniqueKind` |
|-------|------------|-----------------|
| 38    | X-Wing     | `XWing`         |
| 56    | Swordfish  | `Swordfish`     |
| 86    | Jellyfish  | `Jellyfish`     |

**Branch B — Bivalue chains.** Reasoning over bivalue cells and links.

| Score | Technique  | `TechniqueKind` | Status   |
|-------|------------|-----------------|----------|
| 68    | XY-Wing    | `XYWing`        |          |
| 72    | XYZ-Wing   | `XYZWing`       |          |
| 76    | W-Wing     | `WWing`         |          |
| 88    | XY-Chain   | —               | planned  |

**Branch C — Subsets.** Naked/hidden subsets of size 2–4. Naked before hidden at
each size.

| Score | Technique    | `TechniqueKind` |
|-------|--------------|-----------------|
| 32    | Naked Pair   | `NakedPair`     |
| 44    | Hidden Pair  | `HiddenPair`    |
| 50    | Naked Triple | `NakedTriple`   |
| 62    | Hidden Triple| `HiddenTriple`  |
| 82    | Naked Quad   | `NakedQuad`     |
| 92    | Hidden Quad  | `HiddenQuad`    |

### Master (100+)

| Score | Technique          | `TechniqueKind`   | Status  |
|-------|--------------------|-------------------|---------|
| 110   | Phistomefel Ring   | `PhistomefelRing` |         |
| 120+  | ALS family         | —                 | planned |

### Linear view (all branches by score)

Naked Pair (32) · X-Wing (38) · Hidden Pair (44) · Naked Triple (50) ·
Swordfish (56) · Hidden Triple (62) · XY-Wing (68) · XYZ-Wing (72) ·
W-Wing (76) · Naked Quad (82) · Jellyfish (86) · XY-Chain (88) · Hidden Quad (92)

## Train vs. Drill

Each technique offers two modes. Both **force** the target technique to appear
(at least the tunable count — see verifier); they differ in what the player is
allowed to lean on to reach it.

- **Train** — the target is forced, and every *simpler* technique is **allowed**,
  but constrained to the target's branch through Expert. All of Beginner and
  Intermediate are allowed unconditionally, and within Expert only the
  same-branch techniques simpler than the target are allowed. The puzzle is
  solvable with that allowed set plus the forced target.

- **Drill** — allow all *easier tiers* in full, and **concede** the simpler
  techniques in the same branch. Conceded techniques may fire if they happen to
  apply, but the puzzle is not promised to be solvable through them — the target
  must still be genuinely required. Drill isolates the target against its
  immediate neighbours.

Per tier:

- **Beginner** — train only (hidden singles). Drill is not meaningful here.
- **Intermediate** — train allows the **other** Intermediate techniques + Beginner.
  Drill allows Beginner and **concedes** the **other** Intermediate techniques.
- **Expert** — train allows all of Beginner/Intermediate + simpler same-branch
  Expert techniques. Drill allows all of Beginner/Intermediate and **concedes**
  the simpler techniques from the same branch.

In short: train *allows* the in-scope peers — the simpler ones down a branch, or
all the others in a flat tier — and drill *concedes* those same peers, so the
puzzle is forced to depend on the target rather than be short-circuited by one.

## Verifier contract

A generated puzzle clears only if it passes two checks against the spec:

1. **Solvable by allowed + forced.** The puzzle must solve to completion using
   only the allowed and forced techniques.

2. **Stuck without forced.** Using allowed + conceded techniques (the full
   in-scope toolbox *minus* the target), the solver must get stuck. Each time
   the forced technique is genuinely required, it is applied to the stuck state
   and the solve continues. The puzzle clears only if it got stuck — i.e.
   required the forced technique — **at least as many times as specified.** The
   required count is tunable per forced technique ("appears at least N times").

This is what makes a forced technique *unavoidable* rather than merely possible:
if a simpler/conceded technique could substitute for it, the second check would
not get stuck, and the puzzle is rejected.

## Not modeled / out of scope

A few SE entries are intentionally absent:

- **Full House** ("last value in a unit", SE 1.0) — the trivial case where a unit
  has one empty cell. We don't treat it separately; it's just a single. At most a
  coaching label, not a distinct technique.
- **"Direct" variants** (Direct Pointing/Claiming/Hidden Pair/Hidden Triple) — SE
  scores a move lower when its elimination *immediately exposes a single*
  (placement payoff) versus only trimming candidates. That's a per-move
  puzzle-rating axis, not a separate skill, and it never matters here: under
  forcing, a "direct" occurrence is solved by the resulting single, so the
  verifier's avoid-target walk never *requires* the technique. Every puzzle we
  force for a subset is therefore the full (non-direct) variety by construction.
  We score techniques at their full difficulty and keep one node each.
- **Uniqueness** (unique rectangles and loops, SE ~4.5–5.0) — deductions that
  assume the puzzle has a single solution and eliminate "deadly pattern"
  candidates. A distinct family resting on a meta-assumption rather than pure
  grid logic, and not the same as the Phistomefel ring (set equality). Out of
  scope for now.
