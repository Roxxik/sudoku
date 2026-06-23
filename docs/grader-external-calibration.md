# Grader — external human calibration (per-spec gradings, Pelánek as the bar)

The plan for making the **per-spec difficulty gradings** the grader already produces agree with
**human** difficulty, using the external human-difficulty datasets in
[`datasets/normalized/`](../datasets/README.md) (the concrete realisation of the "F2" reference that
[`grader-continuous-scoring.md`](grader-continuous-scoring.md) §6 deferred) and the freshly merged
[`pelanek`](../generator-lab/src/pelanek.rs) implementation as the **benchmark to beat**.

> **Scope decision (2026-06-23, user): no global number.** Earlier drafts proposed a cross-puzzle
> "global" difficulty scalar (the C2/G2 number) that orders puzzles *across* techniques. That is
> **dropped.** The grader's output stays what it is today — a **per-spec, within-technique** rating.
> The external data is for making that per-spec rating *better*, not for building a portable scalar.

> Status: **Stages 0-1 BUILT & MEASURED (2026-06-23); Stages 2-4 not started.** Landed: the
> spec-free grading path [`grade_puzzle`](../generator-lab/src/grade.rs) (G1, Stage 0) and the
> reworked [`datasets_correlate`](../generator-lab/examples/datasets_correlate.rs) harness — Stage 1,
> the **corrected per-technique-bucket scoreboard** (§4.4): it buckets each group's covered puzzles by
> inferred bottleneck (incl. the trunk bucket), reports our `rating` vs human and Pelánek vs human
> PER BUCKET, and aggregates n-weighted within a group, **never pooled across techniques**. Pelánek's
> per-puzzle metrics are cached (`--cache`, keyed by the puzzle, invalidated only by `--runs`/`--k`/
> `--model-seed`) so re-runs are instant; `--no-pelanek` skips the bar for fast iteration. The
> grading path itself is unchanged; no tuning yet. The headline findings (§4): **coverage is high;
> our per-spec rating is flat (rating 0) in the trunk bucket that holds most of the data, pinning our
> within-bucket aggregate to ~0 on every group that has a trunk; the one trunk-free group
> (`armane/extreme`) is the only one where we have signal — and there we already match/beat Pelánek;
> everywhere else Pelánek's Dependency carries the bar via the trunk.**

## 1. The problem: the grader only ever agreed with itself

The Tier A-E grader is internally consistent and, until now, had never been compared to a human. The
self-reference is wired in two places:

- **Calibration.** [`TechNorm::calibrate(kind, sample, rel)`](../generator-lab/src/grade.rs) sets
  every signal's **sign** and keeps/drops its **weight** from `rank_corr(signal, rel)` — and `rel`
  is the project's own relative grader, [`grade_signals`](../generator-lab/src/grade.rs) ≈
  [`grade_batch`](../generator-lab/src/grade.rs). A signal is oriented "harder" by whether it agrees
  with `grade_batch`, not with a human.
- **Acceptance.** The §2.4 faithfulness gate in [`grader-continuous-scoring.md`](grader-continuous-scoring.md)
  is *Spearman rho vs `grade_batch` ≥ 0.85* — "faithful" means "agrees with `grade_batch`."

And `grade_batch` is itself a hand-weighted heuristic ([`Weights::default`](../generator-lab/src/grade.rs)
= dry `0.40` / count `0.25` / scarcity `0.20` / scan `0.15`, picked by eye, never validated). So the
whole stack proves only that it agrees with one unvalidated ancestor heuristic. The known symptom is
already noted in the code: `alts` "calibrates to sign `+1` … not the intuitive `−1`," resolved in
favour of `grade_batch` because *"the project's standard IS faithfulness to grade_batch."* That is
exactly the question only an external reference can settle. **Stage 0 settled it: the grader's per-spec
rating disagrees badly with humans (§4).**

## 2. The two new anchors, and the one hard rule

Two things exist now that did not when the grader was built.

### 2.1 The human datasets — the ground truth (F2)

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
| `armane/extreme` | ordinal (5 lvl) | 300 | Evil < … < Extreme |
| `sakana/nhk` | ordinal (3 lvl) | 99 | Nikoli hand-set easy / medium / hard |

