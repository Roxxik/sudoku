# Multi-spec generation — evaluating many specs per attempt

Investigation writeup, 2026-06-12. Design options and feasibility analysis, no
code yet. Companion to SIMT-EXPERIMENTS.md (whose section-0 numbers are cited
throughout) and SIMT-VISION.md (the three rules, section 7; the laws, section
10). Per the framing discussion: the three rules per spec are the contract;
trajectory and fingerprint faithfulness are NOT required (Law 3
reference-gating suffices) — but large parts of the design below turn out
exactly faithful per spec anyway, which makes the A/Bs cheap.

## 0. The idea, and the honest bound up front

Today one attempt (fill, strip walk, gates) is evaluated against ONE spec; the
production demand is puzzles for MANY specs (the curriculum: train + drill per
target). The idea: evaluate every attempt against S compiled specs at once, so
one fill + one walk's worth of probes can yield up to S puzzles.

The bound to state before any design: **a spec's yield (att/puzzle) is
untouched by sharing.** Multi-spec does not make the rarest spec cheaper — it
makes the other specs' puzzles (nearly) free alongside it. Consequences:

- Demand = "mostly drill hidden-quad": multi-spec is a strict LOSS. You pay
  the union ladder plus fork overhead per attempt for byproducts you do not
  want; dedicated drill-HQ at 31.4 us/att is strictly cheaper per drill-HQ
  puzzle.
- Demand = "fill the whole curriculum" (quota per spec, balanced-ish): the
  win is real. Attempts needed ~ max_s(att/puzzle_s); everything else rides
  along. Ballpark from the section-0 matrix: dedicated one-of-each over the
  eight configs costs ~14.5 s; a multi-spec stream needs ~max(att/puzzle)
  x C_multi — at C_multi 50-100 us/att (union walk + fork fan-out, see
  section 3) that is ~6.7-13.3 s, i.e. **1.1x to 2.2x**, settled by two cheap
  measurements (section 5).
- Demand = "as many puzzles as possible, any mix" is degenerate: unweighted,
  the cheap specs (xy-wing: 12 att/puzzle) dominate any strategy. Real demand
  is quota-shaped; the objective is total work to fill quotas.

Also honest: per-thread sharding already parallelizes the curriculum
wall-clock today (one spec per thread, Law 12). Multi-spec saves total
CPU-work, not wall-clock per se — it is a throughput/efficiency play plus an
architectural unification (one stream serves the curriculum), not a new
capability.

## 1. What is shared and what forks — the semantic facts

The whole design space hangs on six facts. F1-F2 are the sharing levers,
F3 is the irreducible cost, F4 is the correctness story, F5-F6 are the
per-gate machinery that makes S specs cost ~1 spec.

### F1 — probes are spec-independent, and non-unique rejects all specs at once

A probe reads only the board; no kindmask anywhere. And by the load-bearing
observation of SIMT-EXPERIMENTS section 1 (every technique preserves the set
of completions): non-unique implies unsolvable under EVERY toolbox, so a
non-unique verdict reverts the gate for every spec simultaneously. 48% of
posed gates (14.8/att) are resolved for the entire spec set by one shared
probe. The probe stays exactly what it is today — a shared cost device.

### F2 — the verdict/trace transfer theorem (one union solve, many exact answers)

Run the baseline solve ONCE under the union mask U of all live baselines,
first-fire in the canonical ladder order, and record the **fire log** (the
sequence of fired kinds, ~a few entries). Then for every spec s with mask_s
containing the whole fired set F:

- the verdict transfers (solved/stuck identical), AND
- the trace transfers EXACTLY (same fires, same counts, same order).

Proof sketch: at each stall, the first-fire scan minimizes (kind, unit) over
the mask's candidates in a fixed canonical order. mask_s's candidate set is a
subset of U's that contains U's minimum (the fired kind is in mask_s), so the
minimum over the subset is the same fire. Induction over stalls: identical
boards, identical fires, identical closure between fires (the closure kernel
set — singles + LC — is contained in every production baseline; see the edge
cases in section 4). And if U does not solve the board, no sub-mask does
(toolbox monotonicity), so "stuck under U" reverts the gate for every spec.

