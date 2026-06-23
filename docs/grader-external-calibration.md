# Grader — external human calibration (breaking the self-reference)

The plan for tuning the difficulty grader against the **external human-difficulty datasets**
now in [`datasets/normalized/`](../datasets/README.md) — the concrete realisation of the "F2"
reference that [`grader-continuous-scoring.md`](grader-continuous-scoring.md) §6 deferred while it
was hypothetical. The data has landed; this doc says how to use it.

> Status: **Stage 0 BUILT & MEASURED (2026-06-23); Stages 1-3 not started.** The spec-free
> grading path ([`grade_puzzle`](../generator-lab/src/grade.rs), the G1 gap) and the
> [`datasets-correlate`](../generator-lab/examples/datasets_correlate.rs) diagnostic harness are
> landed; the grading path itself is unchanged (Stage 0 is read-only). No signal re-orientation,
> no global-number fit, no recalibration yet. The Stage-0 baseline table + findings are in §5; the
> headline is **the G1 distribution shift is real and severe**, so the Stage-2 natural-puzzle
> re-mine is now *required*, not optional. The data was arranged and normalised first
> ([[project_grader_external_datasets]]).

## 1. The problem: the grader only agrees with itself

The Tier A–E grader is internally consistent and has never been compared to a human. Concretely,
the self-reference is wired into two places:

- **Calibration.** [`TechNorm::calibrate(kind, sample, rel)`](../generator-lab/src/grade.rs) sets
  every signal's **sign** (orientation) and keeps/drops its **weight** from `rank_corr(signal,
  rel)` — and `rel` is the project's own relative grader,
  [`grade_signals`](../generator-lab/src/grade.rs) ≈ [`grade_batch`](../generator-lab/src/grade.rs).
  So a signal is oriented "harder" by whether it agrees with `grade_batch`, not with a human.
- **Acceptance.** The §2.4 faithfulness gate in [`grader-continuous-scoring.md`](grader-continuous-scoring.md)
  is *Spearman rho vs `grade_batch` ≥ 0.85*. "Faithful" is defined as "agrees with `grade_batch`."

And `grade_batch` itself is a hand-weighted heuristic — [`Weights::default`](../generator-lab/src/grade.rs)
= dry `0.40` / count `0.25` / scarcity `0.20` / scan `0.15` over four signals, picked by eye, never
validated. So the whole stack proves only that it agrees with one unvalidated ancestor heuristic.
Every refinement since (the granular blend, the CDF rating, the trajectory and camo signals) inherits
that ancestor as its ground truth. That is the "self-referential in a bad way."

The known symptom is already in the code: the doc notes `alts` "calibrates to sign `+1` (more-alts ↔
tighter-firing in-corpus), not the intuitive `−1`," and the project resolved the conflict in favour of
`grade_batch` because *"the project's standard IS faithfulness to grade_batch."* That is exactly the
question only an external reference can settle. The datasets are that reference.

## 2. What the data gives us, and the one rule it imposes

[`datasets/normalized/`](../datasets/README.md) holds **real human signal**: solve times, completion
rates, and human-set difficulty labels. Seven **groups** (`catalog.json`), each one self-consistent
partial order, columns `puzzle,label_value,label_raw,weight` with `label_value` numeric and **higher =
harder**:

| group | kind | n | the human signal |
|---|---|---|---|
| `synnwang/solve_time` | continuous | 1533 | mean human solve seconds, `weight` = player count |
| `synnwang/D_TO`, `D_TR` | continuous | 344 | time-only / time+completion difficulty metric |
| `armane/org.uk` | ordinal (4 lvl) | 240 | Gentle < Moderate < Tough < Diabolical |
| `armane/sotd` | ordinal (6 lvl) | 360 | Beginner < … < Diabolical |
| `armane/extreme` | ordinal (5 lvl) | 300 | Evil < … < Extreme (expect ~0 coverage, all above-toolbox) |
| `sakana/nhk` | ordinal (3 lvl) | 99 | Nikoli hand-set easy / medium / hard |

**The one hard rule: rank is defined WITHIN a group only.** Labels are never comparable across groups
(different scales, metrics, populations). Every correlation, every fit, every acceptance number is
computed per group and only *then* aggregated (mean of per-group scores, never a pooled regression over
rows). Cross-group pooling is the one mistake that silently re-introduces a fake absolute scale.

**Precedent (the yardstick).** Pelánek 2014 ([`docs/pelanek-2014-sisus-refutation-dependency.md`](pelanek-2014-sisus-refutation-dependency.md))
correlates computed Sudoku difficulty metrics against *mean human solve time* — the same target as
`synnwang/solve_time` — and reports Pearson **r = 0.68 / 0.83** for his best (refutation/dependency)
metrics. That is the realistic band to aim for and to read our numbers against; a cold below-chains
grader on a restricted-range subset will likely land at the low end, and that is informative, not a
failure.

## 3. Two architectural gaps before the data is usable

The current grader cannot grade a dataset puzzle at all. Two things must be built first.

### G1 — spec-free grading (infer the bottleneck from the solve, not from a `Spec`)

Every entry point keys on a `Spec`: [`bottleneck_key(spec)`](../generator-lab/src/grade.rs),
[`bottleneck_mask(spec)`](../generator-lab/src/grade.rs), and
[`grade_one(spec, puzzle)`](../generator-lab/src/grade.rs) which solves with `spec.baseline_mask()`.
A dataset puzzle is a bare 81-char string with no spec. Add a spec-free path:

```
grade_puzzle(puzzle) -> Option<(Signals, f64 rating)>
  trace   = solve_graded(puzzle, FULL_TOOLBOX)         // all 16 kinds allowed
  key     = argmax_{k >= NAKED_PAIR, counts[k] > 0} DIFFICULTY[k]   // hardest harder-kind that fired
            -> None if the cold solve did not finish   // needs chains: UNGRADEABLE
            -> rating 0 (trunk-only) if no harder kind fired at all
  signals = signals_of(trace, 1 << key)                // same featurizer, inferred bottleneck mask
  rating  = rating_from_cdf(granular_score(signals, NORM[key]), CDF[key])