The human label is the **only** thing we ever tune toward.

### 2.2 Pelánek (2014) — the bar to beat

[`pelanek`](../generator-lab/src/pelanek.rs) implements the Pelánek 2014 SiSuS model and its two
computed metrics — **Refutation sum** (step complexity) and **Dependency** (frontier width;
*inverse* — bigger = easier). Pelánek correlated these against *mean human solve time* and reported
Pearson **r = 0.68 / 0.83** (the same target as `synnwang/solve_time`). It is **search-based, not
technique-named**: it grades any uniquely-solvable puzzle (no toolbox/coverage limit) and produces a
continuous number even for singles-only puzzles (via Dependency).

**Pelánek is the benchmark, not a tuning target.** It is a *computed* heuristic, so calibrating our
grader to *match Pelánek* would re-create the exact self-reference of §1 — just swapping `grade_batch`
for another proxy. We never tune *to* Pelánek. We measure our per-spec grader against the **human**
data, and Pelánek's human-correlation is the bar we want to **match or beat**. Stage 0 shows that bar
is currently well above us (§4).

### 2.3 The one hard rule: rank is defined WITHIN a bucket only

Labels are never comparable across groups (different scales, populations). And because the grader is
**per-spec**, the relevant bucket is finer than the group: a puzzle's rating only means "hard *for its
technique*", so an `x-wing` rating and a `jellyfish` rating are not comparable either. Every
correlation is therefore computed **per (group × inferred-technique) bucket** and aggregated only
*then* (mean of per-bucket scores, weighted by n). **No pooling across techniques, ever** — that is
the same prohibition that makes the global number unnecessary: there is no single axis to pool onto.

## 3. The one architectural piece this needs (G1, built)

The grader's per-spec entry points key on a `Spec`: [`bottleneck_key(spec)`](../generator-lab/src/grade.rs),
[`grade_one(spec, puzzle)`](../generator-lab/src/grade.rs). A dataset puzzle is a bare 81-char string
with no spec. To grade it per-spec we must infer *which* technique node it belongs to.

**BUILT — [`grade_puzzle(puzzle) -> Option<PuzzleGrade>`](../generator-lab/src/grade.rs)**
(`PuzzleGrade { signals, key: Option<usize>, rating }`):

```
grade_puzzle(puzzle):
  trace  = solve_graded(puzzle, FULL_TOOLBOX)            // all 16 kinds allowed
  key    = argmax_{k >= NAKED_PAIR, counts[k] > 0} DIFFICULTY[k]   // hardest harder-kind that FIRED
           -> None  if the cold solve did not finish     // needs chains: UNGRADEABLE
           -> key=None (trunk bucket) if no harder kind fired
  rating = rating_from_cdf(granular_score(signals_of(trace, 1<<key), NORM[key]), CDF[key])
```

`key` is the **inferred bottleneck** — the spec-less analogue of `bottleneck_key`'s "hardest forced
member", read off the trace instead of the spec. It is what assigns a dataset puzzle to its
technique bucket and selects which per-technique baked table grades it. `key = None` is the **trunk
bucket** (singles + locked candidates solved it — no harder kind fired). Additive — `grade_one` is
untouched.

**The former G2 "global number" is removed** (the §2 scope decision). `grade_puzzle` returns only the
per-technique rating; nothing combines techniques onto one axis.

## 4. Stage 0 results — how bad is it (measured 2026-06-23)

Two harnesses: [`datasets_correlate`](../generator-lab/examples/datasets_correlate.rs) (our grader)
and [`pelanek --dataset`](../generator-lab/examples/pelanek.rs) (the bar). All numbers are rank
correlations of the *computed* difficulty against the *human* label, within a group.

### 4.1 Coverage (the chains gap — far smaller than feared)