The only specs needing their own work at a gate are those whose mask is
missing some fired kind — and only from the first out-of-mask fire onward
(everything scanned before it transfers, including the no-fire verdicts that
feed the shared LadderMemo).

Quantitatively: ~16 baseline solves/att, of which gates that enter the harder
ladder at all are bounded by the ladder-entry counts — 1.54/att (train HQ),
0.89/att (drill HQ), a few/att for fish+wing unions. **All ~31 probes and
~14.5-15 of the ~16 solves per attempt are fully shared across every spec;
the per-spec residue lives at ~0.5-3 gates per attempt.**

### F3 — walk divergence is irreducible, and it concentrates where the money is

When specs disagree on a gate verdict (one keeps, one reverts), their boards
differ from that cell on, and they never re-merge: every later gate is posed
on different boards. So multi-spec is inherently a **trajectory tree**: one
shared trunk, forks at disagreement gates, per-fork suffix walks (own probes,
own solves, own UA filter state). This is not avoidable bookkeeping — the
per-spec walks ARE different walks; the design only shares their common
prefixes and their per-gate scan work.

The adversarial part: disagreement gates are harder-ladder gates, and those
concentrate LATE in the walk (boards near the uniqueness boundary, <= 32
clues, where ~85% of probe nodes live and where stalls cluster). The cheap
prefix shares; the expensive tail forks. So **gate-weighted sharing
overstates the win — only cost-weighted sharing counts** (measurement MM1,
section 5). This is the same inverse-correlation pattern that killed the
deferred-gate idea (gallery triage M1) and the keep-probe merge; respect it.

Note what this is NOT: the batch-solve-deferral NO-GO deferred gates within
one walk and died on verdict-order escalation. Here no gate is deferred —
walk order is intact per trajectory; sharing is across SPECS at the same
gate. The NO-GO is not reopened.

### F4 — each trajectory IS the dedicated walk: per-spec outputs are exact

Spec s's path through the tree takes, at every gate, exactly the verdict the
dedicated single-spec walk would take (F2 transfers are exact; the fork
continuations run s's own ladder). Same fill, same shuffle (both upstream of
any spec-dependence), same fast paths (alts==0, re-force, UA filter are all
board-functions carried per trajectory). Therefore **the multi-spec rig's
per-spec puzzle stream is seed-for-seed identical to today's dedicated
generator for that spec** — yields unchanged, distributions unchanged, and
the equivalence test is the strongest available: pin each spec's output
against scalar `generate(spec_s)` per seed. (Law 3 satisfied in its
strictest form, even though we did not require it.)

### F5 — the multi-scope ladder step: one scan serves every group

At a shared stall, generalize first-fire to **group first-fire**: scan kinds
in canonical order over the stall board (one CellMarks transpose, one
SubsetCache build, shared); when kind k fires at unit u:

- every live group whose mask contains k takes that fire (it is exactly
  their own first fire, F2) and exits the scan with the edit log; groups
  taking the same fire REMAIN one group (same board, same future);
- groups without k keep scanning the unchanged board for their next in-mask
  candidate;
- a group that exhausts the scan is stuck: its gate verdict is revert
  (becoming a separate trajectory whose board keeps the clue).

Cost: one ladder scan extended past the first fire to the last live group's
fire-or-exhaustion — i.e. at worst the full no-fire scan that stuck verdicts
already pay today (~1.5-2 us with fish+wing, per the E1 notes) — instead of
G separate ladder scans. The no-fire (kind, unit) verdicts feed the
LadderMemo once for all groups. Drill masks (trunk + one kind) come out
especially cheap: their entire ladder is one kind's scan, and the shared
scan answers "does t fire at this stall" for every drill at once.

