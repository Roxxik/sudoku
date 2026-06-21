# Campaign difficulty grader — plan

A grader that takes a generated puzzle and its spec and sorts it into a difficulty
band **within one campaign node**. Relative, not absolute: it orders the puzzles a
single node yields against each other and cuts them into bands. It does **not**
produce a portable difficulty score and is **not** the future `GENERATION-RULES.md`
grader.

## Scope and non-goals

- **Per-node, relative.** Grading happens inside one campaign node, where the spec
  (forced technique, baseline toolbox, ceiling) is fixed. So the player-facing
  **tier is constant across the node** and cannot be the discriminator — every
  signal here is *sub-tier*: how the forced technique is distributed across the
  solve and how hard each instance is to find.
- **Current code only.** Single easiest-first solve path from the existing
  non-branching solver. No quantification over `Π(P)`, no branching solver, no
  folded forcing. Those are premises of the `GENERATION-RULES.md` model (§1, §10),
  which is future design work and will get its **own** grader. This plan deliberately
  works with what ships today.
- **No absolute score.** Output is a band assignment relative to the node's batch.
  We never need calibrated weights against human timing data — only an ordering that
  is roughly right, then quantile cuts.

## Why instrument the non-fused (discrete) solver

Grading runs **only on puzzles that already pass validity** — a yielded puzzle is
one in many, so the grader is firmly off the hot generation path and can afford a
slower, richer solve. That removes the only reason to touch the fast path.

The fused engine ([`FusedLogicSolver`](../generator-lab/src/solve/fused.rs)) is the
wrong place to instrument anyway:

