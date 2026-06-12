# UA strip pre-filter — implementation plan

Status: LANDED; tier-split RESOLVED to Full on both engines (2026-06-12). Stage 1 =
UA4-only on both engines; stage 2 = the full 2-digit library on the scalar/wasm path,
with SIMT initially staying UA4-only because the then-scalar Full build (~3.7-4 us)
outweighed the extra catch on a ~35 us warp attempt (section 7). The packed-build
follow-ups (`docs/UA-PACKED-BUILD.md` sections 11-14) cut the Full build to ~1.4
us/board, which crosses section 7's break-even: remeasured on the same combobench
workload, Full beats UA4 on SIMT by ~0.9-1.0 us/att (train ~33.6 -> ~32.7, drill
~31.0 -> ~30.0; UA-PACKED-BUILD section 15). Production is now the full 2-digit library
on **both** engines. The `UaTier` machinery (enum, `SCALAR`/`SIMT` constants, the `tier`
parameter threaded through `attempt`/`StripState`/the warp/`run_attempts`/`GateStream`)
has since been **deleted** (UA-PACKED-BUILD section 18): production builds Full directly,
and the UA4 codepath (`enumerate_ua4`) survives only as the `ua4_equals_full_size4`
build-level differential oracle. Section 7 below records the original (pre-packed-build)
verdict; references to `UaTier::*` and the strip-walk tier-invariance test
(`tests/ua_filter.rs`, now deleted — soundness pinned at the build level) are historical.

## 1. Idea and why it pays

An unavoidable set (UA) of a solution grid S is a set of cells U such that S restricted
to the complement of U has another completion (the alternate arrangement of U). Hence
every UA must contain at least one given, or the puzzle is non-unique.

The strip walk's dominant cost is the uniqueness prober's non-unique reverts (85.5% of
prober nodes; prober = 43-46% of SIMT warp cycles, ~56% of scalar e2e). The filter
deletes a revert *question* before it is posed: if stripping cell c would leave some
library UA with zero givens, the gate is provably a revert — no probe needed.

This does not touch the per-question floor (>= 1 guess per non-unique board, see
project_prober_at_floor); it reduces the number of questions. It complements the
prober; it must NEVER replace it (see project_do_not_remove_prober) — the library is
incomplete, so the prober remains the authority on everything the filter passes.

Measured stakes (M-UA-LIB, 8000-att counter / 320k-att SIMT split, seed 1,
train(hidden-quad); drill identical; the uniqueness gate is spec-independent):

- Library = all 2-digit-cycle UAs: 55 UAs/board avg, of which 10.7 are UA4s.
- 23.9 reverts/att, 118 revert-nodes/att. False positives measured: 0.
- True catch of revert cost (by nodes): UA4-only 23.9% (23.1% at clue<=32),
  UA4+2digit 36.3% (35.5% at clue<=32). Catch holds at the deep boundary.
- SIMT revert-side pool: 36.8% (train) / 39.4% (drill) of 39.60 / 37.11 us/att
  => ~14.6 us/att. Expected gross: UA4-only ~3.5 us (~9% e2e), full ~5.3 us (~13-14%).
- Scalar pool ~67 us of 140 us/att. Expected gross: UA4-only ~16 us, full ~24 us (~17%).

## 2. Design (both stages share this)

Per attempt, right after the board is filled and before stripping begins, build the
UA library for that solution grid:

- Each UA is a cell-set mask (GridMask or the banded equivalent) plus a given-count.
- Counts initialize to |U| (the walk starts from a full board; every cell is a given).
- A per-cell adjacency index (cell -> indices of UAs containing it). Average ~5 UAs
  per cell with the full library; fixed-capacity flat structure, no per-gate allocation.

Walk integration, in the host-side strip loop (the same walk both the scalar engine
and the warp host drive):

- Before posing the uniqueness probe for candidate cell c: if any UA containing c has
  count == 1, revert immediately without posing the probe. (count == 1 and c in U and
  c currently a given imply the sole given IS c — no need to track which cell.)
- A filter-caught gate takes the exact same revert path as a prober revert: cell
  becomes unstrippable, walk continues. Nothing else changes.
- On each KEPT strip of cell c: decrement the count of every UA containing c.
  Reverted cells stay givens — no count change.

Correctness properties to preserve (these are the point of the design):

