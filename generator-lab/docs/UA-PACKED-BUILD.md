# UA-PACKED-BUILD — packed-permutation engine for the full 2-digit UA enumeration

Status: design, not implemented. Successor to the scalar cycle-decomposition build in
`UaFilter::enumerate_2digit` (see docs/UA-FILTER.md for the filter itself). Written
intent-first: the implementing agent grounds the details in the current code.

## 1. Why

Counter-pinned build cost: UA4 502 ns, Full 3683 ns/board. The Full build's instruction
count is only ~1.3-1.7 us of throughput work, so roughly half the measured time is stalls:
the per-pair cycle walk is a serial chain of two dependent loads per step (9 steps, nothing
to overlap within a pair), and consecutive pairs share a digit, so their `lens`/`cell_uas`
read-modify-writes chain through store-to-load forwarding.

Stakes:
- Scalar runs Full in production; every ns off the build is straight e2e win (~36 boards/ms).
- SIMT runs UA4-only because the build delta (3.18 us) exceeds the incremental catch. The
  flip bar: capped-full displaces UA4 on SIMT iff
  `(build_cappedfull - build_ua4) + ~0.4us walk delta < 12.34pp x 0.14us/pp = 1.73us`,
  i.e. the capped-full build must land at <= ~1.5-1.8 us. This engine targets <= ~1 us.
  If it lands there, consider unifying to ONE production tier (capped Full on both engines,
  UA4 scan kept as a test oracle) and deleting the tier split.

## 2. Theory recap (what makes this vectorizable)

For digit pair {a, b}, the minimal UAs are the connected components of the graph joining
each unit's a-cell to its b-cell. Row+column joins alone form a degree-2 graph whose
components are the cycles of the row permutation

    pi(r) = (row of b) in (the column of a in row r)

pi is a 9-element permutation with no fixed points (a and b cannot share a cell), so every
cycle has length >= 2 and there are at most 4 cycles. Box joins afterwards only merge
cycles (one potential edge per box, between the rows of that box's a-cell and b-cell; the
edge is a self-loop when both sit in the same row). A final component covering k rows has
exactly 2k cells: for each of its rows r, the a-cell and b-cell of row r. Since an 8-row
component would leave a 1-row component (impossible), size 16 never occurs — the only
size-18 case is "all 9 rows in one component".

Everything above is over 9-element byte maps — which is exactly what `pshufb`
(`_mm_shuffle_epi8`) composes in one instruction. A 9-permutation fits a 16-byte register.

## 3. Data layout

One precompute pass over the 81 cells fills, per digit d, three 9-entry byte maps, each
stored 16-byte aligned so it loads straight into an xmm register:

