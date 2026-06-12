# SIMT generator — question-count and warp-shape experiments

Plan of measurements and experiments for the next round of SIMT work, written
against the 2026-06-12 state of `generator-lab` (warp_host / solve::simt /
probe::simt). Scope per discussion: the per-step costs are near their floor
(modulo the planned memo and fish/wing scan work); what is on the table is
**posing fewer probes / fewer baseline solves** and **restructuring what runs
on the warp**. The SIMT-VISION laws are fixed; trajectory and fingerprint
preservation are NOT required (Law 3 reference-gating suffices), except where
an experiment happens to preserve them — that is then called out, because it
makes the A/B cheaper.

---

## 0. Baseline measurements (combobench, count feature, 400k att, seed 1)

8 lanes x 50k per lane, unpinned (no taskset, default governor — slices were
stable across 160k/400k repeats, but absolute us/att carries a few % of noise;
re-pin per DEEPDIVE methodology before trusting small deltas).

| config | us/att | att/puzzle | s/puzzle | probe total | baseline total | verify | host* |
|---|---|---|---|---|---|---|---|
| train hidden-quad        | 33.9 | 80k  | 2.71 | 27.8% | 31.3% (svc 26.1%) |  0.0% | 40.8% |
| train w-wing+jellyfish   | 41.9 | 15k  | 0.64 | 22.6% | 43.8% (svc 39.5%) |  0.1% | 33.5% |
| train xyz-wing+naked-quad| 37.7 | 50k  | 1.89 | 25.2% | 37.8% (svc 33.1%) |  0.0% | 37.0% |
| train sword+naked-triple | 32.8 | 17k  | 0.57 | 28.7% | 29.2% (svc 23.8%) |  0.1% | 42.0% |
| drill hidden-quad        | 31.4 | 100k | 3.14 | 29.9% | 14.9% (svc  9.4%) | 11.0% | 44.2% |
| drill w-wing+jellyfish   | 35.0 | 20k  | 0.70 | 26.9% | 30.6% (svc 25.6%) |  2.5% | 40.0% |
| drill xyz-wing+naked-quad| 30.2 | 133k | 4.03 | 30.8% | 21.6% (svc 15.9%) |  1.9% | 45.7% |
| drill sword+naked-triple | 29.6 | 29k  | 0.84 | 31.6% | 21.1% (svc 15.3%) |  0.8% | 46.5% |

*host = fill + ua-build + strip + unaccounted. Note ~11% "unaccounted"
includes the count build's own rdtsc bracketing (~6 pairs x 51 ticks/att), so
production is somewhat better.

Spec-independent constants: ~51 warp_pass calls/att at util 1.000;
~410 lane-passes/att split 80% probe / 20% baseline; **30.74 probes/att**
(48% revert / 52% keep); **~16.0 baseline solves/att** (one per unique probe).

Diagnostics (temporary DSTAT/LSTAT prints, 160k att):

- **branch nodes: 1.74/probe** (53.5/att). The UA filter already removed the
  deep revert searches; probe cost is closure cascade (10.7 lane-passes/probe),
  not branching.
- **ladder memo bite is 6.5%** on train HQ (7.4 unit-scans skipped vs 106.4
  run, 1.54 ladder entries/att) — the memo is invalidated on every load/flip,
  so nearly every entry is cold. drill HQ: 0.89 entries, 20.1 scans/att.

perf (count+profiling builds, -F 2000):

- train HQ: warp_pass_full 32.3, pump 10.8, advance 8.4, fill 8.3,
  cached_naked_subset(+combos) ~10.0, cached_hidden_subset(+combos) ~7.9,
  clear_clue 5.0, subset_step 4.7, branch_lane 4.2, ua-build 2.8.
  The subset unit scans (~18%) dominate baseline service, not the LC fixpoint.
