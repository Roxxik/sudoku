# Grader — continuous difficulty rating (beyond the 3-band cut)

The next step past [`grader-granular-scoring.md`](grader-granular-scoring.md). That doc made the
**3-band** (gentle / medium / spicy) cut clean for every technique — distinct cut points, even
thirds, and the xyz-wing degeneracy gone — by adding continuous stall signals (`open`, `cascade`,
`depth`, `alts`) and a per-branch logistic blend ([`granular_score`](../generator-lab/src/grade.rs)).
This doc asks the next question the user raised: **can we grade more finely than three bands?**
Yes — and the lever is no longer "make the bands non-degenerate" but "raise the *effective
resolution* of the per-puzzle score and give it a meaning beyond a within-technique rank."

> Status: **Tiers C + D + E SHIPPED (baked); Tier F remains spec.** The granular Tier A + Tier B
> calibration is now baked (`GRANULAR_NORM` + `GRANULAR_CDF` in [`grade.rs`](../generator-lab/src/grade.rs))
> and production `grade_one` returns the continuous Tier-C [`rating`] in `[0, 1]`; the web worker
> cuts it into **5 bands** ([`band_of_rating`], `UI_BANDS = 5`). The coarse `hardness_score`/
> `THRESHOLDS`/`band_coarse` path is kept only as the workbench's reference. Same hard constraint:
> **one instrumented cold easiest-first solve per yielded puzzle**
> ([`solve_graded`](../generator-lab/src/solve/logic.rs)), off the hot generation loop, no
> branching, no path quantification (that is the separate [`GENERATION-RULES.md`](../GENERATION-RULES.md)
> grader).
>
> **Tier C measured (150/spec, pooled):** every technique splits into **5 even fifths**
> (`[20 20 20 20 20]`), supports **10/10** effective levels, and is rank-stable (split-half
> Spearman ≥ 0.93 for 10/12; xyz-wing 0.85, w-wing 0.90 — the wing residual). Faithfulness rho ≥
> 0.85 for all subsets + fish; wings 0.67–0.81 (the relative grader is near-degenerate there, the
> known cap). Two calibration refinements landed with the bake: signal orientation stays fully
> data-driven (§8) but a signal is **dropped** (weight 0) unless its |correlation| clears a noise
> floor (`CORR_FLOOR = 0.20`) — this fixed a sign-flip instability (swordfish 0.62 → 1.00); and
> `alts` calibrates to sign +1 (more-alts ↔ tighter-firing in-corpus), not the intuitive −1, a
> reminder that the deduction-cost relative grader cannot validate spotting difficulty (Tier F2).
>
> **Tier D measured (150/spec, pooled): trajectory featurizer landed, weighted Subset-only.**
> [`signals_of`](../generator-lab/src/grade.rs) is now a featurizer — alongside the tightest-stall
> subset it accumulates 4 trajectory features over the whole bottleneck-firing sequence: D1
> `tight_integral` (`Σ 1/(1+alts)`), D2 `open_sum` (mean blended), D3 `depth_tight_integral`
> (depth-weighted tightness), D4 `payoff_transitions` (dry→payoff jaggedness). [`TechNorm`] gains
> the 4 norms; [`branch_weights`] weights them **only for `Subset`** (light: `tight` 0.10, the
> others 0.05). The empirical driver: bottleneck multi-firing rate is **30–38% (subset drills),
> 20–25% (subset train pairs)** but only **1–4% (fish)**, so trajectory ≡ tightest-stall on fish
> (their weights stay 0 → fish scores **byte-identical** to pre-D). Bivalue multi-fires 13–31% but
> its reference is near-degenerate (`elim` pinned), so trajectory weight **destabilised** it
> (split-half xyz-/w-wing 0.85/0.90 → 0.55/0.67) for no rho gain — withheld, wings also score
> byte-identical (their residual is *spotting* cost = Tier E, not deduction-cost trajectory). Net
> on the multi-firing subsets: naked-pair stable 0.93→0.95 & rho 0.93→0.94, hidden-pair rho
> 0.92→0.96; triples/quads (low multi-fire) flat; all subsets still PASS. A heavier Subset weight
> overshot (naked-pair rho dipped to 0.91), so the light split is the keeper.
>
> **Tier E measured (150/spec, pooled): E1 near-miss camouflage landed, xyz-wing-only.** After C+D
> the one technique still failing the §2.3 split-half stability gate is **xyz-wing (0.85 < 0.90)** —
> the residual, exactly as it was the gate for the 3-band cut. [`count_alternatives`](../generator-lab/src/solve/techniques.rs)
> now returns `(productive, examined)` — every `count_*` mirror tallies the structurally-valid
> *examined* instances alongside the productive ones — and the new [`camo`](../generator-lab/src/grade.rs)
> signal is `examined - alts` at the tightest stall: the pattern-shaped look-alikes the solver spots
> and discards (the spotting cost the deduction signals miss). Camo is weighted **per technique**
> ([`camo_weight`], the one Tier-F1-style override) and nonzero **only for xyz-wing** (0.40): it lifts
> its split-half stability **0.85 → 0.92** (rho 0.75 unchanged — the degenerate-reference cap, §2.4
> "do not regress" met), so **all 12 techniques now clear the §2.3 ≥ 0.90 gate**. It is withheld
> elsewhere: xy-wing (stable 1.00) and w-wing (0.90, whose camo is strongly mode-split — train avg
> 14.8 vs drill 3.7) already pass and camo only destabilises them, and the subsets/fish never needed
> it — so all of them score **byte-identical to pre-E** (camo weight 0 → dropped from the blend). The
> signal is computed toolbox-wide and cold (a free second tally in the existing enumerate scan); E2
> (firing-cell spread) and E3 (candidate-grid complexity) were not needed — xyz-wing was the only
> §2.3 failure and E1 cured it.