### F6 — folded forcing piggybacks on the same fire log

The gallery-triage M2 insight generalizes per spec: at a kept gate with fire
log F, for spec s forcing f (need = 1):

- if f not in F and F is contained in avoid_scope_s (= in_scope_s minus f,
  conceded kinds included): the union trace itself proves the board avoid-
  solvable — Rule 2 cannot hold at this depth, no query needed, and the
  req_met pre-filter is correctly negative;
- if f in F: pose one avoid-scope solve (just another scope-tagged solve
  query, warp-batchable). Stuck => lock set (monotonicity certificate),
  final verify's avoid walk for f is skipped; solved => the requirement
  snapshot is suppressed early.

f-fires are rare (quad: ~0.0005% of solves), so the extra queries are near
zero while capturing drill verify's 11.0% share. One fire log feeds every
(spec, f) pair — this is where "evaluate many specs at once" is literally
free, because the trace is already paid for.

## 2. Approaches

Ordered from cheapest to most ambitious; they nest rather than compete.

### MS-A — fork-free multi-requirement (same baseline, several specs)

If several specs share one baseline mask and differ only in forced kinds /
needs / conceded sets, there is exactly one trajectory and zero forks: one
walk, one solve per gate, S req_met/best/verify bookkeepings reading the same
fire log (F6 included). Limited applicability — the curriculum's baselines
are pedagogically distinct on purpose — but it is a ~day of work on the
CURRENT host (GateEngine already has the trace; the attempt coroutine grows
per-spec best/req state), and it is the right vehicle to validate the
bookkeeping layer (per-spec outboxes, tallies, verify integration) before
any fork machinery exists. Also genuinely useful for spec families like
"baseline = full toolbox, forced = X" for varying X, if such curriculum
entries exist or get defined.

### MS-B — branch-family multi-spec, scalar prototype

The natural first fork-bearing target: one branch's train family is NESTED
(subsets: 6 masks trunk+prefix_k; fish: 3; bivalue: 3), plus the same
branch's drills (trunk + one kind). Within a branch:

- the union solve + fire log answers every train via F2 (for nested masks,
  "F contained in mask_s" reduces to a watermark compare: max fired kind <=
  s's top);
- the multi-scope scan (F5) answers every drill at the shared stall;
- forks split the family at watermark boundaries.

Build it SCALAR first (a `run_attempts_multi` sibling of `run_attempts`,
fork = recursive suffix walk on a StripState clone, ~1.3 KB). This is the
end-to-end validation of F2/F4/F5 (per-spec outputs pinned against dedicated
scalar runs) and the first real fan-out measurement, before any host work.

### MS-C — the full multi-spec rig on the slab host

The production form, and where SIMT-VISION's vocabulary stops being
aspirational: attempts decouple from lanes (the slab), a fork pushes a
suspended suffix-walk attempt into the slab, any free lane picks it up; the
per-gate flow is one shared probe query, one union solve query, the
multi-scope scan at stalls, group continuation solves as ordinary lane
queries with per-query scopes; per-spec outboxes and ledger on the rim.
Cross-branch fork fan-out is the open risk (every harder fire forks the
OTHER branches' families off the trunk — fish trains cannot transfer a
subsets fire), which is why MM1 must precede this build.

Prerequisite: the host redesign already in flight (warp_host_co / the
coroutine M2 split) is the enabling layer — fork-as-attempt requires
attempts >= lanes, which is the same slab the E4 batch station and E5 warp
fill need. One infrastructure investment, three consumers.

### MS-D — the SIMT enabler effects (why this helps the warp story)

Not a build of its own; consequences worth pricing into the decision:

- **E4 pressure**: the batch ladder station's blocker is feed depth (stalls
  arrive 2-6/att; a depth-8 buffer needs the slab). Multi-spec multiplies
  in-flight boards per attempt (trajectory groups + continuation solves) and
  makes stall service mask-uniform (everything scans under U in canonical
  order) — both directly improve E4's utilization math.
