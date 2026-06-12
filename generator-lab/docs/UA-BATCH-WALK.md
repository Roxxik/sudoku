# UA batch-walk hypothesis — measurements & verdict

Follow-on to `docs/UA-FILTER.md` / `docs/UA-PACKED-BUILD.md`. The hypothesis
(`project_ua_batch_walk_hypothesis`): defer the per-cell gates of the strip walk to
**batch boundaries** — solving/probing once for `k` cells instead of `k` times — for a
projected ~20-27% e2e win, the prize being the **baseline solve** (~1/3-1/2 of e2e).

All numbers below: `examples/uabatchwalk` (faithful mirror of `generate::attempt`, no
behaviour change), `cargo run --release --features count -p generator-lab --example
uabatchwalk -- 8000 1`. Diagnostics live in `generate::random` behind `feature = "count"`:
`ua_uncaught_stat` (B1), `req_flicker_stat` (B2), `batch_solve_stat` (B3).

## Verdict (TL;DR)

**Conditional GO on solve-deferral alone; drop the two sub-ideas.**

- **The solve-cost prize is real**: deferring the baseline solve to a `k`-batch boundary
  cuts baseline **passes** (FSTAT, the honest cost proxy) by **~54-63%** (k=4-8). Probe
  cost is otherwise untouched (uniqueness stays per-gate).
- **It is sound and can be made yield-EXACT.** Naively deferring **diverges the
  trajectory** (loses ~7% verified yield — see B3) because uniqueness probes then see
  not-yet-baseline-checked boards. That divergence is fully avoidable (the rollback-replay
  policy below, `diff_attempts == 0` proven across every spec).
- **Net throughput ~+15-25% scalar** (puzzles / time, which prices in yield), at the LOW
  end of the ~20-27% projection and **sensitive to the true baseline e2e share** (pin it
  before building — the net roughly halves if baseline is 1/3 rather than ~1/2).
- **Both sub-ideas are dead/weak**: witness-learning has ~zero ceiling (B1); probe-batching
  cleanliness is low (most batches contain a revert).