- Sound: the filter only fires on gates the prober would revert (UA emptied =>
  alternate completion exists). It can never fast-accept.
- Trajectory-identical: verdicts are unchanged, so the strip walk, the produced
  puzzles, and the run_attempts fingerprint are bit-identical with the filter on or
  off. Scalar and SIMT may even run different library tiers safely.
- Walk-order-independent: the check is stateless given the counts; correctness does
  not depend on cells being tried once (only the savings estimate does).
- Truncation-safe: if the library capacity overflows, dropping UAs is sound (fewer
  catches, never a wrong verdict). Pick capacity with headroom above the 55 avg
  (e.g. 128) and degrade by truncation, not panic.

Note: the M-UA-LIB instrumentation counter already enumerates this exact library and
walks it alongside the faithful attempt walk (false_positive: 0 over 191k reverts).
Lift/port that enumeration and bookkeeping rather than rewriting from scratch; the
production version drops the verification probe and actually short-circuits.

## 3. Stage 1 — UA4-only

Enumeration (~650 comparisons, effectively free; do it inline per fill):

- A UA4 is a rectangle {(r1,c1),(r1,c2),(r2,c1),(r2,c2)} with S[r1][c1] == S[r2][c2]
  and S[r1][c2] == S[r2][c1], spanning exactly two boxes.
- In a valid grid that two-box condition reduces to: the two rows share a band, OR the
  two columns share a stack. (Both at once would put a digit twice in one box —
  impossible; rows and columns both crossing means four boxes and the swap breaks
  them.) So: 9 same-band row pairs x 36 column pairs, plus 9 same-stack column pairs
  x 36 row pairs = 648 checks, no double counting.
- Sanity anchor: 10.7 UA4s/board average (seed-1 fill distribution).

Acceptance gates:

1. run_attempts fingerprint bit-identical, filter on vs off, same seed, scalar AND
   SIMT, train(hidden-quad) + drill(hidden-quad).
2. Full test suite green: cargo test --release (generator-lab equivalence tests are
   too slow in debug).
3. A validation mode (debug_assertions or the count feature) that, when the filter
   fires, still runs the probe and asserts the prober agrees (revert). Production
   builds skip the probe entirely.

Benchmark (one change at a time — this stage lands alone):

- combobench before/after, 40000 attempts, seed 1, train(hidden-quad) and
  drill(hidden-quad), scalar and SIMT paths. (simtbench is broken pre-existing; use
  combobench for SIMT us/att.)
- Also report UA4 enumeration cost ns/board (expect well under 1 us).
- Expected: SIMT ~8-9% us/att improvement gross, scalar ~11%. If the measured win is
  far off the by-nodes prediction, stop and investigate before stage 2 (likely suspects:
  node-share vs cycle-share mismatch in the prober, or filter checks landing off the
  hot path).

## 4. Stage 2 — full 2-digit library

Enumeration intent (sizes 4..18, supersedes stage 1's enumeration — UA4s are the
2-cycles): for each of the 36 digit pairs {a,b}, the 18 cells holding a or b decompose
by the column permutation (per row: col(a) -> col(b)) into cycles. Swapping a<->b on a
subset of cells is valid iff the subset is row/column-closed (a union of cycles) and
box-balanced (every box has equally many a-cells and b-cells in the subset). The
library is the minimal box-balanced unions of cycles per digit pair. Brute force over
cycle subsets is fine (few cycles per pair); keep only minimal ones. Reuse the
M-UA-LIB counter's enumeration — it produced the 55 UAs/board anchor.

Decision gate BEFORE landing — measure enumeration cost us/board:

- The increment over UA4-only is ~+12.4% of the revert pool ~= +1.8 us/att on SIMT,
  +8 us/att on scalar.
- Scalar: full library is unambiguous (build ~1-2 us << 8 us). Always land scalar.
- SIMT: full library only beats UA4-only if the build stays around <= 1 us/board.
  If it lands above, ship tier-split (scalar full, SIMT UA4-only) — tiers may differ
  per engine because the trajectory is identical regardless.

Same acceptance gates and benchmark protocol as stage 1; this stage also lands alone.

## 5. Non-goals and known dead ends

- Do NOT remove or weaken the prober. The filter passes everything it cannot certify.
- Dynamic UA learning from prober revert witnesses: dead. The discovered UA's sole
  given is the just-reverted cell, which is never retried; UAs do not transfer across
  boards.