- train WJ: warp_pass_full 26.1, **fish_step 16.5, wing_step 12.1**
  (+ eliminate_common_peers 1.3, w_wing_link 1.2, buckets 0.5 = ~31% total in
  fish/wing, rebuilt from scratch at every stall), pump 9.7, advance 6.9,
  fill 6.7, clear_clue 4.4, subset_step 4.0 (= LC + snapshot + transpose).

Per-solve cost (baseline total / keeps): drill HQ **0.29 us**, train HQ
0.66 us, train WJ 1.15 us. This number drives the E1 fork below.

---

## 1. The load-bearing observation

The gate's outcome is `keep iff baseline-solvable`. Every toolbox technique
preserves the complete set of completions, so a board the toolbox solves is
unique by construction, and a non-unique board can never solve. Concretely, in
`GateEngine`: a unique probe verdict flips to the baseline phase, and
`apply_baseline` reverts on `!trace.solved` — so the gate's final verdict
never actually consumes the probe's "unique" bit.

The probe is therefore a **cost device, not a semantic one**: a cheap rejector
for the 48% non-unique gates, justified only because a *stuck* baseline
verdict (full no-fire ladder scan per stall) is expensive. On the 52% unique
gates, probe tree exhaustion is pure overhead on top of a baseline solve that
runs anyway. Everything in section 3 (E1) and the endgame in section 5 follows
from this.

---

## 2. Measurements to make (cheap, count-gated, before building anything big)

### M1 — probe retirement depth histograms, split by verdict

Per probe retirement: lane-passes and branch nodes consumed, bucketed,
split unique vs non-unique. Implementation: two per-lane counters in
`GateEngine` (reset on `load`, tallied in `prober_service` paths), histogram
arrays under `feature = "count"`, printed by combobench.

Decides:
- Whether the depth tail is owned by **exhaustion** (unique) or by
  **completion-finding** (non-unique). Exhaustion tail -> E1 pays directly.
  Completion tail -> E1 needs a stuck-solve price below the tail cost (M2),
  and solution-guided digit ordering regains value as a cap-enabler (it pulls
  non-unique detections under the cap; on its own it is small).
- The cap value for E1 (choose so that >= ~95% of non-uniques resolve in
  budget).

### M2 — stuck-solve pricing per spec (= E1 at cap 0)

Run the gate solver-only: every probe flips to baseline immediately; stuck
means revert. Measure us/att and the new baseline lane-pass / ladder-entry
counts. This prices the no-fire ladder tail on real non-unique boards — the
one number the probe's existence rests on, and it is currently unmeasured
(the "4-5x slower" memory predates the UA filter, cached subsets, and memos).

Decides: the per-spec optimal cap for E1 (between 0 and infinity), and
**re-run after each ladder-cost change** (memos, E2, E4) — every ladder
improvement moves the optimal cap down. Back-of-envelope from section 0:
drill HQ pays 9.37 us/att of probe to avoid ~14.7 stuck solves; at 0.29
us/solve plus a stuck tail, solver-only is plausibly already at parity or
better there. Train WJ at 1.15 us/solve (and stuck solves pay the no-fire
fish+wing scan, ~1.5-2 us) is firmly probe-first until E4/memos land.

### M3 — stall pressure per tick

Histogram of the per-tick service-mask population, split by cause (probe
stall -> branch, baseline stall -> ladder, dead/solved -> retire).
Implementation: a few counters in `PuzzleStream::tick`.

Decides:
- The demand trigger threshold k for E2 (an LC pass pays when it replaces
  >= k scalar LC fixpoints; expected break-even k ~ 1-2).
- Whether a batch ladder station (E4) can fill: stall arrival rate vs buffer
  depth vs attempt oversubscription (Law 9). Stalls arrive at roughly
  2-6/att, i.e. one every ~7-20 us per warp — a depth-8 buffer fills in
  ~60-160 us, so E4 needs the attempt slab, not just the 8 resident attempts.

### M4 — fast-path conversion potential (prototype-borne, see E3)