## 1. What "more granular" means — and what limits it today

Granularity is the number of difficulty levels a score can *reliably distinguish within one
technique*. Three things cap it today:

1. **The output is a 3-way cut.** [`band_calibrated`](../generator-lab/src/grade.rs) maps the
   continuous [`granular_score`](../generator-lab/src/grade.rs) through one `(p33, p66)` pair to
   `{0,1,2}`. The score underneath is continuous, but everything finer than thirds is discarded at
   the last step.
2. **The score is only within-technique-normalized.** [`TechNorm`](../generator-lab/src/grade.rs)
   z-scores each signal against that technique's own sample, so `granular_score` is a *relative*
   position inside a node, not a portable number. An x-wing scoring `0.6` and a jellyfish scoring
   `0.6` are not "equally hard" in any global sense — there is no cross-technique axis.
3. **Signals are reduced to one snapshot.** [`signals_of`](../generator-lab/src/grade.rs) collapses
   the whole solve to the *tightest* bottleneck (`open`/`cascade`/`alts`) and the *first* one
   (`depth`). A puzzle with three medium-hard stalls and one with a single very-tight stall can
   reduce to the same `Signals`, even though the human experience differs. The trajectory the trace
   already holds ([`GradeStep`](../generator-lab/src/solve/logic.rs) per harder step) is thrown away.

So "more granular" decomposes into three independent upgrades, shippable in order:

- **C — finer output.** Replace the 3-band cut with a continuous within-technique rating (and,
  optionally, one cross-technique-comparable number). Pure calibration + scoring; no new signals.
- **D — trajectory signals.** Aggregate over the *whole* `trace.steps` instead of one stall, so the
  score has fine structure that distinguishes multi-stall puzzles. Cheap — the data already exists.
- **E — spotting-cost signals.** Add the human "how hard is it to *find*" dimension (near-miss
  camouflage, geometric spread, candidate-grid complexity) the current deduction-cost signals miss.
  This is the lever for the wing residual. Needs new (cold) instrumentation.