- 3-digit UAs (size >= 6, needs ~84 subset solves per board, ~tens of us): out of
  scope. Dead on the SIMT budget; a possible later scalar-only increment — measure the
  catch increment with the M-UA-LIB harness before building anything.
- No spec-dependent behavior: the filter sits in the uniqueness gate and is identical
  for train/drill/all targets.

## 6. Reference numbers (M-UA-LIB, for sanity checks)

True catch of reverts, by count / by nodes:

  tier         clue>=33          clue<=32          ALL
  UA4-only     47.7% / 43.5%     23.4% / 23.1%     26.5% / 23.9%
  UA4+2digit   60.0% / 56.4%     35.1% / 35.5%     38.3% / 36.3%

Locked-cell census (avg cells that are sole given of some library UA, at clue K):
50 -> 1.9, 40 -> 4.2, 32 -> 7.1, 24 -> 10.0, 22 -> 10.7.

SIMT cycle split (320k att): prober 43.1% (train) / 46.0% (drill) of warp us/att;
revert-side = x0.855 => 36.8% / 39.4%. SIMT baseline us/att: 39.60 train / 37.11 drill.

## 7. Results (measured 2026-06-11)

All numbers: 40000 att (scalar) / 160000 att (SIMT, 8 lanes), seed 1, hidden-quad,
pinned core, boost off; `examples/bench` (scalar) and `examples/combobench --ua`
(SIMT). The run_attempts fingerprint is bit-identical across off/ua4/full
(0x98233825), and the full test suite + tests/ua_filter (per-engine, per-tier
trajectory identity) are green — the soundness/trajectory-identity claims all held.

Outcome: TIER-SPLIT, as section 4 anticipated. Scalar/wasm carry the full library;
SIMT carries UA4-only.

  scalar us/att:  off 98.3   ua4 86.5 (-12.0%)   full 84.2 (-14.3%)   <- full wins
  SIMT   us/att:  off 36.9   ua4 34.5 ( -6.5%)   full 36.7 (+6.4% vs ua4)  <- ua4 wins

Build cost (isolated enumeration, ns/board):
  UA4   ~0.49 us (signature scan, 162 cell visits)
  Full  ~4.0 us  (cycle decomposition; see below)

The build is what decides the split, and it is NOT the ~1-2 us section-1/4 assumed
for a naive port. The minimal 2-digit UAs are connected components of the per-unit
a/b join graph; the instrumentation counter found them with a per-cell union-find,
which costs ~8 us/board (the ~2,600 per-board `find` walks dominate — dependent-load
loops). The production enumeration instead exploits the structure: row+column joins
form a degree-2 graph, so the components are the cycles of the row permutation
pi(r) = row-of-b-in(column-of-a-in(r)) (a 9-step walk over precomputed coordinate
tables, zero finds), and box joins merge at most nine cycle ids via a tiny <=9-elt
union-find. That lands at ~4 us (all the divisions hoisted into a one-time
per-board precompute), under the scalar break-even.

At ~4 us build:
- scalar (~95 us attempt): full's extra catch (36% vs 24% of the revert pool by
  nodes) clears the +3.5 us build delta over UA4 -> full nets ~-14% vs ~-12%.
- SIMT (~35 us attempt): full's ~1.8 us extra catch does NOT clear the +3.5 us
  build delta -> full is ~+2 us WORSE than UA4. UA4-only stays (the <=1 us SIMT
  build bar is met only by the rectangle scan).

The UA4 catch matches the by-nodes prediction: count instrumentation showed revert
probe retirements drop 26.5% with ua4 (= the M-UA-LIB by-count UA4 catch) and keep
retirements unchanged. Scalar UA4 (~12%) exceeds the ~11% projection; SIMT UA4
(~6-7% net, ~8% gross) lands just under the ~8-9% projection, the gap being the
~0.5 us build on the small ~35 us SIMT attempt plus node-share vs wall-time slack.

Lesson worth keeping: "N comparisons is effectively free" is a trap — even the 162
UA4 visits cost ~0.5 us, enough to matter on the small SIMT attempt; and the full
build's algorithm (cycle decomposition, not union-find) was the whole ballgame, a
~2x swing that moved full from a loss to a win on scalar.