```

This mirrors [`bottleneck_key`](../generator-lab/src/grade.rs)'s "hardest member" choice, but reads it
off the trace instead of the spec. It reuses `signals_of` / `granular_score` / the baked tables
unchanged — the only new logic is *which* kind is the bottleneck and *which* toolbox solved it.

**BUILT** as [`grade_puzzle(puzzle) -> Option<PuzzleGrade>`](../generator-lab/src/grade.rs)
(`PuzzleGrade { signals, key: Option<usize>, rating }`, `key = None` = trunk-only): `None` exactly
when the [`FULL_TOOLBOX`](../generator-lab/src/grade.rs) cold solve does not finish (ungradeable).
Additive — the spec-keyed [`grade_one`](../generator-lab/src/grade.rs) path is untouched.

**Distribution-shift caveat (must be measured, not assumed).** The baked
[`GRANULAR_NORM`/`GRANULAR_CDF`](../generator-lab/src/grade.rs) were mined from *isolated*
train/drill specs that force exactly one technique and forbid easier same-branch ones. A natural
dataset puzzle solved with the full toolbox produces a *different* signal distribution at the same
inferred bottleneck (more techniques in play, different context). So `grade_puzzle` against the
isolated-mined tables is an approximation. Stage 0 validates whether the shift is material; if it is,
Stage 2 re-mines a "natural-puzzle" calibration pool (the gradeable dataset puzzles themselves,
holdout-split) and bakes a second `NORM`/`CDF` for the spec-free path. One change at a time.

**MEASURED (Stage 0, §5): the shift is severe.** The isolated tables assume every puzzle *has* a
forced hard bottleneck; natural puzzles do not, so the rating pins at the extremes — ~96% of the
`synnwang` puzzles are **trunk-only** (singles + locked candidates solve them, no harder kind fires)
and rate a flat `0`, while `armane/extreme` rates a mean `0.83` with 26% clamped at `1`. The
re-mine is therefore *required*. It also exposes a deeper gap the re-mine alone won't close: the
grader has **no resolution within the trunk-only range**, yet that is exactly where the whole
`synnwang` corpus (mean human solve time 130-6192 s, all "easy" by technique) lives — so ranking
those at all needs a finer easy-range signal (e.g. singles/LC fill-path depth) even when no harder
kind fires.

### G2 — a cross-technique axis (the global number C2, now anchored)

The grader's output is a **within-technique** percentile ([`rating`](../generator-lab/src/grade.rs)):
an x-wing at `0.6` and a jellyfish at `0.6` carry no shared meaning. The dataset labels mix techniques
on one human scale, so to correlate against them we need a number that orders puzzles *across*
techniques. That is exactly the optional global number C2 deferred in
[`grader-continuous-scoring.md`](grader-continuous-scoring.md) §5.2:

```
global(puzzle) = base_T + span_T · rating_T(puzzle)        // T = inferred bottleneck technique
```

C2 was deferred "for lack of an anchor." The datasets *are* the anchor: fit `base_T` / `span_T` so the
global order matches the **human** label within each group (§5), not the branch-pooled `grade_batch`
the §5.2 draft proposed. This is the single highest-leverage use of the data, because cross-technique
is precisely where `grade_batch` has *nothing* to say (it is a per-batch relative cut) and where the
human labels *do* span techniques.

## 4. Metrics (per group, over the solvable subset)

- **Coverage first, always reported.** Per group: fraction of puzzles `grade_puzzle` can finish cold.
  The hard ordinal tiers (`armane/extreme` entirely, Diabolical/Fiendish elsewhere) will be near-zero
  until chains land. Coverage is a *finding* — it quantifies the chains gap — and it bounds every other
  number (a 0.8 rho over 5% of a group is weak evidence). Never silently drop the uncovered tail; log
  it.
- **Continuous groups** (`synnwang/*`): **weighted Spearman rho** of `global` vs `label_value`, weight
  = `weight` (player count for `solve_time`; the others are 1). Pearson on ranks ≈ Pelánek's r, so it
  is directly comparable to the 0.68/0.83 yardstick.
- **Ordinal groups** (`armane/*`, `sakana/nhk`): big tie classes, so use a tie-aware rank measure —
  **Kendall tau-b** (or Somers' D with the level as dependent) of `global` vs the level index — plus
  the readable diagnostic: **mean/median `global` per level must be monotone non-decreasing**, and the
  adjacent-level AUC (P[random higher-level puzzle scores above a random lower-level one]) > 0.5.
- **Aggregate** per-group numbers by averaging (optionally weighted by gradeable n), **never** by
  pooling rows across groups (the §2 rule).

## 5. Staged plan (validate before tuning, one change at a time)

**Stage 0 — measure the self-reference (diagnostic, zero tuning).** Build `grade_puzzle` (G1) and a
`datasets-correlate` harness (an example alongside [`grade_diag`](../generator-lab/examples/grade_diag.rs))
that reads `datasets/normalized/`, grades the solvable subset of each group with the *existing baked
tables*, and prints coverage + the §4 metrics per group. This answers the actual question — *how badly
does the current grader disagree with humans?* — and exposes the G1 distribution shift. No code in the
grading path changes. Output is a table the rest of the plan is judged against.

### Stage 0 results (measured 2026-06-23)

Run: `cargo run --release -p generator-lab --example datasets_correlate`. `global = DIFFICULTY[T] +
rating_T` (the un-fit C2 baseline of §3-G2: the project's own per-technique difficulty as `base_T`,
`span_T = 1`). Rank computed WITHIN each group only.

| group | kind | n | covered | correlation vs human | rating shift |
|---|---|---|---:|---|---|
| `synnwang/solve_time` | continuous | 1533 | 98% | wSpearman **+0.215** | 96% trunk (rating≈0) |
| `synnwang/D_TO` | continuous | 344 | 99% | wSpearman **+0.262** | 97% trunk |
| `synnwang/D_TR` | continuous | 344 | 99% | wSpearman **+0.270** | 97% trunk |
| `armane/org.uk` | ordinal (4) | 240 | 86% | tau-b **+0.330**, monotone, AUC 0.69 | mean 0.17, 76% trunk |
| `armane/sotd` | ordinal (6) | 360 | 89% | tau-b **+0.443**, monotone, AUC 0.71 | mean 0.25, 64% trunk |
| `sakana/nhk` | ordinal (3) | 99 | 100% | tau-b **+0.388**, monotone, AUC 0.71 | mean 0.28, 58% trunk |
| `armane/extreme` | ordinal (5) | 300 | 99% | tau-b **+0.154**, monotone, AUC 0.56 | mean 0.83, 26% at 1 |

Four findings, in order of consequence:

1. **Coverage is HIGH, not near-zero — the §2/§7 "below chains" prediction was wrong.** The toolbox
   (singles + LC + subsets + fish + wings) finishes 86-100% of *every* group, including **99% of
   `armane/extreme`** (predicted ~0). The chains gap bites only at the very top ordinal tier:
   `org.uk` Diabolical 29/60 (48%), `sotd` Diabolical 21/60 (35%). The site "extreme/evil" labels
   mostly do **not** require chains — they overstate logical depth relative to this toolbox. (Sanity-
   checked the other way: genuinely chains-hard puzzles — AI-Escargot-class — return `None`, so the
   coverage is real, not a false-`solved` bug.)

2. **The G1 distribution shift is severe → Stage 2's re-mine is required.** The baked CDF was mined on
   isolated specs that *force* a hard technique, so it assumes every puzzle has a hard bottleneck.
   Natural puzzles pin at the extremes: `synnwang` is **~96% trunk-only** (rating flat 0),
   `armane/extreme` rates mean **0.83** with 26% clamped at 1. The isolated tables do not fit the
   natural distribution.

3. **Continuous rho ≈ 0.22-0.27, far under Pelánek's 0.68/0.83 — but it is the trunk-pinning
   artifact, not (yet) a verdict on the signals.** With 96% of `synnwang` tied at `global = 0` the
   order has almost no resolution to correlate; the corpus lives in the easy range the grader
   flattens. The deeper gap: the grader has **no within-trunk resolution at all**, yet `synnwang`'s
   human solve time (130-6192 s, all "easy" by technique) varies entirely *inside* that range. Ranking
   it needs an easy-range signal (singles/LC fill-path depth) even when no harder kind fires — a new
   work item the data surfaced, beyond the §3-G2 plan.

4. **Ordinal order is directionally correct.** Every ordinal group is **monotone in mean-`global`-
   per-level** and tau-b is positive, rising with how much the labels track our toolbox's depth
   (`sotd` +0.44 > `nhk` +0.39 > `org.uk` +0.33 > `extreme` +0.15, the last weakest because its labels
   barely track below-chains depth). So the cross-technique `DIFFICULTY`-anchored baseline already
   sorts the human levels the right way on average — the room is in resolution, which is what Stages
   1-2 add.

**Next steps (re-prioritised by the Stage-0 findings).**

- **NEW prerequisite — within-trunk resolution (gates the continuous groups).** Finding 3 shows the
  `synnwang` corpus is ~96% trunk-only, where the grader emits a flat `0`. No global-number fit can
  rank a flat column. Before Stage 2 can help `synnwang`, the grader needs an *easy-range* signal that
  varies when no harder kind fires — candidates already in the trace: the singles/LC **fill-path
  depth** (how long the cheap closure took, where it stalled-and-resumed), the LC firing count, the
  candidate-population profile. This is outside the original §3-G2 plan and is the single
  highest-leverage change for the continuous groups; do it as its own measured step, then re-run
  Stage 0. (The ordinal groups are less affected — their hardness spans technique tiers, which
  `global` already captures.)
- **Stage 1 (signal signs) is data-thin and should target the dense buckets only.** The gradeable
  non-trunk puzzles concentrate in a few kinds (subsets, `xy`/`w-wing`); only those buckets can
  re-adjudicate a sign (the `alts ±1` conflict) against the human label. Everywhere else keep the
  `grade_batch` fallback — exactly as the stage already says, now confirmed by the bucket counts.
- **Stage 2 re-mine is confirmed required** (not conditional). Fit `base_T`/`span_T` and re-mine the
  natural-puzzle `NORM`/`CDF`, holdout-split per group.
- **Use the ordinal groups as the near-term acceptance target.** They already validate directionally
  (monotone, positive tau-b) and are not trunk-dominated, so tau-b / AUC lift is the cleanest signal
  that a change helped — the continuous groups stay noisy until the within-trunk gap is closed.

**Stage 1 — adjudicate signal orientation with the human reference.** Where a technique bucket has
enough gradeable dataset puzzles, recompute each signal's sign from `rank_corr(signal, human_label)`
instead of `rank_corr(signal, grade_batch)`. The first thing to settle is the known `alts` conflict
(§1): does the human reference orient it `+1` or `−1`? Re-bake [`TechNorm::calibrate`](../generator-lab/src/grade.rs)'s
`rel` argument from human labels for the buckets that support it; keep `grade_batch` as the fallback
`rel` for buckets too thin to fit (most within-technique buckets will be thin — that is expected, and
the cross-technique fit in Stage 2 is where the data is dense). Measure the within-technique rho and
split-half stability do not regress (the C/D/E gains are kept; this only re-orients, it does not
re-weight).

**Stage 2 — fit and anchor the global number (G2 + the natural-puzzle re-mine if Stage 0 demands it).**
Fit `base_T` / `span_T` to minimise within-group rank loss against the human labels (a monotone /
isotonic fit, summed over groups, never pooling labels). Holdout-validate: fit on half each group,
score the other half, report held-out rho/tau. If Stage 0 found the distribution shift material, this
is also where the spec-free `NORM`/`CDF` is re-mined over the natural-puzzle pool and baked as a second
table. Ship the global number as a **separate, optional** read — the UI badge stays the within-technique
sub-tier (the standing decision in [`grader-granular-scoring.md`](grader-granular-scoring.md)); the
global number feeds sorting / a daily-difficulty curve / cross-puzzle comparison.

**Stage 3 — optional per-technique re-weighting (F1 against the human ref).** Only if Stages 1–2 leave
a residual a technique bucket is large enough to fix: fit per-technique weight vectors by
monotone-constrained regression against the human label, holdout-gated (accept only weights whose
held-out rho beats the per-branch baseline — small buckets overfit instantly). This is the heaviest
step and may simply not have the data within most technique buckets; do not force it.

Re-use the resumable [`grade_diag`](../generator-lab/examples/grade_diag.rs) mining cache machinery so
the dataset solves are computed once and recalibration is instant.

## 6. Acceptance criteria

1. **Coverage reported, tuning on the solvable subset only**, the uncovered tail logged per group.
2. **Continuous groups:** weighted Spearman rho of `global` vs human improves on the Stage-0 baseline
   and is read against the Pelánek 0.68/0.83 band (with the restricted-range caveat stated, not hidden).
3. **Ordinal groups:** monotone mean-`global`-per-level and Kendall tau-b improve on Stage-0.
4. **Held-out:** every fitted number (signs, `base_T`/`span_T`, any weights) is validated on a group
   half it was not fit on; report held-out, not in-sample, numbers.
5. **No regression of the internal grade:** within-technique split-half stability (≥ 0.90, §2.3) and
   the C/D/E gains survive — the human anchor sits on top of the within-technique rating, it does not
   replace it.
6. **No cross-group pooling** anywhere — per-group metrics aggregated only by averaging.

## 7. Risks and honest limits

- **The toolbox caps coverage — but far less than feared (Stage 0 measured 86-100%, §5).** The fear
  was that the hard human tiers would be ungradeable below chains; in fact only the very top ordinal
  tier loses coverage, and the rest is easy-to-medium. So the real limit is the *opposite*: the
  gradeable corpus is concentrated in the easy range, where the grader has the least resolution (the
  trunk-pinning of finding 3). The hardest puzzles still stay out of reach until chains land; state the
  gradeable range with every number.
- **Restricted range deflates correlation.** Dropping the hard tail compresses the difficulty range, so
  rho will read lower than Pelánek's full-range numbers even if the grader is good on what it covers.
  Report coverage alongside rho so the two are read together.
- **Thin within-technique buckets.** Most of the data's power is *cross-technique* (Stage 2). Within a
  single technique a group may have too few gradeable puzzles to re-orient or re-weight reliably — hence
  the `grade_batch` fallback and the holdout gates. Do not manufacture per-technique fits the data
  cannot support.
- **The human label is itself noisy / population-specific.** `synnwang` is one app's player base;
  `armane` is per-site editorial labels. Treat each as one partial order, weight by confidence where it
  exists (`player_count`), and never collapse them into a single ground truth.

## 8. Relation to other docs and memory

- [`grader-continuous-scoring.md`](grader-continuous-scoring.md) — §6 F2 / §5.2 C2: this doc is their
  concrete realisation now the data exists. The within-technique Tiers C/D/E it ships are the backbone
  this plan anchors externally; it does not undo them.
- [`grader-granular-scoring.md`](grader-granular-scoring.md) — the within-technique 3-band cut; the
  standing decision that the UI badge stays a sub-tier read (the global number is separate) holds here.
- [`campaign-grader-plan.md`](campaign-grader-plan.md) — the relative `grade_batch` grader, i.e. the
  *self-reference* this plan replaces as the calibration target (kept only as the thin-bucket fallback).
- [`pelanek-2014-sisus-refutation-dependency.md`](pelanek-2014-sisus-refutation-dependency.md) — the
  methodological precedent: correlate computed difficulty against human solve time; the 0.68/0.83
  yardstick.
- [`datasets/README.md`](../datasets/README.md) + `datasets/normalized/catalog.json` — the data shape
  this plan consumes (read `normalized/` only, never the raw sources).
</content>
</invoke>