- **Continuation solves ride the warp**: a fork's re-closure and a group's
  post-fire drain are ordinary lane work; the scalar residue per gate is the
  shared scan only. This is the concrete form of "move expensive techniques
  to SIMT style": not vectorizing the subset scan per se, but turning the
  per-spec multiplicity into lane occupancy instead of scalar repetition.
- Composes cleanly with E1 (capped probe: keep iff solvable_g per group, the
  unique bit was never consumed), E2 (kernel change, orthogonal), E5 (fill is
  upstream of spec-dependence). E3 (trivial-first) RESHAPES the walk — if it
  lands, MM1 must be re-run on the new walk before sizing MS-C.

### Closed by analysis (recorded so they are not re-derived)

- *Post-hoc harvesting from a pure union walk* (no forks): the union puzzle
  over-strips for every sub-mask — Rule 1 fails for s on clues only U can
  re-derive; adding clues back is a different walk. Forks are intrinsic.
- *Cross-validating one spec's puzzles against other specs*: a train-HQ
  puzzle is minimal wrt trunk+subsets; trunk+HQ almost never solves it.
  Yield epsilon, Rule 3 wrong toolbox.
- *Static pre-forked lanes (one lane per spec per attempt)*: lanes are the
  scarce resource; duplicating identical prefixes 8-wide burns attempt
  throughput for nothing — fork dynamically or not at all.
- *Syncing forked groups at gates to share probes* (unique on the most-
  stripped board transfers to nested sibling boards with more givens — same
  cell, same walk position): sound under a clue-monotonicity certificate,
  but requires cross-group scheduling barriers. A refinement for later, not
  a v1 feature; note that the expensive probes (non-unique) do NOT transfer,
  same asymmetry as always.

## 3. Cost model

Per attempt, multi-spec over spec set S:

```
C_multi ~= C_frame                  (fill + ua-build + strip + host; 40-46% today, shared)
         + C_probe                  (shared; 22.6-31.6% today; grows only with fork suffixes)
         + C_union_solve            (closure shared; ladder = union mask, deeper than any single spec)
         + C_scan_extension         (F5: first-fire -> group-fire, ~stuck-scan price at fire-gates, ~1-3/att)
         + fanout x C_suffix        (F3: forked trajectories' own probes+solves)
         + C_bookkeeping            (S x req/best/outbox, fire-log reads ~ free)
```

Anchors (SIMT-EXPERIMENTS section 0): dedicated walks 29.6-41.9 us/att; the
cross-branch union train(w-wing+jellyfish) already prices a two-branch union
baseline walk at 41.9 us/att (+24% over train HQ) — a full union (subsets +
fish + wings) walk is plausibly ~45-55 us/att (MM2 measures it for free).
Fan-out is THE unknown: fire-gates are ~1-3/att under U, each splitting off
suffix work; if forks sit at 60-80% walk progress cost-weighted, expect
fan-out 1.3-2.2x. That spread is the difference between "barely pays" and
"2x+ on curriculum batch" — hence measure first.

Worked pair (train HQ + drill HQ, the two biggest s/puzzle): union baseline
of the pair = the train mask, so C_union ~= 33.9 us/att; drill forks at
non-HQ subset-fire keeps (~1/att, late) — say C_multi ~= 44 us/att. One
stream serves both: ~100k att x 44 us ~= 4.4 s per (train puzzle + drill
puzzle) vs 2.71 + 3.14 = 5.85 s dedicated => **~1.33x**, plus drill's 11.0%
verify share folded away via F6. Full-curriculum balanced demand compounds
across branches toward the 1.5-2.2x range; rare-spec-only demand stays a
loss (section 0).

## 4. Correctness and laws checklist

- **Law 2** (seed-determinism): a fork's outcome is a pure function of
  (seed, fork point); scheduling permutes completion order only. Holds by
  construction; per-spec ledgers need per-spec outboxes on the rim.