The cold below-chains toolbox finishes **86-100% of every group**, including **99% of `armane/extreme`**
(the doc previously predicted ~0). The chains gap bites only the very top ordinal tier — `org.uk`
Diabolical 29/60 (48%), `sotd` Diabolical 21/60 (35%). The site "extreme/evil" labels mostly do **not**
require chains; they overstate logical depth relative to this toolbox. (Sanity-checked the other way:
genuine AI-Escargot-class puzzles return `None`, so coverage is real, not a false-`solved` bug.)
Pelánek has **no coverage gap** — it search-grades every uniquely-solvable puzzle, including the
chains tiers we cannot reach.

### 4.2 Our per-spec grader vs the human label (the cross-technique baseline, now deprioritised)

The first `datasets_correlate` measured a *cross-technique* ordering (the dropped global number,
`DIFFICULTY[T] + rating_T`). We keep it only as evidence that the cross-technique self-reference was
bad; the corrected **per-technique-bucket** measurement is **§4.4 (Stage 1, now built)**.

| group | our (cross-tech) | Pelánek Refutation | Pelánek Dependency (|·|) |
|---|---|---|---|
| `synnwang/solve_time` | Spearman **+0.215** | +0.320 | **0.644** |
| `armane/org.uk` | tau-b +0.330 | Spearman **+0.781** | 0.765 |
| `armane/sotd` | tau-b +0.443 | Spearman **+0.758** | 0.766 |

(Pelánek numbers are small subsamples — 100-150 puzzles, 8-12 runs — so directional, not final; a full
run at the paper's 30 runs would firm them. `synnwang` is the clean apples-to-apples row, both
Spearman; the ordinal rows mix tau-b vs Spearman so the gap is slightly overstated but still large.)
**Pelánek beats us on every group, by a lot, and covers more.** That is the bar.

### 4.3 The findings that drive the plan

1. **The grader has no resolution in the trunk range — and that is where most data lives.** ~96% of
   `synnwang` is **trunk-only** (singles + locked candidates solve it, no harder kind fires); the
   grader rates every one a flat `0`. No fit can rank a constant column, which is why our `synnwang`
   correlation is near-floor. The **trunk bucket is the largest *and* the worst-graded** — closing it
   is the single highest-leverage per-spec improvement, and it is exactly where Pelánek's Dependency
   metric earns its |0.64| (it varies on singles-only puzzles; ours does not).
2. **The baked tables do not fit natural puzzles (the G1 distribution shift).** The CDF was mined on
   *isolated* specs that *force* a hard technique, so it assumes every puzzle has a hard bottleneck.
   Natural puzzles pin at the extremes — `synnwang` ~96% trunk (rating 0), `armane/extreme` mean
   rating 0.83 with 26% clamped at 1. So a natural-puzzle re-mine of `NORM`/`CDF` is required.
3. **Ordinal order is directionally right but coarse.** Every ordinal group is monotone in
   mean-rating-per-level; the room is resolution, not direction.

### 4.4 Stage 1 — the corrected board (per-technique bucket, measured 2026-06-23)

The reworked [`datasets_correlate`](../generator-lab/examples/datasets_correlate.rs) (`--jobs 12`,
Pelánek `runs=30 k=25`, full groups). Per group: the **n-weighted aggregate over buckets** of our
`rating` vs human and of the Pelánek bar (`max(|refut|, |dep|)`), with the trunk share of the covered
puzzles. A flat / single-level bucket counts as `0` ranking power but is shown, never hidden;
single-level buckets (no within-bucket order to test) are excluded from the aggregate.

| group | covered | trunk / covered | our agg | Pelánek bar | gap (ours − bar) |
|---|---|---|---|---|---|
| `synnwang/solve_time` | 98% | 1448/1502 (96%) | **+0.002** | 0.584 (dep) | **−0.582** |
| `synnwang/D_TO` | 99% | 329/340 (97%) | −0.009 | 0.714 (dep) | −0.723 |
| `synnwang/D_TR` | 99% | 329/340 (97%) | −0.002 | 0.625 (dep) | −0.627 |
| `armane/org.uk` | 86% | 156/206 (76%) | −0.009 | 0.376 (dep) | −0.385 |
| `armane/sotd` | 89% | 206/321 (64%) | +0.041 | 0.385 (dep) | −0.344 |
| `sakana/nhk` | 100% | 57/99 (only trunk is testable) | +0.000 | 0.364 (dep) | −0.364 |
| `armane/extreme` | 99% | **no trunk** | **+0.105** | 0.052 (refut) | **+0.053** |