- **F — calibration upgrade.** Fit the combination per technique against a reference, under
  monotonicity constraints, to convert the extra signal into faithful resolution rather than noise.

## 2. Goal and acceptance criteria

A per-puzzle score with **resolution well past three levels**, still cold and single-path. Concretely,
per technique over a mined corpus (the [`grade_diag`](../generator-lab/examples/grade_diag.rs) cache):

1. **Effective resolution ≥ R.** Define the score's *effective levels* as the number of
   percentile buckets of width `1/R` that are non-empty and separated by a real score gap (no tie
   spanning a bucket boundary). Target **R ≥ 10** for every technique — i.e. a genuine 10-step
   rating, not 3. The hard case is still the branch-pinned techniques (wings); they are the
   acceptance gate, exactly as xyz-wing was for the 3-band doc.
2. **Monotone, no cliffs.** The score is non-decreasing in each oriented signal (as today), and
   continuous except at the integer `grind` steps. A small signal change moves the score a little,
   never across a band by itself (the §5 squash invariant, preserved).
3. **Rank-stable under resampling.** Calibrate on half the corpus, score the other half: the
   *score's own* ranking is reproducible — split-half Spearman of the score against itself
   (A-calibrated vs B-calibrated, on a shared holdout) **≥ 0.9**. This is the granularity analogue
   of "distinct cut": a finer score is only worth more levels if those levels are not sampling noise.