- **Law 3**: strongest form available — per-spec seed-for-seed equivalence
  against the dedicated scalar generator (F4). The reference checker still
  gates everything (verify per spec on per-spec best).
- **Law 4**: gates branch on verdicts; the fire log is a trace used as (a) an
  exact transferred trace where F2 applies, (b) a pre-filter feeding F6 —
  false positives caught by verify, exactly as today.
- **Confluence**: F2's induction needs per-scope confluence (the existing
  harness's territory; VISION wants it as a compiled-spec certificate).
  Specs without it fall back to own-solves at every fire-gate — correct,
  just less shared.
- **Monotonicity certificates**: only needed for F6's locks, per (spec, f),
  with the cold reference check as fallback — same status as single-spec
  folded forcing.
- **Edge cases to exclude from multi-spec sets (or handle per-group)**:
  baselines missing singles/LC (closure-set mismatch breaks F2's shared
  closure); specs forcing a cheap kind (baseline_fast_applicable = false:
  the re-force fast path and fused counts need per-group gating — the
  VERDICT still shares, but exact counts need an own solve). The production
  curriculum (Expert forced kinds, full trunk baselines) hits neither.

## 5. Measure first — two cheap deciders

### MM1 — trajectory-trie sharing diagnostic (the go/no-go)

No new walk machinery needed: per seed, run the S DEDICATED scalar walks
(same fill + shuffle, F4 guarantees each equals its multi-spec trajectory),
record each walk's per-cell decision string (skip / ua-caught / alts0 /
reforce / keep / revert-nonunique / revert-stuck) plus per-gate cost proxies
(probe branch nodes, ladder entries, rough us). Offline, fold the S strings
into a trie: shared-prefix work counted once, suffix work per leaf.

Report, per candidate spec set (per-branch family; train-only; drill-only;
full curriculum; the HQ pair): trajectories per attempt (leaf count), fork
positions (cost-weighted), **cost-weighted shared fraction**, projected
C_multi and projected speedup vs dedicated at the set's demand profile.

Kill criteria: cost-weighted sharing < ~30% for the curriculum set, or
projected speedup < ~1.2x for every interesting demand profile, or leaf
count ~ S (sharing collapses) => NO-GO, record and stop. (~150-300 lines as
an example binary, count-feature style.)

### MM2 — union-walk pricing (zero code)

`combobench --force hidden-quad --force jellyfish --force w-wing` (train)
prices the full-union baseline walk today — yield is irrelevant (a puzzle
would need all three forced kinds), us/att and the phase split are the
numbers. Compare against the section-0 matrix for the union-ladder premium;
re-run with `--toolbox drill` for the conceded-heavy variant.

### Sequencing if GO

```
MM1 + MM2 (days)
  -> MS-A on the current host (bookkeeping layer, fork-free)         [~days]
  -> MS-B scalar branch-family prototype (F2/F4/F5 end-to-end,
     per-spec equivalence pins, real fan-out numbers)                [~week]
  -> MS-C rig on the slab host (after/with the coroutine M2 split;
     shared prerequisite with E4/E5)                                 [the build]
F6 (folded forcing) can land inside MS-A already — it pays on drills
standalone (11.0%) and composes with everything later.
```

## 6. Relation to in-flight work

- The warp_host_co redesign (engine-in-coroutine, M2 split next) is not
  competing work — it is the enabling layer: forks need attempts decoupled
  from lanes, which is the same slab E4 and E5 need. If MM1 says GO, the
  slab stops being "vision infrastructure" and becomes load-bearing for a
  measured win.
- SIMT-EXPERIMENTS E1/E2/E5 are orthogonal and compose; E3 reshapes the walk
  and forces an MM1 re-run; E4's feasibility is IMPROVED by multi-spec
  (MS-D) — sequence E4 sizing after the multi-spec decision, not before.
- The UA filter, ladder memo, and all kernel work transfer unchanged (they
  are board-side, spec-blind).
