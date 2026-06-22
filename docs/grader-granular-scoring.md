# Grader — granular per-puzzle scoring

A redesign of the **absolute single-puzzle** scoring the web app uses, so the
gentle / medium / spicy band is a clean *sub-tier* read for every technique. This is a
follow-up to [`campaign-grader-plan.md`](campaign-grader-plan.md): same model of human
difficulty, same signals philosophy, but a *finer* score that does not collapse under
quantile banding.

> Status: **spec only.** The current code (`hardness_score` + per-technique `THRESHOLDS` in
> [`grade.rs`](../generator-lab/src/grade.rs)) is the coarse interim — it bands "by what we
> have now." This doc specifies what replaces it. No implementation yet.

## 1. The problem: the absolute score is too quantized to band

The relative grader ([`grade_batch`](../generator-lab/src/grade.rs)) ranks a whole node's
batch against itself and quantile-cuts — it never needs an absolute scale, and ties are
broken by the rank's mid-point. The web app generates **one puzzle at a time**, so it instead
maps a single puzzle to a stable score ([`hardness_score`](../generator-lab/src/grade.rs)) and
cuts it against a per-technique threshold pair ([`THRESHOLDS`](../generator-lab/src/grade.rs)),
calibrated to even thirds.

That score is

```
hardness_score = grind + 1/(1 + scarcity)
grind          = longest_dry_run + (bottleneck_count - 1)      // small integer, usually 0
scarcity       = min elims over the bottleneck firings         // small integer, usually 1..15
```

Both terms are **coarse integers**, so the score lands on a handful of discrete values and many
puzzles tie. Even-thirds cuts then can't separate them. Measured over a mined corpus
(150 puzzles/spec, train + drill, via [`grade_diag`](../generator-lab/examples/grade_diag.rs)):

| technique | why it degenerates | banded g / m / s |
|---|---|---|
| **xyz-wing** | the wing eliminates ≈ **1** candidate every time → `scarcity≈1` → `1/(1+1)=0.5` for nearly all; `grind` rarely moves → p33 = p66 = 0.5, **medium band empty** | 3 / 0 / 97 |
| x-wing / swordfish / jellyfish | `grind ≡ 0` (fires once, single follows) and `scan_work ≡ 1`; the **only** live signal is `scarcity`, itself just `{1/2, 1/3, 1/4, …}` → near-binary | ~28 / ~30 / ~40, lumpy |
| subsets (pair/triple/quad) | `grind` varies a little, so these band acceptably, but spicy still over-weights on the integer step | ~25 / ~40 / ~35 |

The root cause is **not** the weights or the thresholds — it is that the *signals themselves*
have too few distinct values within a technique. A technique whose elimination count is
structurally fixed (every wing kills one candidate) has nothing left to vary. We need signals
that stay live **within** such a node.

## 2. Goal and acceptance criteria

A per-puzzle score `S(puzzle, spec)` such that, **per technique**, over a mined corpus:

1. **No degenerate cut.** The 33rd and 66th score percentiles are distinct (`p33 < p66`), so
   all three bands are reachable — in particular xyz-wing and the fish must split.
2. **Even thirds within tolerance.** Each band holds `33% ± 8%` of a technique's puzzles.
3. **Monotone in felt difficulty.** Within a technique the score is non-decreasing in the
   ordered signals (more firings, longer dry runs, scarcer / harder-to-spot stalls score
   higher). The integer "grind" stays the dominant axis where it varies.
4. **Faithful to the relative grader.** On a held-out batch, the absolute band agrees with
   [`grade_batch`](../generator-lab/src/grade.rs)'s relative band on ≥ 80% of puzzles (the
   absolute path is a one-at-a-time stand-in for the same ordering, not a different opinion).
5. **Cold-path only.** Still one instrumented easiest-first solve per yielded puzzle, off the
   hot generation loop. No branching, no path quantification (that is the separate
   `GENERATION-RULES.md` grader).

## 3. The granularity principle

> Every technique must have at least one signal that varies **continuously within that
> technique's node**, so puzzles do not tie at the cut points.

The current signals are all small integers, and several are *constant* within a branch:

| signal | subsets | fish | wings | continuous? |
|---|---|---|---|---|
| bottleneck_count | varies | ≡ 1 | varies a little | no (small int) |
| longest_dry_run  | varies | ≡ 0 | varies a little | no (small int) |
| scarcity (min elims) | varies | varies (1..15) | ≡ 1 | no (small int, branch-pinned) |
| scan_work | varies | ≡ 1 | varies a little | no (small int) |