The trivial-first scheduler's value cannot be read off the current walk
(triviality depends on visit order); the scalar prototype IS the measurement.
Track: probes/att, trivial keeps/att, baseline solves/att, yield, us/att,
s/puzzle.

### M5 — read-set dirt rates (informs the already-planned memo work; listed for sequencing only)

Per ladder entry: digit planes changed since last scan; changed cells that
*became* bivalue/trivalue; units changed since the previous gate's last scan
(memo carried across gates instead of invalidated). High cross-gate
similarity -> the planned memos remove most of the 18% (train HQ subsets) /
31% (train WJ fish+wing) slices, which **shrinks E4's prize and lowers E1's
optimal cap** — measure before sizing E4.

### M6 — off-warp LC fire rate

Fraction of `subset_step` calls that early-return on `scalar_lc_fast`
(today: invisible; one counter). High rate -> E2's demand-LC pass replaces
many scalar fixpoints and re-saturates lanes without scalar service at all.

---

## 3. Experiments

### E1 — capped probe: the probe as a budgeted non-unique rejector

**Design.** Give each probe query a branch-node budget (deterministic per
lane, Law 2). Completion found in budget: revert (as today). Tree exhausted in
budget: flip (as today). Budget exhausted, inconclusive: **flip to baseline
anyway**; solved -> keep (solvable implies unique), stuck -> revert (matches
what the probe would have concluded). Cap = infinity is today's rig, cap = 0
is solver-only — one knob spans the whole design space, so M2 is free once E1
exists.

**Law status.** Verdict-identical in every branch — same keeps, same reverts,
same traces on kept boards. Fingerprint-preserving; `tests/equiv_warp_repr`
stays green. Not a new rig, a cost knob.

**Expected gain.** Deletes exhaustion work beyond the cap on the 52% unique
gates whose solve runs anyway; adds stuck solves only for non-uniques deeper
than the cap (rare once M1 places the cap; rarer with solution-guided digit
ordering as a companion tweak). Per spec, bounded by M1's tail mass times the
probe slice (22.6-31.6%): realistic **3-8% e2e near-term** on subset/drill
specs at a finite cap, and **up to ~15% on drill specs if M2 confirms
cap ~ 0** (drill HQ arithmetic: 9.37 us probe vs ~14.7 x 0.29 us + stuck
tail). Train WJ: ~0 until the ladder gets cheap — then re-run M2.

**Risk/effort.** Small diff in `GateEngine::service` (count nodes, one extra
flip path). Low risk. Build first.

### E2 — demand-triggered LC pass on the unified warp

**Design.** Resurrect the vectorized in-closure LC (in git history per
solve/simt.rs docs) but fire it on demand: when this tick's service mask
shows >= k stalled lanes (M3 chooses k, expected 1-2), the next pass runs
singles+LC for the whole warp. Compiled-spec flag gates it (LC must be in
the baseline scope — true for all production specs; probe lanes have no upper
bound, Law 5).

**Why the 1/8-lane objection dissolves.** Over-propagation is sound and
*useful* for the lanes that didn't ask: probe lanes get smaller search trees,
baseline lanes get prunes they would have paid scalar LC for at their next
stall. The waste is only the pass's vector ALU — which the trigger amortizes
against k scalar `scalar_lc_fast` fixpoints (~200-400 ns each) plus the
avoided service round-trips. The measured in-closure-LC loss (1.36x vs 1.87x)
priced *unconditional* LC every pass; demand-triggered is an unmeasured third
point on that curve.

**Law status / fp.** Today the ladder always runs from the singles+LC
fixpoint (scalar_lc_fast runs first at every stall); with demand-LC the
ladder still runs from the same fixpoint (confluence certificate), so subset
traces — and hence puzzles — should be byte-identical for production specs.
Verify fp equality in the A/B; if it drifts, the rig is still Law-3 gateable,
but treat drift as a bug signal first.

