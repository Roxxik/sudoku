# Fill branch rule & cost — what's worth optimizing

Status: STUDY COMPLETE + LANDED ON BRANCH (2026-06-14), branch `fill-branch-stats`. Two
coupled changes are applied here: (1) `Rng::from_seed` now SplitMix64-finalizes the seed (§6.5,
a prerequisite), and (2) `fill::random_solution` IS now the diagonal-seeded fill (§6) — so every
caller (`attempt`, `warp_host`, wasm) gets the -29% automatically; the plain empty-board fill
remains as `random_solution_with::<Mrv>`. The harvest/`determinism_fp` fixtures are
deliberately NOT re-baselined (a parallel change invalidates them anyway; this branch merges
with that one and the fixtures are redone once). Tool: `examples/fillpaths.rs` (a faithful
re-implementation of the fill DFS with branch-cell / value-order / prefill overrides, validated
byte-identical to the empty-board fill in §0). Companion: the prober's dual study
`docs/PROBER-BRANCH-RULE.md`, and the perf memos `project_fill_self_opt`,
`project_grid_fill_bitboard`.

**One-line conclusion.** The fill's MRV branch rule, sieve depth, and representation are all
at their measured floor — every per-node lever the prober had is unavailable here (the fill is
the prober's dual: node count is floored at 81, and the value order is *pinned* by the random
sampling). The one thing that does move the fill is **not a branch rule at all**: pre-seeding
the three diagonal boxes with random permutations replaces the 27 most expensive early MRV
scans with 3 free shuffles — a measured **1.42x fill speedup (-29.5% wall)**, ~-3% end-to-end.
Its only cost is that the produced grid is no longer byte-identical to core (re-baseline the
harvest fixtures).

---

## 0. Why this study, and the prober/fill duality

The strip generator's first half is the fill: `fill::random_solution` builds a complete
solution grid from the empty board by an **MRV + random-value DFS** on a digit-transposed
representation (`Fill<Bands<RowMajor>, Mrv<4>>`). It is **scan-bound**: ~82 nodes/grid, each
paying one candidate-count *sieve* scan, only ~1.75 backtracks. The fill is a robust **~4.0
us/att, ~9-11% of total generation wall** — and, unlike the prober, it never runs on the warp:
both the scalar `attempt` and the SIMT `warp_host` call the scalar `random_solution` on the
host (warp_host.rs phase `[8]`). So its only currency is scalar host time.

This study asks the same question `PROBER-BRANCH-RULE.md` asked of the prober — is there a
better branch rule (cell choice, value order, structure)? — and gets the **opposite** answer,
for a structural reason. The prober and the fill are duals:

| axis | prober (`cap=1` existence, near-full board) | fill (satisfaction, from empty) |
|---|---|---|
| node count | **many** (89% on reverts) — headroom to cut | **floored at 81** (~98% forced) — no headroom |
| value order | **free** (verdict-invariant) → MCV won -5% | **pinned** — the order IS the sampled grid |
| where it runs | the warp kernel (80% of passes) | scalar host only (~10% of wall) |

Every lever the prober could pull, the fill cannot. So the realizable win here is not a branch
rule at all (§6).

---

## 1. The workload: where the fill's cost is

`fillpaths` §1 (production MRV+random, 2000 grids; re-impl validated byte-identical to
`random_solution_with::<Mrv>` in §0, and its min-count histogram reproduces the production
`fillbench --features count` to 0.1pp):

- **82.06 nodes/grid, 1.754 backtracks/grid → 98.7% of nodes are forced.** The floor is 81
  (one placement per cell); the fill is already ~1 node above 81 + ~1 backtrack.
- **Each MRV scan is a fixed 9-board sieve sweep** (`Sieve::compute`, depth 4 = 7 ops/digit),
  branchless, in one SIMD register — its cost is *independent of board fullness*. So **node
  count == scan count == the fill's wall-clock**, and the per-node cost is the same on a near-
  empty board as on a near-full one.
- The branch-cell candidate-count distribution (the MRV min, which both ranks the pick and
  sets the sieve depth): **min=1 (naked single) 42.5%, min=2 27.7%, min=3 13.5%, min>=4
  16.3%** (cum <=3 = 83.7%, the measurement that picked depth 4).
- Nodes are spread ~evenly across board fullness (~20% per 16-cell bucket) — there is no
  "expensive slice" to gate, because every scan costs the same.

Implication: the only ways to make the fill faster are (a) fewer scans, or (b) a cheaper
per-scan. (b) is the sieve, which is tuned (§4). (a) is node count, which is floored at 81 by
the search (§2, §3) — *unless* you place cells without scanning them at all (§6).