No single signal is live everywhere. The fix is to add signals read off the **stall state**
that are continuous and stay informative even when the technique's elimination count is fixed.

## 4. New signals (the granularity fix)

All are read at the **tightest bottleneck** (the stall whose firing is hardest), with the
first bottleneck as a secondary. They are additive to the existing
[`GradeStep`](../generator-lab/src/solve/logic.rs)/`GradeTrace` and computed in the same cold
`solve_graded` loop.

### S5 — Stall openness `open` (primary continuous signal)

The total live-candidate population at the moment the bottleneck must fire —
[`candidate_population`](../generator-lab/src/solve/logic.rs) already computes exactly this; we
just need to **record it at the bottleneck** instead of only as a per-step delta. Range
≈ 40..200, effectively continuous. A sparser board (the technique is the only move on a board
with few candidates) is a different hunt from a busy one. Orientation is set by calibration
(§6) — correlate `open` against the relative grade and fix its sign + weight per branch.

### S6 — Bottleneck fill-depth `depth`

How many cells are already placed when the **first** bottleneck fires (`occupied_count` at that
state — [`occupied_count`](../generator-lab/src/solve/logic.rs) exists). Range 0..81,
continuous. A late bottleneck means the cheap closure carried the solver a long way first;
an early one means the puzzle resists immediately.

### S7 — Toolbox-wide alternatives `alts` (the honest scarcity)

At the tightest stall, the count of **all** distinct productive deductions across the allowed
toolbox — the `live`-style count [`campaign-grader-plan.md` §The signals/3](campaign-grader-plan.md)
deferred as "highest-effort," replaced here because it is the principled cure for the wing
degeneracy. Where min-elims is structurally pinned (wings ≡ 1), the number of *eligible wing
patterns on the board* still varies: one eligible pattern is a needle hunt, several and you
stumble onto one. Needs a **non-mutating, enumerate-don't-short-circuit** scan variant of each
technique (today [`step_once`](../generator-lab/src/solve/logic.rs) and the fused
[`ladder`](../generator-lab/src/solve/fused.rs) stop at the first hit). Lower = harder.

### S8 — Payoff cascade size `cascade` (finer dry/payoff)

How many cells the cheap closure places **immediately after** the tightest bottleneck firing
(the occupancy delta the current dry/payoff flag already thresholds at ≥ 1 — keep the integer,
just also record the magnitude). A firing that unlocks a long single-cascade is gentler than
one that yields a single placement and re-stalls. Continuous-ish; refines the binary dry flag.

### S3′ — Mean / sum bottleneck elims (cheap refinement)

Replace `scarcity = min(elims)` with also carrying `sum` and `mean` over the bottleneck
firings. Strictly finer than `min` at no new instrumentation (the per-step `elims` are already
in [`GradeStep`](../generator-lab/src/solve/logic.rs)). Useful for subsets; insufficient alone
for the branch-pinned cases (hence S5/S7).

## 5. The scoring functions (per branch)