Three reads, all sharpenings of §4.3:

1. **The trunk flatness IS the score on every trunk-bearing group.** Our rating is a constant `0` in
   the trunk bucket, which is 96-97% of the continuous data and 58-76% of the easier ordinal groups,
   so the n-weighted aggregate is pinned to ~0 (the per-technique buckets that *do* have signal are
   too thin to move it). This is §4.3.1, now quantified bucket-by-bucket. Closing the trunk (Stage 2)
   is worth essentially the entire continuous-group gap.
2. **`armane/extreme` is the control: no trunk bucket → we have signal, and there we beat the bar.**
   Every `extreme` puzzle needs at least a hidden-pair, so it has no flat-trunk mass; our within-bucket
   rating reaches **+0.105** while Pelánek's refutation/dependency are weak in that hard, narrow range
   (bar 0.052). The dropped cross-technique number (§4.2) hid this entirely — the grader is not
   uniformly worse than Pelánek, it is worse *exactly where it is flat*.
3. **Pelánek's bar is carried by Dependency in the trunk** (|0.45-0.72|): precisely the singles-only
   resolution our trunk bucket lacks, and precisely what Stage 2 must reproduce off the trace.

The non-trunk per-technique buckets are thin (most `n < 25`) and span a narrow human-label range, so
their per-bucket rho is noisy and many are single-level (reported `n/a`) — the restricted-range
deflation of §7. The board is reproducible instantly from the cache; it is the baseline Stages 2-4
are judged against.

## 5. The plan (per-spec; beat the bar; one change at a time)

**Stage 1 — the corrected scoreboard (per-technique bucket). DONE (2026-06-23) — see §4.4.**
Reworked `datasets_correlate` to bucket each group's covered puzzles by inferred `key` (incl. the
trunk bucket) and report, **per bucket**: n + coverage, our `rating`-vs-human rank correlation, and
Pelánek-vs-human on the same bucket (the bar), aggregated per group (n-weighted over buckets, a
flat/undefined bucket = `0` ranking power with its n still counted), never pooled across techniques.
This is the board every later stage is judged on. The cross-technique global baseline is dropped.
Continuous groups use weighted Spearman, ordinal use Kendall tau-b; n is reported per bucket (thin
buckets stated, not hidden; single-level buckets — no within-bucket order — shown as `n/a` and
excluded from the aggregate). Pelánek is mined once into a per-puzzle cache (`--cache`).

    cargo run --release -p generator-lab --example datasets_correlate -- --jobs 12 [--no-pelanek]

**Stage 2 — close the trunk gap (highest leverage).** Give the **trunk node** a real within-spec
sub-order so it is not flat `0`. The signal must vary when no harder kind fires — candidates already in
the trace: singles/LC **fill-path depth** (how far the cheap closure went, where it stalled and
resumed), the LC firing count, the candidate-population profile. (Pelánek's *Dependency* is the
human-validated proof such a signal exists; we build our own off the trace rather than tune to it.)
Measure against the trunk-bucket human label — `synnwang` is mostly trunk, so the data is dense here.
One change, measured against Stage 1.

**Stage 3 — adjudicate signal orientation with the human reference.** In the **dense** technique
buckets only, recompute each signal's sign from `rank_corr(signal, human_label)` instead of
`rank_corr(signal, grade_batch)`. First settle the known `alts` conflict (§1): does the human
reference orient it `+1` or `−1`? Re-bake [`TechNorm::calibrate`](../generator-lab/src/grade.rs)'s
`rel` from human labels where the bucket supports it; keep `grade_batch` as the fallback `rel` for
thin buckets (most will be thin — Stage 0 confirms the data concentrates in trunk + a few kinds).
Check the within-technique split-half stability does not regress (the C/D/E gains are kept; this
re-orients, it does not re-weight).