- It drains naked singles + both locked-candidate orientations in **bulk, reordered**
  waves, and its own module docs state the cheap-kind counts are undefined on that
  path ([fused.rs:139-141](../generator-lab/src/solve/fused.rs#L139)). You cannot read
  a clean step sequence or per-step placement-payoff boundary off it.
- The discrete [`LogicSolver`](../generator-lab/src/solve/logic.rs) fires **one
  technique at a time** via [`step_once`](../generator-lab/src/solve/logic.rs#L84),
  easiest-first, and is already the correctness oracle. That one-step-at-a-time shape
  is exactly the trace the grader's signals are defined over.

So: add a step-recording grading solve alongside `LogicSolver::solve_tracked`, reuse
the same ladder, leave the fused gate untouched.

## The model of human difficulty

A human is a greedy easiest-first solver: they always take the cheapest move they can
*find*. Within a node the cheap phase (singles, locked candidates) is broadly similar
across puzzles and is "free" — it cascades on its own. What separates puzzles is the
hard phase, organized around the **bottleneck**:

> A **bottleneck** is a stall where the cheap closure is exhausted and the node's
> forced technique `T` must fire to make progress.

Every valid puzzle in the node has at least one bottleneck (that is what makes `T`
forced). Difficulty is then about the bottlenecks: how many there are, how hard each
is to spot, and whether they chain without reward. Two further facts from the
brainstorm:

- **Combining constraints / scanning is the major work.** The hard techniques are
  read-heavy scans; the felt cost is the scanning, not the arithmetic. Within a node
  the per-technique recognition constant is fixed (tier is constant), so what varies
  is *how many* scans and *how hard each is to find*.
- **Scarcity makes a bottleneck hard.** The fewer productive moves available at a
  stall, the harder it is to spot any progress at all. One eligible pattern on the
  whole board is a needle hunt; several alternatives, and you stumble onto one.

## The signals

All four are derivable from a single easiest-first path. Two are already in
`SolveTrace`; two need the step trace described below.

### 1. Bottleneck count

How many times `T` must fire on the easiest-first path — `counts[T]` from
[`SolveTrace`](../generator-lab/src/spec/kinds.rs#L178). One forced firing then
singles-to-the-end is far gentler than three forced firings at identical tier.
**Already available**, no new instrumentation.

### 2. Dry back-to-back runs (the primary signal)

The "annoying to handle" case: two firings of `T` one after another with **no cell
placed in between**. `T` eliminates, no single appears, and `T` is needed *again* —
you do the hard scan twice before any reward, re-scanning a board that looks
unchanged, with no positive feedback that the first deduction was right.

This is a **placement-payoff gap**: a forced firing is **dry** when the cheap closure
immediately after it places zero cells before the next harder step is needed.

- **Measure:** in the solve loop, after a harder step fires, run the cheap closure
  and record whether it placed any cell *this iteration*. A dry firing is one
  followed by zero placements and then another harder step. Report the count of dry
  firings and the longest dry run.
- **Why primary:** it refines signal 1 in the direction that matters — three
  *separated, each-paying-off* `T`s are gentle (reward after each find); three dry
  `T`s in a row are one brutal grind. This is the signal the brainstorm flagged as
  the nastiest, so it leads the ordering (see Banding).

### 3. Scarcity at the bottleneck

At each stall resolved by `T`, how many distinct productive deductions are available
across the allowed toolbox. The cheap techniques are drained at a stall, so this is
effectively "how many `T`-deductions exist right now." Few = needle hunt = hard.

- **Measure:** at the stall state, enumerate *all* firing instances instead of
  short-circuiting on the first (the way [`step_once`](../generator-lab/src/solve/logic.rs#L84)
  and the [`ladder`](../generator-lab/src/solve/fused.rs#L451) currently stop at the
  first hit). This is a **single-state, non-branching count** — still on the one
  path, not over all paths. It is the honest current-code stand-in for the future
  model's `live`.
- **Aggregate** by `min` over the node's bottlenecks (the tightest stall defines the
  puzzle's hardest moment); keep the mean as a secondary.
- **Effort note:** this is the highest-effort signal — it needs an enumerating,
  non-mutating scan variant of the allowed techniques. A cheap first-cut proxy is the
  number of distinct eliminations the first forced firing makes; the fuller version
  counts independent deductions across the toolbox.

### 4. Scan / combine work

A mild path-length term standing in for total scanning effort: the sum of harder
steps (`counts[k]` for `k >= NAKED_PAIR`) or the cheap-closure iteration count.
Within a node it largely tracks signal 1, so it carries the lowest weight; it breaks
ties between puzzles that match on the first three. **Already available** (derived
from `counts`).

## Data to record: `GradeTrace`

Extend the discrete solver with a grading variant (working name `solve_graded`)
that returns, in addition to the existing `SolveTrace` fields:

- the ordered **step sequence**: for each step, the kind fired and whether the cheap
  closure that followed placed >= 1 cell (the dry/payoff flag for signal 2);
- per **bottleneck** (each stall resolved by `T`), the **alternatives count** for
  signal 3.

Hook points, all in [logic.rs](../generator-lab/src/solve/logic.rs):

- The loop in `solve_tracked` is the template — same shape (drain singles, check
  solved, one harder step, repeat). The grading variant records a `Step` per
  iteration and, before taking the forced step, runs the enumerating scan for the
  alternatives count.
- Detect a placement-in-this-iteration by occupancy delta (the `SolveView` occupancy
  the board already exposes) or by whether a single kind fired in the cheap phase.

Keep `solve_tracked` and the fused path unchanged; `solve_graded` is additive and
cold-path only.

## Banding (relative, per node)

No absolute score, so the order only has to be roughly right:

1. Compute the four signals per puzzle.
2. **Rank-normalize each signal within the node** (percentile across the batch), so
   signals on different scales compare.
3. **Weighted sum** of the normalized ranks, with **dry-runs (2) highest, count (1)
   next, scarcity (3) next, scan-work (4) lowest** — the priority the brainstorm
   settled on. Because the inputs are already rank-normalized and we only need an
   ordering, rough weights suffice; no human calibration data is required.
4. **Quantile-cut** the combined order into the node's bands (3 — gentle / medium /
   spicy — is the natural default).

An alternative to the weighted sum is a strict lexicographic order by the same
priority; it avoids weights entirely but is brittle (one signal dominates). The
weighted sum of normalized ranks is the recommended default.

## Implementation phases

1. **`solve_graded` + `GradeTrace`** in `logic.rs`: step sequence with the
   dry/payoff flag. Unlocks signals 1, 2, 4 (1 and 4 are already in `counts`; 2 needs
   the payoff flag).
2. **Enumerating scarcity scan** for signal 3: a non-mutating "count all firing
   instances at this state" pass. Start with the cheap proxy (eliminations from the
   first forced firing), then the fuller toolbox-wide count.
3. **Banding pass**: rank-normalize, weighted-sum, quantile-cut over a node's batch.
4. **Wire into the campaign generation** (the `train`/`drill`/`*_isolated` builders'
   consumers) so a node's yield is graded and banded.

## Open decisions

- **Weights and band count.** The priority order is set (dry-runs lead); the exact
  weights and the number of bands per node are calibration knobs, to be tuned once we
  can eyeball a node's banded output.
- **What counts as "payoff."** This plan uses *a cell placed* as the reward boundary
  (the strict reading from the brainstorm). Whether LC-only progress between two
  forced firings should also count as non-dry is a refinement to revisit.
- **Scarcity dedup.** When two productive deductions at a stall are "the same
  opportunity" (the same dedup question the future model flags in
  `GENERATION-RULES.md` §12). The cheap proxy sidesteps it; the fuller count must
  pick a rule.

## Relation to other docs

- `GENERATION-RULES.md` — the future, path-quantified model; its grader (§10) is a
  separate, heavier, post-validity functional over `Π(P)`. **This plan is not that.**
  It is the shippable, current-code, per-node grader.
- `CURRICULUM.md` / [`Tier`](../generator-lab/src/spec/kinds.rs#L133) and
  [`DIFFICULTY`](../generator-lab/src/spec/kinds.rs#L48) — the cross-node tier axis.
  This grader operates strictly *below* a fixed tier, so it is complementary: tier
  picks the node, this grader bands within it.
</content>
</invoke>
