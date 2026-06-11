# UA strip pre-filter — implementation plan

Status: planned (GO decision 2026-06-11, measurement M-UA-LIB). Two stages: UA4-only
first, then the full 2-digit library. Land and benchmark each stage in isolation.

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