**Stage 4 — natural-puzzle re-mine + optional per-technique re-weight.** Re-mine the `NORM`/`CDF` over
the gradeable dataset puzzles themselves (holdout-split per bucket), curing the §4.3.2 distribution
shift, and bake it as the spec-free path's table. Then, only where a bucket is large enough, fit
per-technique weight vectors by monotone-constrained regression against the human label,
holdout-gated (accept only weights whose held-out human rho beats the per-bucket baseline — small
buckets overfit instantly). Heaviest step; may simply lack the data in most buckets — do not force it.

Re-use the resumable [`grade_diag`](../generator-lab/examples/grade_diag.rs) mining-cache machinery so
the (slow, especially for Pelánek) dataset solves are computed once and recalibration is instant.

## 6. Acceptance criteria

1. **Coverage reported, tuning on the solvable subset only**, the uncovered tail logged per group.
2. **Per-technique-bucket human correlation improves on the Stage-0 baseline** and **closes the gap to
   Pelánek** (the §2.2 bar), reported per group and aggregated — never pooled across techniques.
3. **The trunk bucket is no longer flat:** it has resolution and a positive human correlation (the
   §4.3.1 fix).
4. **Held-out:** every fitted number (signs, any weights, the re-mined `NORM`/`CDF`) is validated on a
   bucket half it was not fit on; report held-out, not in-sample, numbers.
5. **No regression of the internal grade:** within-technique split-half stability (≥ 0.90) and the
   C/D/E gains survive — the human anchor sits on top of the within-technique rating.
6. **No cross-technique pooling and no global number** — per-bucket metrics, aggregated by averaging.

## 7. Risks and honest limits

- **The toolbox caps *our* coverage — but far less than feared (86-100%, §4.1).** Only the top ordinal
  tier loses coverage; the rest is easy-to-medium. The real limit is the *opposite*: the gradeable
  corpus is concentrated in the easy range, where the grader has the least resolution (§4.3.1). Pelánek
  has no such gap, which is part of why it is the bar. State the gradeable range with every number.
- **Thin within-technique buckets.** Most of the data is trunk + a few kinds, so most per-technique
  buckets are thin. Hence the `grade_batch` fallback and the holdout gates. Do not manufacture
  per-technique fits the data cannot support; the trunk bucket (Stage 2) and the few dense kinds are
  where the real signal is.
- **Restricted range deflates correlation within a bucket.** A single technique's puzzles span a
  narrow difficulty range, so within-bucket rho reads lower than a full-range number. Report n and
  coverage alongside every rho.
- **The human label is itself noisy / population-specific.** `synnwang` is one app's player base;
  `armane` is per-site editorial labels. Treat each as one partial order, weight by confidence where
  it exists (`player_count`), and never collapse them into a single ground truth.
- **Pelánek is expensive.** It is randomized rollouts (30 runs, refutation per stuck cell), so the
  hard groups cost orders of magnitude more than our toolbox solve. Keep the `--limit`/`--runs` knobs
  and the mine-once cache if Pelánek ever enters a tuning loop (it is the bar, so it is graded once
  and cached, not re-run per change).

## 8. Relation to other docs and memory

- [`grader-continuous-scoring.md`](grader-continuous-scoring.md) — the within-technique Tiers C/D/E
  this plan anchors externally. **Its §5.2 optional global number C2 is now explicitly dropped** (§2
  scope decision); the rest (the per-technique rating) is the backbone this plan improves.
- [`grader-granular-scoring.md`](grader-granular-scoring.md) — the within-technique sub-tier cut; the
  standing decision that the UI badge stays a per-spec sub-tier read holds (and there is now no
  competing global read).
- [`campaign-grader-plan.md`](campaign-grader-plan.md) — the relative `grade_batch` grader, i.e. the
  *self-reference* this plan replaces as the calibration target (kept only as the thin-bucket fallback).
- [`pelanek-2014-sisus-refutation-dependency.md`](pelanek-2014-sisus-refutation-dependency.md) +
  [`pelanek.rs`](../generator-lab/src/pelanek.rs) — the **bar to beat**: a search-based, human-validated
  computed metric. We measure ourselves against it on the human data; we never tune to it.
- [`datasets/README.md`](../datasets/README.md) + `datasets/normalized/catalog.json` — the data this
  plan consumes (read `normalized/` only, never the raw sources).
