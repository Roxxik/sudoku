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

> Status: **Stages 0-4 BUILT & MEASURED (2026-06-23), incl. the Stage-4 trunk-frontier-averaging
> refinement (§4.8).** Landed: the
> spec-free grading path [`grade_puzzle`](../generator-lab/src/grade.rs) (G1, Stage 0); the
> reworked [`datasets_correlate`](../generator-lab/examples/datasets_correlate.rs) harness — Stage 1,
> the **corrected per-technique-bucket scoreboard** (§4.4): it buckets each group's covered puzzles by
> inferred bottleneck (incl. the trunk bucket), reports our `rating` vs human and Pelánek vs human
> PER BUCKET, and aggregates n-weighted within a group, **never pooled across techniques**. Pelánek's
> per-puzzle metrics are cached (`--cache`, keyed by the puzzle, invalidated only by `--runs`/`--k`/
> `--model-seed`) so re-runs are instant; `--no-pelanek` skips the bar for fast iteration. And **Stage
> 2 — the trunk frontier rating** ([`trunk_profile`](../generator-lab/src/solve/logic.rs) +
> [`trunk_rating`](../generator-lab/src/grade.rs), §4.5): the trunk bucket is no longer a flat `0` —
> it is graded on the singles + locked-candidate fill-path frontier (our own deterministic analogue of
> Pelánek's Dependency, built off the trace, not tuned to it). The headline findings (§4): **coverage
> is high; the trunk was flat `0` and held most of the data, pinning every trunk-bearing group's
> aggregate to ~0; Stage 2 closes that — the trunk now correlates +0.34..+0.57 with humans, lifting
> the group aggregates to +0.34..+0.55 and closing 70-95% of the gap to Pelánek (the ordinal groups
> now match/beat the bar), exactly as §4.3.1 predicted.** And **Stage 3 — human sign adjudication**
> ([`human_signal_orientation`](../generator-lab/src/grade.rs) + [`TechNorm::reoriented`](../generator-lab/src/grade.rs),
> driven by [`grade_diag --human-orient`](../generator-lab/examples/grade_diag.rs), §4.6): in the
> dense non-trunk buckets it re-derives each live signal's **sign** from the human label instead of
> `grade_batch`, gated so a flip that regresses split-half stability is reverted. Result: the **`alts`
> conflict (§1) is settled `+1`** (the human reference never orients it `−1`), the two largest dense
> buckets (hidden-pair, swordfish) need no flip (the existing signs were right), and the one
> human-improving stability-safe flip — **xy-wing `cascade` `−1→+1`** — lifts its pooled human-rho
> +0.080→+0.179; naked-pair's and w-wing's bigger raw wins are stability-rejected. Surgical bake (signs
> + two CDFs only; trunk and every other row byte-identical). And **Stage 4** — the natural-puzzle
> NORM/CDF re-mine (§4.7, spec-free `NATURAL_*` overlay) **plus the trunk-frontier averaging** (§4.8,
> [`trunk_profiles_rand`](../generator-lab/src/solve/logic.rs) + [`trunk_rating_runs`](../generator-lab/src/grade.rs)):
> the trunk frontier is now averaged over 30 randomized fill orders (Pelánek's Dependency averaging,
> off our own trace), closing the determinism-vs-averaging gap so **every trunk bucket matches/beats
> the Pelánek Dependency bar** and the continuous-group aggregates land within ±0.03 of it (from
> −0.11..−0.17 at Stage 2); scoped to the frontier term, seeded from the puzzle, spec-based production
> byte-identical.

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

### 4.5 Stage 2 — the trunk frontier rating (measured 2026-06-23)

The trunk bucket is no longer flat. [`trunk_profile`](../generator-lab/src/solve/logic.rs) runs one
deterministic easiest-first singles + locked-candidate fill and records, at every step, the **frontier
width** — how many cells a naked or hidden single forces at once (deduped by cell). The mean of that
over the first `k = 25` steps is our [`trunk_dependency`](../generator-lab/src/grade.rs): the same
quantity Pelánek's *Dependency* averages (a narrow, sequential forced chain = harder), but read off
our own fill rather than 30 randomized rollouts, so it is **built off the trace, never tuned to the
bar** (§2.2). [`trunk_rating`](../generator-lab/src/grade.rs) maps it (oriented harder = higher) plus
a locked-candidate-stall term through a logistic to a continuous `(0, 1)`. The within-bucket rank — all
the harness reads — is a monotone reparam of `−dependency` for the pure-singles majority, so the
logistic constants are presentation only; the LC term only lifts the puzzles that needed a locked
candidate above the singles ones.

| group | trunk: Stage 1 → Stage 2 | trunk bar (Pelánek dep) | group agg: Stage 1 → Stage 2 | bar | gap (ours − bar) |
|---|---|---|---|---|---|
| `synnwang/solve_time` | flat → **+0.485** | −0.591 | +0.002 → **+0.470** | 0.584 | −0.582 → **−0.114** |
| `synnwang/D_TO` | flat → +0.570 | −0.716 | −0.009 → +0.546 | 0.714 | −0.723 → −0.168 |
| `synnwang/D_TR` | flat → +0.530 | −0.633 | −0.002 → +0.514 | 0.625 | −0.627 → −0.111 |
| `armane/org.uk` | flat → +0.463 | −0.452 | −0.009 → +0.349 | 0.376 | −0.385 → **−0.027** |
| `armane/sotd` | flat → +0.558 | −0.557 | +0.041 → +0.406 | 0.385 | −0.344 → **+0.021** (beats bar) |
| `sakana/nhk` | flat → +0.343 | −0.364 | +0.000 → +0.343 | 0.364 | −0.364 → −0.022 |
| `armane/extreme` | (no trunk) | — | +0.105 (unchanged) | 0.052 | +0.053 |

Three reads:

1. **The trunk now carries the groups it used to pin to zero.** The trunk-bucket correlation jumped
   from flat to +0.34..+0.57, lifting every trunk-bearing group's aggregate to +0.34..+0.55 and
   closing **70-95% of the gap to Pelánek**. `armane/sotd` and `org.uk` now match or beat the bar.
   This is §4.3.1 ("closing the trunk is worth essentially the entire continuous-group gap") realised.
2. **We trail the bar exactly by the determinism-vs-averaging margin.** Our single deterministic fill
   path reaches ~80-95% of Pelánek's Dependency magnitude on the continuous trunks; the residual is
   that Pelánek averages the frontier over 30 randomized fill orders while we read one. Recovering it
   (a few-run frontier average) is a natural Stage-4 refinement, not a re-tuning. **DONE — §4.8: the
   30-run average closes essentially the entire residual; we now match or beat the Dependency bar on
   every trunk bucket.**
3. **The locked-candidate term earns its place on the ordinal groups.** Ablating it (`TRUNK_LC_WEIGHT
   = 0`) costs the editorially-labelled ordinals (org.uk +0.463 → +0.395, sotd +0.558 → +0.530, nhk
   +0.343 → +0.242) — where "needs a locked candidate" is part of the site's difficulty rank — and is
   neutral on `synnwang` (±0.003), whose trunk is almost all pure singles. It never hurts, so it stays.

The per-technique (non-trunk) buckets are unchanged — Stage 2 touches only `grade_puzzle`'s trunk
branch; the spec-based production path (`grade_one`/`rating`) is byte-identical.

### 4.6 Stage 3 — human sign adjudication (measured 2026-06-23)

Stage 3 re-derives, in the **dense** non-trunk buckets only (pooled rankable n ≥
[`SIGN_DENSE_MIN`](../generator-lab/src/grade.rs) = 40 — hidden-pair 167, swordfish 115, w-wing 67,
naked-pair 59, naked-triple 57, xy-wing 44), each **live** granular signal's **sign** from the human
label instead of `grade_batch`. [`human_signal_orientation`](../generator-lab/src/grade.rs) pools the
per-(group × bucket) rank correlation n-weighted — never the raw labels (the §2.3 rule) — and
[`TechNorm::reoriented`](../generator-lab/src/grade.rs) flips a sign only where the human reference
*determines* it (`|corr| ≥ CORR_FLOOR`). Mean/scale/weight are untouched (re-orient, not re-weight;
the §4.3.2 re-mine is Stage 4). Workbench: [`grade_diag --human-orient`](../generator-lab/examples/grade_diag.rs).

Three findings:

1. **The `alts` conflict (§1) is settled in favour of `+1`.** Where `alts` is live — naked-pair
   (`+0.038`), naked-triple (`+0.239`), xy-wing (`−0.123`) — the human reference either **agrees**
   with the grade_batch `+1` (naked-triple) or is **undetermined** (below the floor; naked-pair,
   xy-wing). It **never** orients `alts` to `−1`. The "intuitive −1" the code flagged is not what
   humans see — the self-reference happened to be right here.
2. **The two largest dense buckets need no flip.** hidden-pair (n=167) and swordfish (n=115) — the
   bulk of the non-trunk dataset mass — have every live signal either agree with grade_batch or fall
   below the floor, so their orientation is **unchanged** (human-rho before == after: +0.234, +0.278).
   The biggest buckets *validate* the existing signs.
3. **The biggest raw wins are stability-rejected; the gate enforces §6.5.** The human label wants to
   flip naked-pair (`open`/`open_mean` −1→+1, human-rho +0.106→**+0.291**) and w-wing (`cascade`
   −1→+1, +0.009→**+0.196**) — but each drops the within-technique split-half stability below the 0.90
   floor (naked-pair 0.94→0.86, w-wing 0.92→0.78), so the **stability gate reverts both**. The flips
   that survive are **xy-wing** `cascade` −1→+1 (stable 1.00; human-rho +0.080→**+0.179** pooled — on
   the board solve_time +0.055→+0.192 at n=23, D_TO/D_TR −0.300→−0.100, org.uk +0.301→+0.241 the
   minority direction) and **naked-triple** `tight`/`depth_tight` +1→−1 (stable 0.98; neutral,
   +0.220→+0.222).

And **Stage 4 — the natural-puzzle re-mine** ([`NATURAL_NORM`/`NATURAL_CDF`](../generator-lab/src/grade.rs),
driven by [`datasets_correlate --natural-remine`](../generator-lab/examples/datasets_correlate.rs),
§4.7): the **spec-free** path ([`grade_puzzle`](../generator-lab/src/grade.rs)) gets its **own** tables,
re-mined over the gradeable dataset puzzles, gated by a held-out human correlation — leaving the
curriculum `GRANULAR_*` tables (the production `grade_one`/`rating` path) **byte-identical**. Two
buckets clear the 2-fold held-out gate and are baked: **swordfish** `cdf` (+0.095 → +0.166 held-out —
the §4.3.2 clamping made concrete: natural swordfish scores span [0.5, 10.7] vs the isolated CDF's
[0.1, 2.8], so 63% of `armane/extreme` swordfish pinned at rating `1.0`; the re-mined CDF restores
their order) and **naked-pair** `normcdf` (+0.043 → +0.067 — NORM mean/scale re-centered, e.g. `alts`
mean 2.18 → 14.0). Headline: **`armane/extreme` +0.105 → +0.133** (the no-trunk control, where the
distribution shift bit hardest; swordfish bucket +0.086 → +0.159, now beating the Pelánek bar by
+0.081 vs +0.053), every other group flat (trunk-dominated or thin). The **optional per-technique
re-weight is attempted but NOT baked**: under repeated 6-partition CV + split-half stability + a
non-degeneracy guard, every apparent gain is either split-luck (naked-pair/naked-triple: positive
mean, negative worst-partition) or a degenerate single-signal collapse (xy-wing → `cascade`-only,
zeroing `alts` — a Stage-3 orientation, not a re-weight), so per "do not force it" the data is too
thin to re-weight. Production untouched.

The Stage-3 bake is **surgical** — the committed `GRANULAR_NORM` with exactly those three sign flips and the
two affected CDFs re-derived under them (`grade_diag --human-orient` prints the paste-ready rows);
every other norm/CDF row, the trunk, and the whole spec-based production path are **byte-identical**.
Net on the production scoreboard the continuous groups tick up (solve_time +0.470→+0.472, D_TO
+0.546→+0.549, D_TR +0.514→+0.517), org.uk eases −0.004 (the xy-wing minority), the rest flat;
acceptance keeps every technique's split-half stability ≥ 0.90 (criterion §6.5). A small, honest,
stability-safe gain — Stage 3's value is *removing the §1 self-reference where the data is dense
enough to trust*, and proving (alts, hidden-pair, swordfish) that the existing orientation was mostly
already right.

### 4.7 Stage 4 — natural-puzzle re-mine + (rejected) re-weight (measured 2026-06-23)

Stage 4 acts on the **G1 distribution shift** (§4.3.2): the baked `GRANULAR_*` tables were mined on
the *isolated* curriculum corpus (specs that force one technique), so a **natural** dataset puzzle
solved with the full toolbox lands at a different signal/score distribution at the same inferred
bottleneck. The architecture exploits a clean split: the spec-based production path
([`grade_one`/`rating`](../generator-lab/src/grade.rs), the web app's `gen_worker`) grades exactly
those isolated-spec puzzles, while the spec-free path ([`grade_puzzle`](../generator-lab/src/grade.rs))
grades natural puzzles. So Stage 4 gives the spec-free path its **own** tables
([`NATURAL_NORM`/`NATURAL_CDF`](../generator-lab/src/grade.rs), an overlay on `GRANULAR_*` that only
touches the re-mined rows), and the production path stays **byte-identical** (criterion §6.5, free).

**The NORM/CDF re-mine — baked.** [`datasets_correlate --natural-remine`](../generator-lab/examples/datasets_correlate.rs)
buckets every group's covered, keyed puzzles by inferred technique, and for each bucket dense enough
(`>= SIGN_DENSE_MIN` natural puzzles) runs a **2-fold held-out** comparison of the isolated tables vs
two re-mine candidates — `cdf` (re-percentile the CDF over the natural scores, un-clamping) and
`normcdf` ([`TechNorm::remeaned`](../generator-lab/src/grade.rs) re-centers each signal's logistic on
the natural distribution *and* re-CDFs under it). A candidate is baked only if it beats the isolated
table on a bucket half it was **not** fit on (criterion §6.4). Two buckets clear it:

| bucket | candidate | held-out (isolated → re-mine) | what it fixes |
|---|---|---|---|
| `swordfish` | `cdf` | +0.095 → **+0.166** | natural scores span [0.5, 10.7] vs isolated CDF [0.1, 2.8] → 63% of `armane/extreme` clamped at `1.0`; re-CDF restores their order |
| `naked-pair` | `normcdf` | +0.043 → **+0.067** | NORM mean/scale re-centered (`alts` mean 2.18 → 14.0 — natural naked-pairs sit in a busier stall) |

`hidden-pair` (n=192), `naked-triple`, `xy-wing`, `w-wing` show **no** held-out gain and keep the
isolated row (the re-mine neither helps nor is forced). On the board (no-Pelánek aggregates):

| group | S3 agg | S4 agg | Δ | note |
|---|---|---|---|---|
| `armane/extreme` | +0.105 | **+0.133** | **+0.028** | the no-trunk control — swordfish bucket +0.086 → +0.159, now `ours-bar` +0.081 (was +0.053) |
| `synnwang/solve_time` | +0.472 | +0.473 | +0.001 | trunk-dominated; naked-pair bucket thin |
| `armane/org.uk` | +0.345 | +0.344 | −0.001 | naked-pair n=11 |
| `sotd` / `nhk` / `D_TO` / `D_TR` | — | — | 0 | trunk-dominated / thin non-trunk buckets |

This is exactly §4.3.2's prediction: the natural re-mine helps **where the distribution shift bit** —
`armane/extreme`, the one group with no flat trunk and the heaviest clamping — and is neutral
everywhere the aggregate is carried by the (Stage-2) trunk or by buckets too thin to re-mine.

**The optional per-technique re-weight — attempted, NOT baked.** The harness then fits each dense
bucket's blend **weights** against the human label (coordinate ascent on the live signals), under a
deliberately strict gate: **repeated 6-partition** 2-fold CV (a single even/odd split is high-variance
on a thin bucket — it lets a fit generalise across *that* split by luck), **plus** the §6.5 split-half
stability floor, **plus** a non-degeneracy guard (a fit may rebalance the blend but not zero the
branch's dominant designed signal — that is a Stage-3 *orientation* question, not a re-weight). The
result is a clean negative:

- `naked-pair` (+0.037 mean / **−0.037** worst) and `naked-triple` (+0.017 / **−0.040**) — **split-luck**:
  the single-split gain reverses on other partitions.
- `xy-wing` is the only robust gain (+0.094 worst-partition, stable 0.92) — but it collapses to
  **`cascade`-only, zeroing `alts`** (the wing branch's primary signal, weight 0.45). That is the same
  `cascade` the Stage-3 sign flip already human-oriented (§4.6); a weight refinement that deletes the
  technique's primary signal is a *replacement*, not a re-weight, and it moves **no** group aggregate.

So per the plan's "may simply lack the data in most buckets — **do not force it**," no weights are
baked. The data is too thin for a credible per-technique re-weight; the harness stays as the machinery
to re-attempt as the corpus grows. (A separate Stage-4 refinement noted in §4.5 — averaging the
**trunk** frontier over a few fill orders to recover the determinism-vs-averaging margin — targets the
trunk-dominated continuous groups; orthogonal to this re-mine, it is now **done and baked: §4.8**.)

### 4.8 Stage 4 (trunk) — frontier averaging over fill orders (measured 2026-06-23)

Stage 2's trunk rating read the [`trunk_dependency`](../generator-lab/src/grade.rs) frontier off **one
deterministic** easiest-first fill; Pelánek's *Dependency* averages it over ~30 *randomized* fill
orders, and §4.5 read 2 measured that our single read reaches only ~80-95% of its magnitude. The fill
order does not change *which* puzzle is harder, but a single order is a noisy estimator of the mean
frontier — so this refinement de-noises it the same way Pelánek does, **built off our own trace, never
tuned to the bar** (§2.2). [`trunk_profiles_rand`](../generator-lab/src/solve/logic.rs) runs `R`
randomized fills (a uniformly-random forced cell each step, RNG seeded from the puzzle so the rating
stays a deterministic, reproducible function of the puzzle), and
[`trunk_dependency_avg`](../generator-lab/src/grade.rs) averages the frontier **per step index** over
the runs (every run places one cell per step in the same total, so the curve is well-defined) before
the mean over the first `k = 25` steps. The change is **scoped to the frontier term only**: the
locked-candidate stall term is read from the deterministic profile (Pelánek has no LC analogue), so
`R = 0` reproduces Stage 2 exactly. Driven by
[`datasets_correlate --trunk-average`](../generator-lab/examples/datasets_correlate.rs) (our grader
only, no Pelánek), which A/Bs the trunk bucket deterministic vs averaged at `R ∈ {2,4,8,16,30}`.

The gain is **monotone in `R`, saturating by `R ≈ 16`**, and **regresses no group**. Baked at
`R = TRUNK_DEP_RUNS = 30` (Pelánek's default) as the spec-free trunk default
([`trunk_rating_runs`](../generator-lab/src/grade.rs), called by
[`grade_puzzle`](../generator-lab/src/grade.rs)'s trunk branch). Trunk bucket and group aggregate,
deterministic (Stage 2) → averaged (`R = 30`), against the Pelánek Dependency bar:

| group | trunk: det → R30 | trunk bar (|pel dep|) | group agg: det → R30 | bar | gap (ours − bar) |
|---|---|---|---|---|---|
| `synnwang/solve_time` | +0.485 → **+0.588** | 0.591 | +0.473 → **+0.572** | 0.584 | −0.111 → **−0.012** |
| `synnwang/D_TO` | +0.570 → **+0.711** | 0.716 | +0.549 → **+0.686** | 0.714 | −0.165 → **−0.027** |
| `synnwang/D_TR` | +0.530 → **+0.634** | 0.633 | +0.517 → **+0.618** | 0.625 | −0.108 → **−0.008** |
| `armane/org.uk` | +0.463 → **+0.484** | 0.452 | +0.344 → +0.360 | 0.376 | −0.032 → **−0.016** |
| `armane/sotd` | +0.558 → **+0.567** | 0.557 | +0.406 → +0.411 | 0.385 | +0.021 → **+0.026** (beats bar) |
| `sakana/nhk` | +0.343 → **+0.392** | 0.364 | +0.343 → +0.392 | 0.364 | −0.021 → **+0.027** (beats bar) |
| `armane/extreme` | (no trunk) | — | +0.133 (unchanged) | 0.052 | +0.081 |

Two reads:

1. **The averaging closes essentially the entire residual determinism-vs-averaging gap.** Every trunk
   bucket now **matches or beats** the Pelánek Dependency bar (`D_TR`, `org.uk`, `sotd`, `nhk` beat it;
   `solve_time`/`D_TO` land within ±0.005 of it). The trunk-dominated continuous-group aggregates,
   which trailed the bar by −0.11..−0.17 at Stage 2, now sit within **±0.03** of it — the §4.5 read 2
   prediction realised in full. This is the single largest remaining lift in the plan, and it needed no
   new constants (the logistic center/scale/LC weight are untouched), only a less-biased estimator of
   the *same* frontier quantity.
2. **Production is untouched and the change is reproducible.** Like the rest of Stage 4 it lives on the
   spec-free path only ([`grade_puzzle`](../generator-lab/src/grade.rs), example-only); the spec-based
   `grade_one`/`rating` path the web app's `gen_worker` calls is byte-identical. The per-puzzle RNG seed
   ([`trunk_seed`](../generator-lab/src/grade.rs), FNV-1a of the grid) makes the averaged rating a pure,
   reproducible function of the puzzle — the same property the deterministic fill had.

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

**Stage 2 — close the trunk gap (highest leverage). DONE (2026-06-23) — see §4.5.** Gave the **trunk
bucket** a real within-bucket sub-order off the singles + locked-candidate fill: the frontier-width
mean ([`trunk_dependency`](../generator-lab/src/grade.rs), our deterministic analogue of Pelánek's
Dependency — fewer simultaneous forced cells = a narrower, harder chain) plus a locked-candidate-stall
term, mapped harder = higher through a logistic to `(0, 1)` ([`trunk_rating`](../generator-lab/src/grade.rs)).
Built off the trace,
never tuned to the bar (§2.2). Result: the trunk went from flat to +0.34..+0.57 vs the human label,
closing 70-95% of the gap to Pelánek and matching/beating the bar on the ordinal groups; the LC term
is ablation-justified (it carries the ordinals, is neutral on `synnwang`). The change is confined to
`grade_puzzle`'s trunk branch — the spec-based production path is untouched.

**Stage 3 — adjudicate signal orientation with the human reference. DONE (2026-06-23) — see §4.6.**
In the **dense** technique buckets only (pooled rankable n ≥ [`SIGN_DENSE_MIN`](../generator-lab/src/grade.rs)),
re-derived each **live** signal's sign from `rank_corr(signal, human_label)` instead of
`rank_corr(signal, grade_batch)`: [`human_signal_orientation`](../generator-lab/src/grade.rs) pools the
per-(group × bucket) correlation n-weighted (never the raw labels), and
[`TechNorm::reoriented`](../generator-lab/src/grade.rs) flips a sign where the human reference clears
the floor; mean/scale/weight stay (re-orient, not re-weight). A **stability gate** reverts any flip
that drops the within-technique split-half stability below 0.90 (criterion §6.5), so the C/D/E gains
survive. Thin buckets keep the `grade_batch` fallback. Outcome: the **`alts` conflict (§1) is settled
`+1`** (the human reference never orients it `−1`); the two largest dense buckets (hidden-pair,
swordfish) need no flip; the one stability-safe human-improving flip — **xy-wing `cascade` −1→+1**
(pooled human-rho +0.080→+0.179) — and the neutral **naked-triple `tight`/`depth_tight` +1→−1** are
applied; naked-pair's and w-wing's bigger raw wins are stability-rejected. Surgical bake
(`grade_diag --human-orient`): the committed `GRANULAR_NORM` + those three sign flips + the two
re-derived CDFs, everything else byte-identical. The `--data` datasets are loaded through the shared
[`datasets`](../generator-lab/src/datasets.rs) module the Stage-1 scoreboard also uses.

**Stage 4 — natural-puzzle re-mine + optional per-technique re-weight. DONE (2026-06-23) — see §4.7.**
Re-mined the `NORM`/`CDF` over the gradeable dataset puzzles and baked them as the **spec-free path's
own** tables ([`NATURAL_NORM`/`NATURAL_CDF`](../generator-lab/src/grade.rs)), an overlay on `GRANULAR_*`
that touches only the rows whose natural re-mine beat the isolated table on a **2-fold held-out** human
correlation — so the spec-based production path is byte-identical. Two buckets cleared the gate
(swordfish `cdf`, the §4.3.2 un-clamp; naked-pair `normcdf`), lifting `armane/extreme` +0.105 → +0.133
(swordfish bucket +0.086 → +0.159) and neutral elsewhere (trunk-dominated / thin). The **optional
re-weight is attempted but NOT baked**: under repeated 6-partition CV + split-half stability + a
non-degeneracy guard, every gain is split-luck or a degenerate single-signal collapse (xy-wing →
`cascade`-only), so per "do not force it" the data is too thin to re-weight. Driven by
[`datasets_correlate --natural-remine`](../generator-lab/examples/datasets_correlate.rs) (our grader
only; no Pelánek needed), which prints the paste-ready `NATURAL_*` arrays and the re-weight verdicts.

**Stage 4 (trunk) — frontier averaging over fill orders. DONE (2026-06-23) — see §4.8.** Replaced the
trunk bucket's single deterministic frontier read with the mean over `R = 30` randomized fill orders
(Pelánek's Dependency averaging, off our own trace), scoped to the frontier term only (the LC term and
all logistic constants are unchanged, so `R = 0` is Stage 2 exactly). Seeded from the puzzle, so the
rating stays a reproducible pure function of it. Result: every trunk bucket now matches/beats the
Pelánek Dependency bar and the trunk-dominated continuous-group aggregates close to within ±0.03 of it
(from −0.11..−0.17 at Stage 2). Baked as `grade_puzzle`'s spec-free trunk default
([`trunk_rating_runs`](../generator-lab/src/grade.rs)); the spec-based production path is byte-identical.
Driven by [`datasets_correlate --trunk-average`](../generator-lab/examples/datasets_correlate.rs).

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