- It is a **substantial new rig** (own SIMT twin + equivalence pinning, per the
  hypothesis's Law-3 note). Worth pursuing only because the baseline gate is the
  acknowledged "real remaining native lever" (`project_baseline_gate_is_untapped_half`).

## B1 — library-growth ceiling (the witness-learning sub-idea is dead)

The hypothesis's probe-batching wants each repair to *learn* a new UA to pre-catch
re-emptyings. Ceiling on that: of the reverts the COMPLETE 2-digit UA library already
fails to catch, how many could ANY growable UA library catch? (3-digit UAs are dead —
`project_ua_strip_filter`.)

```
reverts 23.9/att, revert-nodes 118.0/att
catch by reference 2-digit lib (by nodes): uncapped 36.3%, shipped cap-14 36.3%, size-18-only 0.06%
UNCAUGHT by any 2-digit UA: 63.7% of revert COST
  distinct-digit count of the uncaught witnesses: 3:5% 4:8% 5:12% 6:17% 7:22% 8:22% 9:13% (by nodes)
  2-digit witnesses uncaught by the complete 2-digit lib: 0
```

**63.7% of revert cost is uncaught, and 100% of it has a >=3-digit witness** (zero 2-digit
witnesses uncaught — the complete 2-digit library already catches every 2-digit orbit). So
no growable 2-digit library helps, and 3-digit UAs are dead. **Witness-learning adds
nothing; the probe-batching sub-idea has no headroom.** (cap-14 vs uncapped confirms the
size-18 tier is catch-dead, 0.06pp, as already known.)

## B2 — req-flicker (the YIELD risk the hypothesis flagged is absent)

Batch-boundary `best` tracking misses a requirement that fires then un-fires *within* a
batch. Measured `req_met` transitions over the faithful walk:

```
spec                  reached-req   fires/att  UN-fires/att  flicker-atts
train(hidden-quad)        0 (0.0%)      0.000       0.000          0
drill(hidden-quad)      472 (5.9%)      0.059       0.000          0
train(hidden-triple)     18 (0.2%)      0.002       0.000          0
train(naked-pair)       692 (8.7%)      0.086       0.000          0
```

**Zero un-fires anywhere** (24k+ attempts) => `req_met` is monotone-on; once the forced
kind fires it keeps firing as you strip further. So boundary `best`-tracking + an
end-of-attempt flush loses nothing *to flicker*. (The real yield risk is elsewhere — B3.)

NOTE: `reached-req` = the forced kind fired in the baseline trace; it is **not** a verified
success. `verify` still gates irreplaceability, and for drill (conceded peers substitute)
it rejects almost all of them — drill(hidden-quad) reaches 5.9% but verifies ~0%.

## B3 — batched-solve dry-run (the headline)

`batch_solve_stat` runs a sequential reference and a batched-solve walk on the SAME seeds.
Uniqueness stays per-gate (so the accumulated state is provably unique at every boundary);
only the baseline solve defers to every-`k`-keeps. `k=1` reproduces sequential exactly (the
anchor). Cost proxy is **passes** (FSTAT) and **probe-nodes** (PCTR), not query counts — a
boundary solve drains a more-stripped board, so counts would lie.

Three dirty-boundary recovery policies:
- **naive** — latest-first bisect (cheapest; over-restores => diverges most).
- **replay** — re-strip the batch under the per-cell baseline gate, trusting the
  initial-pass uniqueness verdicts (recovers most yield, still diverges some).
- **exact** — rollback-replay: a CLEAN boundary (joint solve succeeds) is *provably*
  divergence-free (baseline-solvability is monotone, so a solvable full batch had no
  baseline revert, so every uniqueness probe saw the sequential board); a DIRTY boundary
  rolls the whole window back to the pre-batch state and replays it cell-for-cell with full
  per-cell gating. Byte-identical verified output (`diff_attempts == 0`).

### B3a — solve-cost cut (drill(hidden-quad), seq baseline 16.0 solves / 167 passes per att)

```
 policy   tot-solv  passes   solv-cut  pass-cut  pnode-d   diff
k=4 naive    5.44    61.4     66.1%     63.3%     +1.2%     0
k=4 replay   6.13    70.7     61.8%     57.7%     +3.1%     0
k=4 exact    6.57    77.7     59.0%     53.6%    +25.8%     0
k=8 naive    3.66    42.4     77.1%     74.7%     +3.5%     0
k=8 replay   5.26    64.9     67.2%     61.2%     +4.9%     0
k=8 exact    5.77    72.4     64.0%     56.7%    +35.6%     0
```

The pass-cut is large and real. **exact** trades a big **probe-node increase** (+26-36%,
re-probing dirty windows) for zero divergence; naive/replay keep probes ~flat but diverge.

### B3b — VERIFIED yield (the real metric — a `best` must pass `verify`)

Divergence vs `k` on the best-measured spec (train(naked-pair), seq-verified = 692):

```
  k   replay-verif  replay-loss  replay-diff   exact-verif  exact-loss  exact-diff
  1       692          +0.0%          0            692         +0.0%         0
  2       664          -4.0%        102            692         +0.0%         0
  4       645          -6.8%        183            692         +0.0%         0
  8       618         -10.7%        235            692         +0.0%         0
 16       607         -12.3%        242            692         +0.0%         0
```

`k=1` (no deferral) reproduces sequential exactly. **replay** loses verified yield growing
with `k` (trajectory divergence — NOT flicker, B2). **exact** holds `diff = 0` and zero
loss at every `k`. Per-spec at k=8 exact, every spec: `seq-verified == bat-verified`,
`diff = 0` (train naked-pair 692=692, train/drill hidden-triple, the rare unions, and the
cranked train(hidden-quad) all preserved).

### B3c — dirty-window re-probe split (sizes the "re-probe only after first revert" lever)

EXACT's entire cost is re-probing the rolled-back dirty window. Only cells from the FIRST
baseline-revert onward can flip (the prefix saw the same boards sequential did), so a
monotone bisect that locates that revert (~`log2` solves) and re-probes only the suffix
would delete the *prefix* re-probe. How much is that? (8000 att, seed 1; `pfx-share` = the
deletable fraction of dirty re-probe nodes; `proj-pnode-d` = pnode-d after the cut; `rev-pos`
= avg deferred-keep index of the first revert / avg window keeps, so `log2` of the second is
the bisect's added solves):

```
spec                k  dirty/at  pfx-share   pnode-d  proj-pnode-d   rev-pos
drill(hidden-quad)  4    0.532     45.4%     +25.8%      +15.8%      1.4/3.8
drill(hidden-quad)  8    0.472     37.4%     +35.6%      +24.3%      3.4/6.9
train(hidden-quad)  4    0.476     45.1%     +22.6%      +13.8%      1.4/3.9
train(naked-pair)   4    0.513     45.3%     +24.9%      +15.2%      1.4/3.8
```

**Verdict on the bisect lever: real but partial — it is NOT "~replay's low cost."** The
prefix is only **~37-45%** of the dirty re-probe (less at larger `k`), because the first
revert lands roughly mid-window (`rev-pos` ~1.4 of 3.8 keeps at k=4) and the suffix carries
the bulk of the re-probe nodes (every uniqueness-revert cell after the first baseline-revert
must be re-probed). So bisect cuts the k=4 EXACT overhead from ~+25% to **~+15%**, not to
replay's +3%. The bisect's own cost is cheap (`log2(3.8)`~2 baseline solves per dirty
boundary, ~0.5/att). The **suffix** (~55-63% of the dirty re-probe) is the irreducible part
under a faithful replay; compressing it further requires the (unmeasured) observation that
suffix cells that originally KEPT stay unique when the reverted clue is restored
(uniqueness is monotone under added givens) — only originally-REVERTED suffix cells can flip,
so trusting suffix keeps and re-probing only suffix reverts (with a cascade-fallback) could
shrink it. Separately, the k/2 suffix recursion attacks PASSES not probes: the replay solves
per-cell (k=1), discarding the suffix's pass-cut; re-batching the suffix at k/2 recovers it
(see B3a — EXACT's pass-cut 53.6% vs REPLAY's 57.7% at k=4 is exactly this gap).

## Economics (net throughput = yield / time)

`time_ratio ~= 1 - pass_cut * baseline_share + pnode_delta * prober_share`. Using the
measured native split `project_baseline_gate_is_untapped_half` (~49% baseline, ~45%
prober); zero yield loss for exact, the measured loss for replay:

| policy (k=4, drill) | pass-cut | pnode | yield | net throughput |
|---|---|---|---|---|
| exact | 53.6% | +25.8% | 1.000 | **+17%** |
| replay | 57.7% | +3.1% | 0.932 | **+27%** |

Two honest caveats that bound the verdict:
1. **e2e-share sensitivity.** If baseline is only ~1/3 of e2e (the hypothesis's figure,
   likely post-filter/SIMT) the exact net falls to ~+5% and replay to ~+13%. **Pin the
   real baseline e2e share before building** — the answer swings 2-3x.
2. **exact vs replay is a real fork.** replay nets HIGHER (+27% vs +17%) because its probe
   overhead is negligible and its 7% yield loss costs less than exact's +26% re-probe — but
   replay produces *different* puzzles (a new fingerprint). exact preserves byte-identical
   output. The hypothesis already budgeted a new Law-3 rig with its own pinning, so replay
   is admissible; exact is the choice only if output-identity to today's generator matters.

## Key next lever (measured — B3c)

The exact policy's +26% probe overhead is re-probing the WHOLE dirty window. Only cells
*after the first baseline-revert* in a window can flip; the rest were already correct, so a
bisect-located, suffix-only replay deletes the prefix re-probe. **Measured (B3c): that prefix
is only ~37-45% of the dirty re-probe** — the first revert lands mid-window, the suffix
carries the rest — so the bisect cuts k=4 EXACT from ~+25% to ~+15% probe, NOT to replay's
+3%. It is a real cut at near-zero cost (~2 bisect solves per dirty boundary), but it does
not by itself reach "best of both." Two further levers to combine for the byte-identical
target: (1) re-batch the suffix replay at k/2 (recovers the pass-cut the per-cell replay
discards — a SOLVES win, orthogonal to the probe cut); (2) trust suffix cells that originally
KEPT (uniqueness is monotone under the restored clue) and re-probe only originally-reverted
suffix cells, with a cascade-fallback — could shrink the ~55-63% suffix, UNMEASURED.

### Lever 3 — boundary on cells-processed, not on deferred keeps (UNMEASURED)

The dirty re-probe cost scales with the WINDOW's cell count, not its keep count: the replay
re-probes every non-fast cell in the rolled-back window, and most of those are
uniqueness-revert cells, not keeps (~24 reverts/att vs ~16 keeps/att). But the boundary today
triggers on `k` deferred *unique keeps*. Late in the strip unique keeps are rare (most probes
revert), so gathering `k` keeps drags in a long tail of revert cells — the window balloons
exactly where baseline-reverts also cluster, which is why dirty windows are expensive (B3c
shows the re-probe nodes growing with `k` and `rev-pos` reaching 9.6/13.5 keeps at k=16).

Triggering the boundary on `k` cells *processed* (or on whichever of `k`-keeps / `k`-cells
fires first) caps the window directly and is implicitly **given-adaptive without a schedule**:
early (given-rich) `k` cells ~= `k` keeps, so batching is full; late (given-poor) `k` cells
hold only 0-1 keeps, so windows stay small and the dirty re-probe stays bounded. This is the
cheap structural cousin of the explicit adaptive-`k` / AIMD lever — a one-line change to the
boundary predicate — but it shifts the pass-cut profile (clean boundaries now bundle fewer
solves late-strip), so it must be measured against the keep-count boundary, not assumed.

## What was measured, what was not

- Measured: scalar walk, native x86. B1 ceiling, B2 flicker, B3 cost + verified yield
  across train/drill specs of varying rarity (naked-pair common, hidden-triple measurable,
  hidden-quad cranked to 120k, the three combobench union pairs). B3c: the bisect lever's
  ceiling (prefix ~37-45% of dirty re-probe; proj-pnode-d ~+15% at k=4, not replay's +3%).
- NOT measured: the SIMT twin (the warp would batch differently — and with the fused
  `warp_pass` doing probe+baseline in one pass, the e2e attribution is muddy, so the prior
  baseline-e2e-share economics are not reliable for the generator); the suffix-keep-trust
  compression (B3c's open lever — how often an originally-reverted suffix cell flips to
  unique under the restored clue); the in-between `k` sweep (3,5,6,…); adaptive `k` by givens
  / AIMD; the dirty-window-handling actually BUILT (bisect + k/2 suffix recursion).

---

# Lever measurements and the decision gate (M1-M6) — VERDICT: NO-GO

The follow-on measurements (`examples/batchlevers`, the same 8 production configs:
train+drill x {hidden-quad, w-wing+jellyfish, xyz-wing+naked-quad, swordfish+naked-triple})
drive the levers through the gate. **They fail it.** The B3 pass-cut was the honest proxy for
*total* baseline work, but it is dominated by the cheap closure passes; the expensive
harder-ladder (subset-ladder) work the SIMT `baseline-svc` line is made of is NOT cut by
deferral — it GROWS. Every one of the strategy's GO criteria except `diff==0` fails.

## M1 — svc-step proxy split (decisive)

Split the baseline solve's `FSTAT` work into closure passes (`FSTAT[7]`, the B3 "pass-cut",
the SIMT `baseline-warp` analog) and harder-ladder subset steps (`FSTAT[2]`, the SIMT
`baseline-svc` analog). The pass-cut is real and config-invariant (~54% exact k=4); the
**svc-step cut does not track it** — it is flat-to-NEGATIVE, and escalations/solve rise 3-5x
(a deferred boundary solve drains a `k`-deeper board, so it escalates the harder ladder much
more often). Exact, k=4, per config:

```
config                     pass-cut  svc-cut   esc/solve(seq->exact)
train(hidden-quad)           54.0%   -38.1%      0.097 -> 0.378
drill(hidden-quad)           53.6%   -24.7%      0.056 -> 0.226
train[w-wing+jellyfish]      55.7%    -1.0%      0.159 -> 0.489
train[xyz-wing+naked-quad]   55.5%   -14.5%      0.140 -> 0.475
train[swordfish+naked-trip]  54.0%   -39.6%      0.103 -> 0.401
drill[w-wing+jellyfish]      54.8%    +5.5%      0.098 -> 0.315
drill[xyz-wing+naked-quad]   54.0%   -31.9%      0.078 -> 0.302
drill[swordfish+naked-trip]  54.0%   -29.3%      0.076 -> 0.296
```

svc-cut is negative on 6/8 configs (best case +5.5%, never within 50pp of the +55% gate).
The absolute svc base is small (~0.25-2.0 steps/att) but in the SIMT cost structure that
slice is the dominant baseline cost (phstat: `baseline-warp` ~5%, `baseline-svc` ~24% avg,
range 9-39%). So the pass-cut only addresses the cheap ~5% closure slice while the dominant
~24% slice grows ~30-40%. The earlier `project_ua_batch_walk_hypothesis` projection of
"+18% avg" was credited the pass-cut against the FULL ~29% baseline share — wrong: only the
~5% closure slice is cut.

## M2 — cells-processed boundary (Lever 3): refuted as a rescue

Boundary on `min(k keeps, m cells)` (EXACT, k=8) caps the dirty-window size and DOES shrink
the dirty re-probe (`pnode-d` off +32% -> m=8 +12% -> m=6 +9%, as hypothesized) — but it
makes BOTH the pass-cut and the svc-cut WORSE (more, shallower boundary solves still each pay
the harder-ladder entry cost). train(hidden-quad):

```
 m     pass-cut  svc-cut  pnode-d
 off    57.5%    -35.5%   +32.0%
 12     46.8%    -46.5%   +16.3%
  8     26.7%    -69.3%   +12.1%
  6     15.7%    -72.0%    +8.8%
```

It shifts the trade (probe down, passes+svc up); it does not rescue svc. `diff==0` throughout.

## M4 — k/2 suffix recursion: refuted as a net win

Re-batching the dirty replay at `k/2` instead of per-cell recovers a few points of the
(cheap) pass-cut at `diff==0` (k=8 per-cell 57.9% -> k_4 60.4%, near replay's 62.3%) — but the
recursive sub-rollbacks re-probe cells repeatedly and the sub-boundary solves are deeper, so
it BALLOONS the expensive dimensions: pnode-d +32% -> +67%, svc-cut -34% -> -131%
(train(hidden-quad), k=8). The per-cell replay dominates it on every dimension that costs.
The doc's "k/2 suffix recursion (a SOLVES win)" is real but a net LOSS once M1 prices the
slices.

## M5 — the gate table (best exact = per-cell, m=off)

M2 and M4 are both refuted, so the best exact variant is plain per-cell exact. Sweeping
`k in {2,3,4,6,8}` over all 8 configs, the GO criteria (pass-cut>=55%, svc-cut>=55%,
pnode-d<=10%, diff==0): **every (config, k) is `no`.** The decisive failure is svc-cut.
`proj-pnode-d` is the suffix-only bisect floor (prefix re-probe deleted, B3c's ceiling) and
even it stays above 10% wherever the pass-cut clears 50%. Representative (drill(hidden-quad)):

```
 k   pass-cut  svc-cut  pnode-d  proj-pnode  diff  gate
 2     34.9%   -36.5%   +15.4%    +6.9%        0    no
 4     53.6%   -24.7%   +25.8%   +15.8%        0    no
 8     56.7%   -20.8%   +35.6%   +24.3%        0    no
```

`diff==0` holds for every exact policy at every k (rollback-replay, the recursive replay, and
the cells-cap all preserve byte-identical output — the one criterion that passes).

## M6 — curriculum attempt-weighting: weighting deepens the NO-GO

Production runs the linear curriculum (`core::curriculum::CURRICULUM`, Naked Singles ..
Jellyfish). Verified yield falls steeply, so the rare hard stages dominate wall-clock — and
they are exactly the worst svc-cut. Per fast-path stage (train, EXACT k=4, 8000 att):

```
stage          verified  att/puzzle  pass-cut  svc-cut
naked-pair         692       12        53.7%   -35.9%
hidden-triple       18      444        54.0%   -38.1%
naked-quad           2     4000        54.0%   -38.1%
hidden-quad          0    >8000        54.0%   -38.1%
xy-wing            744       11        54.1%    -7.0%   (common -> low weight)
w-wing             860        9        55.6%    -1.0%   (common -> low weight)
swordfish           11      727        53.0%   -45.0%
jellyfish            0    >8000        53.0%   -44.9%
```

The two near-zero-svc-cut stages (xy-wing, w-wing) are COMMON (low wall-clock weight); the
wall-clock-dominant rare stages (naked-quad, hidden-quad, swordfish, jellyfish, x-wing) sit at
-38% to -45%. **Attempts-per-puzzle-weighted svc-cut = -38.8%.** Weighting pushes the headline
MORE negative, not toward the gate.

## Economics and verdict

`time_ratio ~= 1 - pass_cut*baseline_warp_share - svc_cut*baseline_svc_share +
pnode_delta*prober_share` (svc_cut negative => the term ADDS time). With the SIMT phstat
shares (`baseline-warp` ~5%, `baseline-svc` ~24%, prober ~28%) and the curriculum-weighted
exact k=4 (pass_cut 54%, svc_cut -38.8%, pnode_delta +24%):

```
1 - 0.54*0.052 - (-0.388)*0.236 + 0.24*0.28 = 1 - 0.028 + 0.092 + 0.067 = 1.13  (+13% SLOWER)
```

even with the suffix-only bisect (pnode -> +14%) it is ~+10% slower. **The SIMT twin is a net
LOSS** because deferral grows the dominant baseline-svc cost; the wall-clock-dominant configs
sit at the high end of the `baseline-svc` range (9-39%), deepening it. The original +18%
projection is inverted by the slice it ignored.

**Scalar is indeterminate from this rig** (not GO): the pass-cut is real, but whether the
cheap-closure savings outweigh the svc increase depends on the SCALAR closure-vs-svc cost
ratio, which is unmeasured. Either way the prize is far below the doc's old +17% scalar
projection (which, like the SIMT one, mis-credited the pass-cut against all of baseline). The
one measurement that would settle scalar: a wall-time split of `FusedLogicSolver` into closure
vs subset-ladder cost.

**Decision: NO-GO on the batch-solve-deferral build (exact or replay).** The prize was the
baseline solve; M1 shows deferral does not cut the expensive half of it. Do not build the SIMT
twin. `diff==0` (exactness) was fully demonstrated and is not the blocker — the economics are.

> Provenance: the count-gated rig that produced these numbers (`batch_solve_stat_cfg` +
> `examples/{uabatchwalk,batchlevers}`, `k=1` the sequential anchor) lived on the
> `ua-batch-walk` branch, now closed. Only this writeup was kept on master; the code is
> recoverable from git history (reflog) if the question is ever reopened.