The discriminating signal differs by [`Branch`](../generator-lab/src/spec/kinds.rs), so the
score is a **family** keyed by the bottleneck technique's branch, not one global formula. Each
is a continuous weighted sum over **per-technique-normalized** signals (z-score or
percentile-rank against that technique's calibration sample — §6), so the absolute score
reproduces the relative grader's normalized-rank sum one puzzle at a time:

```
S = grind                                   // integer backbone, the dominant axis where it moves
  + squash( Σ_k w_branch[k] · norm_T(sig_k) )   // continuous sub-order in [0, 1), never crosses a grind step
```

`norm_T` maps a raw signal to its per-technique percentile in `[0, 1)` (baked breakpoints, §6);
`squash` keeps the blended sub-order strictly `< 1` so `grind` still dominates wherever it
varies. Suggested per-branch weights (starting point; calibration tunes magnitudes, priority is
fixed):

| branch | live signals → weights |
|---|---|
| **Subset** | grind leads; then `alts` (S7), `mean elims` (S3′), `open` (S5) |
| **Fish** | grind ≡ 0 → `open` (S5) + `alts` (S7) lead; `min elims` secondary |
| **Bivalue (wings)** | `min elims` pinned → `alts` (S7) + `open` (S5) lead; grind contributes for xyz/w |

Two implementation tiers, ship in order:

- **Tier A (minimal, no new enumerate scan).** Add only S5 (`open`) and S8 (`cascade`) — both
  fall out of `candidate_population`/`occupied_count` already in the loop — plus S3′. Keep the
  `grind` backbone; the continuous `open` term alone breaks the fish/wing ties. This clears
  acceptance §2.1–§2.3 for fish and most wings without the expensive S7.
- **Tier B (full).** Add S7 (`alts`), the toolbox-wide enumerate count, for the honest scarcity
  and the §2.4 faithfulness target. This is the larger build (a non-mutating enumerating scan
  per technique) and is the same work `campaign-grader-plan.md §3` flagged as deferred.

## 6. Calibration workflow

Per-technique normalization breakpoints (`norm_T`) and the band cut points are **data**, mined
once and baked — exactly as `THRESHOLDS` is today, just richer.

The harness already exists: [`grade_diag`](../generator-lab/examples/grade_diag.rs) mines a
resumable, per-(mode, target) puzzle cache (never re-derives — some hard targets are seconds per
puzzle), grades from cache instantly, and with `--calibrate` prints paste-ready cut points.
Extend it to also emit, per technique, the per-signal percentile breakpoints `norm_T` needs.

```
cargo run --release -p generator-lab --example grade_diag -- --count 400 --jobs 4 --calibrate
```

Pool **train + drill** per technique (drill skews harder; pooling lets drill read spicier and
train gentler *within* the technique, which is the intended sub-tier behaviour). Re-run after
any change to the generator or the grading solve — the table is calibrated, not derived.

## 7. Instrumentation summary

| where | change |
|---|---|
| [`GradeStep`](../generator-lab/src/solve/logic.rs) | add `open` (candidate population before the step), `cascade` (placements in the following closure), keep `elims` |
| `GradeTrace` | nothing structural — signals reduce from the richer steps |
| `solve_graded` | record `open`/`depth` at the bottleneck (both helpers exist); Tier B: call the enumerate scan before taking the forced step for `alts` |
| techniques | Tier B only: a non-mutating "count all firing instances at this state" variant (no short-circuit) |
| [`signals_of`](../generator-lab/src/grade.rs) | reduce the new fields (tightest-stall `open`/`alts`, `mean`/`sum` elims, `cascade`) |
| [`hardness_score`](../generator-lab/src/grade.rs) | replace with the per-branch `S` of §5; key the branch off [`bottleneck_key`](../generator-lab/src/grade.rs) |
| `THRESHOLDS` + `norm_T` tables | regenerate via §6 |
| [`grade_diag`](../generator-lab/examples/grade_diag.rs) | emit per-signal breakpoints; add the §2 acceptance checks (distinct cuts, thirds tolerance, faithfulness vs `grade_batch`) |

The fused fast path ([`fused.rs`](../generator-lab/src/solve/fused.rs)) stays untouched — all of
this is cold, post-validity, one solve per yielded puzzle.

## 8. Open decisions

- **`open` orientation per branch.** Whether sparser-board (low `open`) is harder or easier is
  an empirical question; fix the sign per branch from the calibration correlation, do not assume.
- **`norm_T` form.** Percentile-rank breakpoints (faithful to `grade_batch`, bigger table) vs a
  two-number mean/scale z-score per signal (smaller, assumes roughly unimodal). Start with
  mean/scale; upgrade to breakpoints only where §2.4 fails.
- **Per-(technique) vs per-(technique, mode) tables.** This doc pools modes (§6). If train and
  drill diverge too far for a technique to pool cleanly, split that row — at the cost of
  `bottleneck_key` needing a drill/train discriminant off the spec.
- **Custom-spec key.** A multi-force / `force_any` custom spec keys on its hardest member
  ([`bottleneck_key`](../generator-lab/src/grade.rs) already does this); confirm that read is
  acceptable for arbitrary user toolboxes, or fall back to a branch-generic table.
- **Tier A sufficiency.** Whether `open` + `cascade` alone clear acceptance for the wings, or
  whether xyz/w-wing genuinely need S7 (`alts`). Measure Tier A first; only build Tier B for the
  techniques that still degenerate.

## 9. Relation to other docs

- [`campaign-grader-plan.md`](campaign-grader-plan.md) — the original per-node **relative**
  grader and its four signals. This doc keeps that model and makes the **absolute** one-puzzle
  projection of it granular enough to band. S7 here is that plan's deferred "fuller scarcity."
- `GENERATION-RULES.md` — the future path-quantified grader; still separate and heavier. This
  remains current-code, single-path, cold.