**Expected gain.** Replaces most scalar LC fixpoints (part of subset_step's
4-4.7%), deletes a slice of baseline stalls outright (lanes re-saturate
without scalar service), and shrinks probe trees. Estimate **2-5% e2e**,
spec-independent; possibly more via fewer probe branch nodes (M1 re-run
shows it).

**Risk/effort.** Medium: kernel variant + trigger plumbing in `tick`.
Regression risk if the trigger is too eager — keep k a measured constant.

### E3 — trivial-first gate scheduling (deduce-then-probe rounds)

**Design.** Replace the single shuffled sweep with rounds: sweep remaining
cells, strip every currently-trivial one (`alts == 0` and re-force fast
paths — both verdict-carried, no question posed), repeat until no cell is
trivial; then pose one probe for the next contested cell (RNG order among
contested); after each kept gate, re-sweep trivials. Both fast-path theorems
are per-state and order-independent, so soundness is untouched.

**Why it should pay.** A cell visited late sees fewer givens, so triviality
decays along the walk: the fixed shuffle converts would-be-trivial keeps into
probes. Each conversion saved is a probe AND a baseline solve (~0.3 + 0.66 us
on train HQ): converting even 3-4 of the ~16 kept gates/att is **~10% e2e**,
on top of fewer reverted probes if contested cells shrink. Re-force already
catches 23% of gates in shuffle order — there is real mass here.

**Law status.** New rig: same-seed puzzles differ (Law 2 holds — still a pure
function of the seed; Law 3 reference-checker-gated, honest accounting). The
yield (att/puzzle) WILL shift and may move either way — judge on s/puzzle,
never us/att alone. Distribution of produced puzzles also shifts (bias toward
boards with long forced chains); whether that matters is a product question to
flag, not an engineering one.

**Risk/effort.** Scalar prototype in `attempt()`-variant form first (an
afternoon); SIMT port only if s/puzzle improves. Main risk: yield drops eat
the per-attempt win on rare specs.

### E4 — batch ladder station: vectorize subsets/fish across stalled boards

**Design.** The gallery's ladder warp, with the concrete vectorization story:
stalled boards vacate into a buffer (their resident lane refills from the
attempt slab); at pressure, a batch warp of 8 stalled boards runs the ladder
**in lockstep over combination indices** — all lanes walk the same (unit,
kind, combo) or (digit, orientation, combo) sequence over their own masks,
with a per-lane fired mask freezing first-fire lanes. Per-lane transpose paid
once per stalled board (it is paid today too, in CellMarks/SubsetCache).
Enumerating raw slot combos costs ~3-4x the scalar pruned enumeration per
lane; 8 lanes wide that still nets ~2x, branchless (today's combination loops
are also a bad-spec source, ~11% per DEEPDIVE). Wings stay scalar — long
tail. Outputs are edit logs (Law 7) applied back to the resident lane.

**Law status.** First-fire order across lanes is per-lane and preserved;
within-lane ladder order unchanged -> traces unchanged. The board's absence
from the resident warp while buffered changes tick interleaving only
(Law 2-safe). Buffer + pressure firing per Laws 8/9; this is specialization,
not migration — no load balancing, the 124-byte snapshot transfer is noise
against a 0.5-2 us scalar ladder visit.

**Expected gain.** Targets the subset scans (~18% train HQ) and fish scans
(~16.5% train WJ): at ~2x net, **8-15% e2e on ladder-heavy specs** — BUT this
prize overlaps the planned memo work (both attack rescans). Sequence: land
memos, re-run section-0 profiles, size E4 against what remains (M5). The
strategic value is independent of the % win: E4 makes **stuck verdicts
cheap**, which is what lets E1's cap fall toward 0 on more specs.

**Risk/effort.** The big build (station, buffer, slab oversubscription).
Do last, after the memos and M2/M3 have re-priced it.

### E5 — warp fill (fill as just another lane occupant)

**Design.** `random_solution` is an existence search with RNG-ordered
branching — structurally the same query the prober runs. Run fills on the
unified warp: a FillEngine whose stall service picks the branch digit from
the lane's RNG stream (per-lane streams keep Law 2); the kernel needs nothing
new. Fills occupy lanes between gates, so no second warp and no utilization
question.