---

## 2. Branch-CELL selection is saturated — MRV is load-bearing

`fillpaths` §2 holds the random value order and varies the cell rule (400 grids, 8000-node
cap; a non-MRV rule backtracks explosively, so it is bounded and reported as a cap-hit):

| cell rule | nodes/grid (finished) | vs MRV | capped (exploded) |
|---|---|---|---|
| **mrv** (production) | 81.97 | — | 0% |
| bivalue | 124.1 | +51% | 0% |
| lowidx (lowest unsolved) | 91.9 | +12% | 0% |
| random cell | 1580 | +1825% | 99.5% |
| maxcand (most candidates) | — | — | 100% |

MRV is what keeps the empty-board fill near-linear. The "keep-options-open" duals — random
and maxcand — **explode** (the search has no constraint guidance and backtracks catastrophically);
`bivalue` (the prober's rule) is +51% nodes and, on the production rep, **1.72x slower**
(`project_grid_fill_bitboard`: early on no cell is bivalue, so it degenerates to naive lowest-
index branching). There is no static cell rule better than MRV. The branch-cell axis is at its
optimum and there is no oracle headroom worth chasing — the floor is 81 and MRV is ~1 node
above it.

---

## 3. Value ordering is PINNED — the prober's one win is unavailable

This is the axis where the prober found its -5% (static MCV). For the fill it is closed.
`fillpaths` §3 holds the MRV cell and varies the value order:

| value order | nodes/grid | vs floor (81) |
|---|---|---|
| solution-guided (the value oracle) | 81.00 | 0% |
| ascending | 81.00 | 0% |
| descending | 81.00 | 0% |
| MCV (most-constraining) | 81.00 | 0% |
| LCV (least-constraining) | 81.00 | 0% |
| **random (production)** | 82.06 | **+1.3%** |

**Every deterministic value order fills in exactly 81 nodes — zero backtrack.** The ~1.75
backtracks/grid exist *solely* because the value order is randomized. But the randomization is
not optional: the order the candidates are tried **is** the produced grid (a different order is
a different grid). So there are two reasons value ordering cannot help:

1. **The win is bounded by the backtrack overhead (~1.3%)** — there is almost nothing to
   recover even in principle.
2. **Recovering it means derandomizing**, which collapses the grid distribution to a single
   fixed grid (non-uniform sampling) and breaks byte-identical-to-core. The axis is closed.

The prober could reorder children freely because its verdict is order-invariant. The fill's
"children order" is its entire output. Same mechanism, opposite verdict.

---

## 4. Sieve depth is settled (D=4), not a fresh lever

The per-scan cost is the sieve depth `D` (`Mrv<D>`). This was tuned before this study:
`Mrv<D>` picks the same cell for every `D` (it recomputes a full sieve on the rare all-`>=D`
node), so depth is a pure byte-identical per-node compute knob, and **D=4 is the measured
optimum** (the §1 distribution: 83.7% of nodes resolve at <=3, so depth 4 fast-paths all but
the ~16% near-empty nodes). The specific cheap idea the §1 histogram invites — a **depth-2
naked-single fast path** (skip the deep sieve when min=1) — is a **measured dead-end**
(`project_fill_self_opt`): naked singles are 42.5%, not a majority, and you cannot continue the
symmetric sieve past depth 2 without re-reading all 9 digit boards, so the 57% non-single nodes
pay *more*. `capped_min_tier` already short-circuits `exactly(1)` for singles, so there is no
tier-walk waste either. Depth is not a lever; `fillpaths -- N 1 depth` confirms the crossover.

---

## 5. Structural dead-ends (consolidated — do not re-prototype)

Every structurally-different fill has already been built and measured to lose. Collected here
so they are not re-attempted:

- **Incremental candidate maintenance** (don't rescan; update only the ~20 peers a placement
  touches): an undo-log is **1.13x slower** (branch+store per peer) and a maintained `count[]`
  array wins 1.28x but the **branchless SIMD sieve beats it** (`project_grid_fill_bitboard`).
  The digit-major rep was *chosen* precisely so full-rescan is cheaper than incremental scatter.
- **Vectorize the fill across grids on the warp** (use the unified kernel to propagate): a
  **decisive loss, 1.37x slower** (`project_fill_self_opt`). From-empty MRV has 0 backtracks,
  so the snapshot/restore DFS scaffolding is pure overhead; ~46 branches/grid pay scalar cost;
  the 8-wide ALU is mostly idle (the first ~20 placements propagate ~0-1 cells/pass). The
  scalar recursive MRV fill is the right tool. **Do not pursue warp-fill.**
- **Cell-major / mask-only reps, de-recursing the DFS**: regress or neutral
  (`project_grid_fill_bitboard`).
- Already-landed cheap wins (at floor): targeted 16-byte backup, single-word `contains()`
  gather, `from_digits` skip for the complete-grid strip start.

The scan is the cost, and the scan is already optimal *for the empty-board MRV search*. That
qualifier is the whole point of §6.

---

## 6. The win: pre-seed the diagonal boxes (cut SCANS, not branches)

Node count is floored at 81 *by the search*. But cells placed by **construction** — never
scanned, never branched — don't count against that floor at all. The three **diagonal boxes**
(top-left, centre, bottom-right) pairwise share no row, column, or box, so each can be filled
with an independent random permutation of 1..9: **27 cells, branch-free, conflict-free, 3
shuffles instead of 27 MRV scans.** Then MRV completes the remaining 54.

`fillpaths` §5 (node count == scan count, 2000 grids) — the completion's shape is *unchanged*,
it just starts 27 cells in:

| prefilled | nodes/grid | backtracks/grid | vs empty |
|---|---|---|---|
| none (empty) | 82.06 | 1.75 | — |
| 1 diagonal box (9 cells) | 73.11 | 1.82 | -10.9% |
| 2 diagonal boxes (18 cells) | 63.57 | 0.90 | -22.5% |
| **3 diagonal boxes (27 cells)** | **55.29** | 1.59 | **-32.6%** |

The reduction is almost exactly the 27 placed cells (82-27 ~= 55), and **backtracks do not
rise** (1.59 vs 1.75) — a random diagonal always extends, and MRV completes it just as cheaply
as it fills from empty. The 27 cells it removes are the *early, sparse-board* scans, whose
shuffles are also the most expensive (9-, 8-, 7-candidate).

`fillpaths -- 50000 1 bench` prices it in real wall-clock on the production banded rep
(the diagonal is now `fill::random_solution`; `diagonal_fill_is_valid` test green):

```
empty MRV (baseline) 3774.1 ns/grid   (random_solution_with::<Mrv>)
diagonal (now prod)  2669.5 ns/grid   (random_solution)
=> diagonal is 1.41x  (-29.3% wall, vs the -32.6% scan-count projection)
```

The -29.5% wall closely tracks the -32.6% scan-count projection (the small gap is the 3
shuffles + slightly denser completion). **A real 1.42x fill speedup.** Since the fill is ~9-11%
of generation wall (fixed per attempt across specs), that is **~-3% end-to-end** (naked-pair:
fill 4.00us of 36.2us total → ~-1.2us → -3.3%).

**The cost.** The diagonal `random_solution` consumes a different RNG stream and explores a
different node order, so the grid is **not byte-identical to core** — it requires re-baselining
the harvest fixtures (`tests/harvest_reconstructs`, `determinism_fp`), deliberately deferred on
this branch (see the status header). The grid
is still valid and random (three uniform diagonal permutations + a randomized completion — the
standard random-grid construction), so faithfulness is preserved; only the exact seed->grid
bijection changes. `equiv_warp` (scalar == warp lane-for-lane) is unaffected as long as both
drivers adopt the same fill.

---

## 6.5 PREREQUISITE: the prefill needs a scrambled seed

`Rng::from_seed` loads the seed **directly** as the xorshift64 state, and generation feeds it
**sequential** seeds (`base..base+N`). xorshift64 avalanches slowly, so the first outputs from
small seeds are degenerate. `fillpaths -- N 1 entropy` measures it:

- **The first `range(9)` over 30000 sequential raw seeds is 0.000 bits — 100% land on bin 0.**
  The first `next_u64()`'s high bits are tiny (dominated by the `<<17` of a small number), so
  `(next * 9) >> 64 == 0` always: the first Fisher-Yates swap is *deterministic*.
- The empty fill mostly tolerates this — the dead first draw only skews the first cell's
  candidate order, **diluted across 81 cells** (per-cell entropy min 3.000 vs the 3.170 ideal).
- **The diagonal prefill does not.** It spends those first (broken) draws on box 0's
  permutation — the grid's structural backbone — so one cell is **constant across all 30000
  grids** (per-cell entropy min **0.000**, mean 3.083). A permanently-fixed cell is
  disqualifying for a puzzle generator.

The fix is a one-shot **SplitMix64 finalizer on the seed** before it becomes state (the
canonical way to seed an xorshift family). `fillpaths` confirms it restores both fills to a
uniform 3.170 bits (min and mean), and the first `range(9)` to a full 3.170:

| config | per-cell entropy (mean / min) | first range(9) |
|---|---|---|
| empty, raw seed | 3.167 / 3.000 | 0.000 bits (100% -> 0) |
| diagonal, raw seed | 3.083 / **0.000** | 0.000 bits |
| empty, splitmix | 3.170 / 3.170 | 3.170 bits |
| diagonal, splitmix | 3.170 / 3.170 | 3.170 bits |

Every `from_seed` caller passes a sequential counter, a fixed test seed, or a user seed —
**none** sets a specific xorshift state — so scrambling inside `from_seed` is a safe, pure
improvement (and stays native==wasm: SplitMix64 is integer math). It is a separate change from
the prefill ([[feedback_one_change_at_a_time]]) and good on its own (it lifts the empty fill's
min cell from 3.000 to 3.170), but it is a **hard prerequisite** for adopting the diagonal
prefill. Land the seed scramble first (re-baseline `determinism_fp`/harvest, which the prefill
re-baselines anyway), then the prefill on top.

---

## 7. Does it carry to SIMT? (trivially — the fill is host-scalar both ways)

Unlike the prober (whose MCV win had to be ported into the warp's `branch_lane`), the fill is
**scalar host work in both the scalar and the SIMT generators** — `warp_host` calls the same
`random_solution` (warp_host.rs phase `[8]`). So the diagonal prefill is a pure scalar-host
change with **no kernel work**: because `random_solution` itself is now the diagonal fill, both
the scalar `attempt` and the `warp_host` site get the -29% with no call-site change. Zero warp
risk.

---

## 8. Bottom line & recommendation

| lever | fill cost | realizable? | notes |
|---|---|---|---|
| **diagonal-box prefill** | **-29.5% wall (1.42x)** | yes, host-only | not byte-identical (re-baseline fixtures) |
| seed scramble (SplitMix64) | ~0 | yes, prerequisite | fixes the dead first draw (§6.5); needed before prefill |
| branch-CELL rule (MRV) | at optimum | — | bivalue/random/maxcand worse or explode (§2) |
| value ordering | <=1.3% ceiling | no | pinned by sampling; derandomizing biases grids (§3) |
| sieve depth (D) | tuned at 4 | — | depth-2 fast path measured dead (§4) |
| incremental scan / warp-fill | loses | no | 1.13x / 1.37x slower (§5) |

**Recommendation / landed.** Both applied on this branch: (1) the **seed scramble** (SplitMix64
in `Rng::from_seed`, §6.5) — independently good and a hard prerequisite; (2) `random_solution`
**is** now the diagonal fill, so both call sites get it. Still TODO before merge: re-baseline
the harvest/`determinism_fp` fixtures (deferred — a parallel change redoes them) and confirm the
~-3% e2e on `findpar-bench`. Do
**not** pursue value ordering, a deeper/shallower sieve, incremental scanning, or warp-fill —
all are dual-of-the-prober closed or measured dead. The fill's branch *rule* is saturated; its
one remaining win is to scan fewer cells by constructing them, and the diagonal is the maximal
free construction.

---

## 9. Reproduce

```
# diagnostics (node counts, histograms, cell/value rules, diagonal scan-count): cheap, scales
cargo run --release -p generator-lab --example fillpaths -- 2000 1
# the wall-clock payoff (empty MRV vs diagonal prefill), needs volume:
cargo run --release -p generator-lab --example fillpaths -- 50000 1 bench
# the settled sieve-depth A/B (D=4 confirmation, not a lever):
cargo run --release -p generator-lab --example fillpaths -- 50000 1 depth
# the seed-entropy measurement (the diagonal-prefill prerequisite, §6.5):
cargo run --release -p generator-lab --example fillpaths -- 30000 1 entropy
# fill's e2e share (phase [8]) on the warp:
cargo run --release --features count -p generator-lab --example findpar-bench -- --force naked-pair --attempts 200000
# correctness of the diagonal fill:
cargo test --release -p generator-lab --lib fill
```

`fillpaths` args: `[attempts] [seed] [bench|depth|entropy]`. The default runs the cheap
diagnostics (§0-§5); `bench` runs only the production wall-clock A/B; `depth` the sieve-depth
confirmation; `entropy` the seed/output entropy study. §2's bad cell rules are node-capped
(they explode), so a small sample suffices.
```