- `R_d[row] = col`  (column of d in the row)
- `C_d[col] = row`  (row of d in the column; C_d is R_d's inverse)
- `B_d[box] = row`  (row of d's cell in the box)

27 tables x 16 bytes = 432 B scratch. Lane hygiene: lanes 9..15 are don't-care throughout —
every `pshufb` index that feeds lanes 0..8 is itself in 0..8, so garbage in high lanes never
propagates into low lanes. Initialize high lanes to 0x80 for tidiness and make every
lane-wise test mask to the low 9 lanes (movemask & 0x1FF). State this invariant in a comment
so nobody chases ghost lanes.

## 4. Per-pair pipeline (36 pairs)

Step 1 — compose pi (1 op):

    pi = pshufb(C_b, R_a)          // pi[r] = C_b[R_a[r]]

Step 2 — cycle labels by min-label doubling (~12 ops, the dependent chain is 4 rounds):

    L = identity [0,1,...,8,...]; P = pi
    repeat 4 times:                 // 2^4 = 16 >= 9 covers any cycle
        L = pminub(L, pshufb(L, P)) // L[r] = min label over pi^j(r), j < 2^k
        P = pshufb(P, P)            // P = pi^(2^k)

After 4 rounds `L[r]` is the minimum row index in r's cycle — a canonical cycle id, no
finds, no per-pair scratch reset.

Step 3 — single-cycle early-out (~2 ops): if L is all-zero in lanes 0..8 (pcmpeqb against
zero + movemask), the pair is one 18-cell component. Under the cap policy (section 5) the
pair is DONE: no box joins, no emission. This ends ~52-58% of pairs at ~15-18 ops total.

Step 4 — box joins (only multi-cycle pairs): gather the 9 edges' endpoint labels in two
shuffles:

    Ea = pshufb(L, B_a)            // lane bx = cid of the row holding a in box bx
    Eb = pshufb(L, B_b)

Then merge scalar: extract Ea/Eb to bytes and union over a <= 9-entry label remap `M`
(at most 4 distinct labels, at most 3 effective merges; most edges are self-loops and
skip). Path-compress M fully (trivial at this size), then apply in one shuffle:

    F = pshufb(M_packed, L)        // F[r] = final component label of row r

If F has a single distinct label in lanes 0..8, the components merged to one 18-cell set —
same drop rule as step 3 under the cap.

Step 5 — emission (scalar, only pairs with >= 2 final components, ~15/36 pairs): per final
label in order of first row occurrence, allocate a UA id; `counts[id] = 2 x (rows with that
label)`; per row r append the id to `cell_uas` of cells `(r, R_a[r])` and `(r, R_b[r])`.
KEEP THE ALLOCATION ORDER of the scalar build (ids assigned in first-row order within each
pair, pairs in the same (a, b) iteration order) so the packed and scalar builds produce
bit-identical `UaFilter` structs — that makes the differential test an `assert_eq` on the
whole struct rather than a set comparison.

## 5. Cap policy (decided by measurement, lands with or before this engine)

Catch-by-cap, by nodes (the decision metric): cap4 23.9%, cap6 32.4% (-3.9pp vs full),
cap12 36.1% (-0.29pp), cap14 36.24% (-0.06pp), full 36.3%. Size histogram per board:
4: 10.68, 6: 6.67, 8: 4.05, 10: 3.42, 12: 3.74, 14: 5.61, 18: 20.84.

Cap-14 therefore means exactly: drop 9-row (18-cell) components — 58% of the library
footprint (375/648 memberships) for 0.06pp of catch. In this engine that is the step 3/4
early-outs; no size arithmetic is needed anywhere. Dropping UAs is verdict-safe (a dropped
UA only loses catches; uncaught gates fall through to the prober), so the trajectory and
fingerprint are unchanged by construction — still pin it with the existing tier tests.
The library-size anchor test moves from ~55 to ~34 UAs/board; lens drops from exactly 8
to ~3.4 average.

## 6. Cost model

- Precompute: 81 iterations x 3 stores plus box-index math, ~0.1-0.15 us.
- ~20 single-component pairs x ~15-18 ops (latency ~12-16 cycles each, and independent
  across pairs — the OoO window overlaps them).
- ~15 emitting pairs x (~25 ops vector + small scalar merge + ~18 membership RMWs).
- ~273 surviving memberships (post-cap) x ~3 ops.

Estimate ~0.7-1.1 us total. Against the bar: ample margin to 1.5-1.8 us, so partial wins
still flip SIMT. If the first cut lands above ~1.2 us, profile before micro-tuning: the
likely residue is either the emission RMW chains (then interleave two pairs, or buffer
memberships per pair and flush) or the merge scalarization (then try the vector variant:
two propagation rounds of `M = pshufb(M, M)` after seeding M from the edge list).

Land it one change at a time: cap-14 on the existing scalar build first (its own win, and
it shrinks what the packed engine must do), then the packed engine, each with its own
before/after `ua_build_cost` + bench run.

## 7. Correctness and validation

- Keep the scalar cycle-decomposition build as oracle and fallback. Differential test:
  packed vs scalar `UaFilter` bit-identical over many seeds (enabled by the id-order rule
  in step 5).
- Existing pins carry over: `ua4_equals_full_size4` (rectangle scan = size-4 components),
  `full_uas_are_genuine` (every UA's swap is a distinct valid grid), library-size anchors
  (adjusted for the cap), `tests/ua_filter` trajectory/fingerprint identity per engine and
  tier, debug-only prober-agrees cross-check.
- The all-zero/single-label tests must mask to lanes 0..8 (movemask & 0x1FF) — the only
  place lane hygiene can bite.

## 8. Portability

- Zen4 is the perf target: SSSE3 `_mm_shuffle_epi8` (or AVX `vpshufb`) under the
  appropriate target-feature gating; check how the crate currently handles target features
  before choosing compile-time vs runtime dispatch.
- The same algorithm maps 1:1 to wasm SIMD (`i8x16.swizzle`) and NEON (`vqtbl1q_u8`) — note
  that the scalar production tier (Full) ships in the wasm cdylib, so wasm either keeps the
  scalar build (fine: correctness identical) or gets the swizzle path as a follow-up. Do
  not block the x86 win on the wasm port.

## 9. Non-goals

- Do not vectorize the walk-side filter (`caught`/`kept`): it is a gather by construction
  and already ~0.2-0.3 us/att post-cap. The cap IS the walk optimization.
- No trajectory changes of any kind: this is a build-cost rewrite of an enumeration whose
  output (post-cap) is fixed.
- 3-digit UAs stay dead (subset solves per board, wrong cost class).