**Law status.** New rig: a different propagation order yields different
solutions per seed (fill correctness never depended on which valid grid you
get; the solution distribution shifts — Law 3 gated).

**Expected gain.** fill is 11.7-13.4% e2e (3.96-3.99 us). Warp-amortized at
8 lanes with scalar branch service, expect roughly half: **~5-6% e2e**,
spec-independent. Also removes the largest remaining scalar slice from the
coroutine resume path.

**Risk/effort.** Medium: engine + attempt-priming restructure. Independent of
everything above; good parallel-track work.

### E6 (backlog) — strip-walk SIMD batching

`clear_clue` 8-wide is plausible vector code (the peer geometry reduces to
per-lane variable shifts, vpsllvd), but walk steps arrive event-driven at
~1-2 per tick, so it needs its own buffered station and depth; prize bounded
by the ~14% strip slice. Park it until E1-E5 settle; revisit when the host
floor dominates.

Closed by analysis (recorded so they are not re-derived): lane=digit
re-banding (the naked-single sieve becomes cross-lane reductions);
speculative probe+baseline dual-lane gates (util is 1.000 — speculation
displaces real work); multiple resident warps with query migration (no
utilization problem exists; only specialization pays, which is E4);
incremental cross-gate solves / saturated-board sharing (clue removal needs
truth-maintenance retraction — fights SoA and Law 7); dynamic UA learning
from probe witnesses (sole given is the reverted cell, never retried).

---

## 4. Sequencing

```
M1, M3, M6  (one combobench patch, a day)
   |
   v
E1 capped probe  -- M2 falls out (cap=0 run)  -> ship knob, per-spec cap table
   |
E2 demand-LC     -- gate: M3/M6; fp-pinned A/B
   |
(planned memo work lands here; re-run section-0 profiles + M5)
   |
   +--> re-run M2: optimal caps drop; drill/subset specs may go cap ~ 0
   |
E4 batch ladder station -- gate: post-memo profile still shows >= ~10% in
   |                        ladder scans; M3 says the buffer can fill
   v
endgame: one resident warp (scope-compiled closure: singles + demand-LC
[+ scope rungs]), batch station eating stalls, probe reduced to a capped
rejector, the solve as gate authority.

parallel track: E3 scalar prototype (s/puzzle verdict), E5 warp fill.
```

Composite expectation, honestly: E1+E2+E3+E5 in the near term ~ **15-25%
e2e** on most specs; with memos+E4 the ladder-heavy specs (train WJ at 41.9
us/att) have the most headroom, plausibly **1.5-1.8x per attempt** total.
The host floor (fill+ua+strip+plumbing, 40-46% today) then dominates —
that is E5/E6 territory, and on rare specs (80k-133k att/puzzle) anything
beyond per-attempt cost means touching yield, which only E3 reaches (in
either direction — watch it).

---

## 5. Repro

```
# matrix (this doc's section 0):
cargo run --release -p generator-lab --features count --example combobench -- \
    --force NAME[:N] [--force NAME] --toolbox train|drill --per-lane 50000

# configs used: hidden-quad | w-wing,jellyfish | xyz-wing,naked-quad |
#               swordfish,naked-triple   x   train | drill

# profiles:
cargo build --release -p generator-lab --features count,profiling --example combobench
perf record -F 2000 -- target/release/examples/combobench --force ... --per-lane 10000
perf report --stdio --no-children

# diagnostics used for branch-node / memo-bite numbers: temporary prints of
# probe::simt::dstat_snapshot()[2] and solve::lstat_snapshot() in combobench
# (not committed; M1/M3/M6 should land as proper count-gated reporting).
```

Caveat: all numbers above were taken unpinned with default governor on the
Zen 4 laptop; slices reproduced across runs, but pin (taskset, boost off, per
DEEPDIVE) before reading small A/B deltas.