4. **Faithful (unchanged target).** Spearman rho vs the relative [`grade_batch`](../generator-lab/src/grade.rs)
   stays **≥ 0.85** where it is today (subsets, fish) and does not regress for wings. rho — not the
   noisy 3-way band-agreement — is the metric (see the granular doc's finding).
5. **Cross-technique comparability (only if a global number ships, §5.2).** Two puzzles at the same
   within-technique percentile of harder-vs-easier techniques order by technique base difficulty;
   validated against [`grade_batch`](../generator-lab/src/grade.rs) pooled across a branch, or an
   external continuous reference (§6).

## 3. Tier C — finer output (continuous within-technique rating)

The cheapest win, and it needs no new signal. The granular score is already continuous; stop
throwing away its resolution at the cut.

### C1 — per-technique percentile rating

Replace the single `(p33, p66)` pair with a baked **CDF** per technique: a small monotone
breakpoint table (e.g. the 0/10/20/.../100 percentiles of `granular_score` over the calibration
pool, ~11 anchors). `rating_T(puzzle) = piecewise-linear-interp(granular_score, CDF_T) ∈ [0, 1]` —
the puzzle's percentile within its technique, a continuous 0–100 read. The 3-band UI label stays a
trivial derived cut (`< 0.33`, `< 0.66`) so nothing downstream breaks; callers that want finer
resolution read the percentile.

This is the natural extension of the thirds calibration already emitted by
[`grade_diag --calibrate`](../generator-lab/examples/grade_diag.rs): more anchors, same machinery,
same pooled train+drill corpus.

### C2 — optional global difficulty number

A single 0–100 across all techniques, for sorting/daily-curve uses. Anchor on the curriculum axis
already in the codebase: [`DIFFICULTY`](../generator-lab/src/spec/kinds.rs) per kind and the
[`Tier`](../generator-lab/src/spec/kinds.rs) cut points. Map

```
global = base_T + span_T · rating_T(puzzle)
```

where `base_T` / `span_T` place each technique's `[0,1]` rating into a calibrated global band so the
hardest easy-technique puzzle sits just below the easiest hard-technique puzzle (or deliberately
overlaps, a §7 decision). `base_T`/`span_T` are calibration data, fit so the global order matches a
branch-pooled [`grade_batch`](../generator-lab/src/grade.rs) (or §6's external reference). Keep it
**optional and separate** from the within-technique rating — the UI's badge is a sub-tier read and
must not silently become a global one (the user's standing decision in
[`grader-granular-scoring.md`](grader-granular-scoring.md)).

## 4. Tier D — trajectory signals (resolution from the whole solve) — SHIPPED

> **Landed (baked).** The four features below are computed by [`signals_of`](../generator-lab/src/grade.rs)
> in its existing single walk (the `Signals` struct dropped `Eq` to carry the two `f64` integrals),
> normed per technique in [`TechNorm`], and blended via [`branch_weights`] — but **Subset-only**:
> see the status block for the multi-firing-rate driver (subsets 20–38%, fish 1–4%, the
> degenerate-reference wings withheld to Tier E). D1/D2/D3 are stored as `tight_integral` /
> `open_sum` (mean taken in `raw_granular`) / `depth_tight_integral`; D4 ships as
> `payoff_transitions` (the dry→payoff transition count; `longest_dry_run` already carried the
> longest-dry-run half, and cascade-variance was left out as redundant once transitions captured
> the jaggedness).

The trace already records a [`GradeStep`](../generator-lab/src/solve/logic.rs) per harder step with
`open`/`elims`/`cascade`/`alts`/`paid_off`/`depth`. Today [`signals_of`](../generator-lab/src/grade.rs)
reduces it to point summaries. Aggregate over the full sequence instead — all cheap, no new
instrumentation, and each adds within-band structure:

- **D1 — tightness integral.** `Σ_stalls 1/(1 + alts)` (and/or `Σ 1/(1 + elims)`): total stuck-ness
  over the solve, not just the single worst stall. Distinguishes one-hard-stall from many-medium-
  stalls puzzles that share a tightest `alts`.
- **D2 — search-work integral.** `Σ_stalls open` or mean `open` across stalls — total candidate
  population the solver had to scan over the solve. Generalises the single-stall `open`.
- **D3 — late-stall weighting.** Weight each stall's tightness by its `depth` (a hard stall after 60
  placements is felt more than one at 25). A depth-weighted tightness integral.
- **D4 — payoff shape.** Sequence statistics over `cascade`/`paid_off`: number of dry→payoff
  transitions, longest dry run (already `grind`), variance of cascade sizes. A jagged stop-start
  solve reads harder than one smooth descent.

These promote `signals_of` from a reducer to a small **trajectory featurizer**; the tightest-stall
values stay as a subset (so Tier A/B behaviour is recoverable). They sharpen exactly the
multi-firing nodes (subsets, multi-wing drill specs) where the single-stall reduction is lossiest.

## 5. Tier E — spotting-cost signals (the human "find it" dimension) — E1 SHIPPED

> **E1 landed (baked), xyz-wing-only.** [`count_alternatives`](../generator-lab/src/solve/techniques.rs)
> returns `(productive, examined)` — each `count_*` mirror tallies the structurally-valid *examined*
> instances alongside the productive ones, a free second counter in the existing non-mutating scan
> ([`AltCount`]). [`GradeStep`](../generator-lab/src/solve/logic.rs) carries `examined`; the new
> [`Signals::camo`](../generator-lab/src/grade.rs) is `examined - alts` at the tightest stall (the
> dead look-alikes). It is a 10th blended signal, weighted **per technique** ([`camo_weight`]) — the
> one Tier-F1-style override — nonzero **only for xyz-wing** (0.40), the lone technique still under the
> §2.3 stability gate after C+D. Result: xyz-wing split-half **0.85 → 0.92**, so all 12 techniques
> clear ≥ 0.90; xy-/w-wing (already ≥ 0.90, and destabilised by camo) and the subsets/fish keep camo 0
> and score byte-identical to pre-E. rho is unchanged everywhere (wings stay at the degenerate-reference
> cap, §2.4 met). E2/E3 below were **not needed** — xyz-wing was the only §2.3 failure and E1 cured it.

The signals so far measure *deduction* cost (how much the firing eliminates, how stuck the board is).
Human difficulty is dominated by *search* cost — how hard the pattern is to **locate**. `alts` is a
first proxy (fewer productive patterns = a needle hunt). Finer spotting signals, all cold:

- **E1 — near-miss camouflage. SHIPPED.** Extend the enumerate scan
  ([`count_alternatives`](../generator-lab/src/solve/techniques.rs)) to also count *plausible-but-dead*
  configurations the solver must examine and reject: bivalue cells sharing a wing's digits that do
  not complete a link; subset-shaped cell groups whose union is one digit too large; fish base-line
  sets that miss the cover by one. High camouflage (many look-alikes per real pattern) = harder to
  find. This is the strongest candidate for the **wing residual** (rho 0.66–0.81 today), where the
  deduction signals are pinned but the spotting difficulty genuinely varies. Implementable as a
  second tally in the existing `count_*` mirror functions (count examined, not just productive),
  staying non-mutating.
- **E2 — pattern locality / spread.** The geometric footprint of the firing pattern: are the wing
  pivot + wings in one box/line (easy to scan together) or scattered across the grid (hard)? Cell-
  index spread or distinct-unit count of the tightest firing's cells. Cheap to record alongside the
  firing in [`solve_graded`](../generator-lab/src/solve/logic.rs).
- **E3 — candidate-grid complexity.** Beyond total `open`: the count of bivalue cells, the average
  candidates per empty cell, the number of cells at the firing's digits. Shape of the search space,
  not just its size.

E1 is the principled cure for the wings the same way S7 (`alts`) was for the fish; E2/E3 are cheap
refinements. All keep the cold, non-mutating, single-solve contract.

## 6. Tier F — calibration upgrade (turn signals into faithful resolution)

More signals only help if combined well. The current blend is a hand-weighted per-*branch* logistic
average ([`branch_weights`](../generator-lab/src/grade.rs)); calibration only sets each signal's
mean/scale/sign. To extract resolution:

- **F1 — per-technique weights.** Drop from per-branch to per-technique weight vectors, fit so the
  score best reproduces a reference *ordering* (the relative [`grade_batch`](../generator-lab/src/grade.rs)
  rank, and/or §6's external reference). The granular doc already found one such override by hand
  (Fish wants elim/scarcity-led, not the spec's open+alts-led); F1 systematises that. Fit by
  **monotone-constrained** least squares / isotonic regression so signal orientation can't invert
  and §2.2 (monotonicity) is structural. **Holdout-validate** — per-technique weights overfit
  ~150-puzzle samples easily; accept only weights whose holdout rho beats the per-branch baseline.
- **F2 — external reference (the real fix). NOW CONCRETE — see
  [`grader-external-calibration.md`](grader-external-calibration.md).** Calibrate against a continuous
  human-difficulty reference, anchoring the per-spec gradings on humans instead of self-consistency
  with `grade_batch`. This is the only part that reaches outside the repo's own signals. The human data
  has since landed in [`datasets/normalized/`](../datasets/README.md) (solve times, completion rates,
  human-set labels), so F2 is no longer hypothetical: its plan — spec-free grading, per-technique-bucket
  human correlation with Pelánek as the bar to beat, the staging and acceptance — is broken out into its
  own doc. (That doc's 2026-06-23 scope decision **drops the §5.2 global number**: the grading stays
  per-spec.) Without it the grader is internally consistent but its *absolute* scale is conventional.

Keep the `grind` integer backbone and the `(0,1)` squash throughout — F changes how the
sub-order is blended, never that `grind` dominates where it varies.

## 7. Instrumentation & calibration summary

| where | change |
|---|---|
| [`GradeStep`](../generator-lab/src/solve/logic.rs) | E2 spread + the firing's cell footprint; (open/elims/cascade/alts/depth already present) |
| [`count_alternatives`](../generator-lab/src/solve/techniques.rs) | E1: a second non-mutating tally of examined-but-dead near-misses per technique |
| [`signals_of`](../generator-lab/src/grade.rs) | D: promote to a trajectory featurizer (integrals + sequence stats), keep the tightest-stall subset |
| [`Signals`](../generator-lab/src/grade.rs) / [`TechNorm`](../generator-lab/src/grade.rs) | the new D/E signal fields + their per-signal norms |
| scoring | C1 percentile CDF rating; C2 optional global `base_T`/`span_T`; F1 per-technique weights |
| [`grade_diag`](../generator-lab/examples/grade_diag.rs) | emit the CDF/global tables; add the §2 acceptance checks (effective-levels R, split-half stability, rho); the cache + workbench already exist |

## 8. Staging — ship and measure in order

1. **Tier C first (output only).** It is pure scoring/calibration over the *existing* signals, so it
   measures the resolution ceiling of what we already have. If C alone clears R ≥ 10 for most
   techniques, the finer signals are only needed for the residual.
2. **Tier D (trajectory). DONE.** Cheap, no new instrumentation; measured the resolution lift on
   multi-stall nodes — a real gain on the multi-firing subsets (Subset-weighted), neutral by
   construction on the single-firing fish, and withheld from the degenerate-reference wings (whose
   residual is spotting cost, not deduction-cost trajectory). See the status block.
3. **Tier E (spotting cost), residual-targeted. DONE (E1).** Built E1 camouflage for the one
   technique still failing the §2.3 split-half gate after C+D — **xyz-wing** (the §2.1 R ≥ 10 and
   even5 checks were already met everywhere; the residual was *stability*, the guard that those
   levels are real not noise). Camo, weighted xyz-wing-only, lifts its split-half 0.85 → 0.92; the
   other wings already passed and are left byte-identical. E2/E3 unbuilt (not needed). See §5.
4. **Tier F (calibration).** Last, once the signal set is fixed; it tunes, it doesn't add.

Measure each in isolation against §2 before stacking (the project's one-change-at-a-time
discipline), re-using the resumable [`grade_diag`](../generator-lab/examples/grade_diag.rs) cache so
no re-mining is needed unless the generator or the grading solve changes.

## 9. Open decisions

- **R, the resolution target.** 10 levels is a placeholder; the right number is "as fine as the UI
  will display and the corpus can support." A wing technique may top out below 10 even with E1 —
  accept its ceiling rather than manufacture noise (§2.3 is the guard).
- **Global number: overlap or strict tiers?** Whether technique bands abut or overlap on the global
  axis is an empirical/curriculum question (a hard x-wing may genuinely out-rank an easy jellyfish).
  Fix from the branch-pooled reference, do not assume.
- **External reference (F2).** Worth the dependency for a true absolute scale, or is internal
  consistency with `grade_batch` enough for the UI's purposes? Decides whether §5.2/§2.5 are real.
- **Cost ceiling.** E1's near-miss enumeration is heavier than the productive-only `alts` scan.
  Still cold and per-puzzle, but confirm it stays off any path that runs per-attempt.
- **Where it stops.** This is the richest score a *single cold easiest-first solve* can yield. Capturing
  difficulty that depends on *which order a human tries things* needs the path-quantified
  [`GENERATION-RULES.md`](../GENERATION-RULES.md) grader — still separate, still heavier, out of scope here.

## 10. Relation to other docs

- [`grader-granular-scoring.md`](grader-granular-scoring.md) — the predecessor: fixed the 3-band cut
  (Tier A/B, landed). This doc keeps that score as its backbone and adds resolution on top.
- [`campaign-grader-plan.md`](campaign-grader-plan.md) — the per-node **relative** grader; still the
  faithfulness reference (§2.4) and the calibration target for F1.
- [`grader-external-calibration.md`](grader-external-calibration.md) — the concrete F2 plan: tune the
  per-spec gradings against the external human-difficulty datasets
  ([`datasets/normalized/`](../datasets/README.md)), breaking the `grade_batch` self-reference, with
  Pelánek as the bar to beat. Realises §6 F2; its 2026-06-23 scope decision **drops §5.2's optional
  global number** (the grading stays per-spec).
- [`GENERATION-RULES.md`](../GENERATION-RULES.md) — the path-quantified grader; the ceiling this doc
  deliberately does not cross.
</content>
</invoke>
