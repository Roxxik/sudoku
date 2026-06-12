# UA-PACKED-BUILD — packed-permutation engine for the full 2-digit UA enumeration

Status: LANDED (x86_64). `UaFilter::enumerate_2digit_packed` is the production
`UaTier::Full` build on x86_64 + SSSE3; the scalar `enumerate_2digit` stays as the
other-arch/wasm path and the differential-test oracle. Section 11 has the measured outcome;
the original intent-first design is below. Successor to the scalar cycle-decomposition build
in `UaFilter::enumerate_2digit` (see docs/UA-FILTER.md for the filter itself).

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

## 10. Measured baseline (post-cap) — profiling the shipped scalar build

Added after the fact: section 1's `3683 ns` is the **pre-cap** Full build. This doc was
written before the cap-14 skip landed (commit `6f64da3`, which reports `3.68 -> 3.15
us/board`); the diagnosis in section 1 still holds, but the numbers a future
implementer benchmarks against are the post-cap ones below. Zen4, single core pinned,
build isolated from `run_attempts` (pre-gen the solutions, rebuild `UaTier::Full` in a
tight loop so `random_solution` stays out of the timed/profiled region — `ua_build_cost`
already does the timed half; the perf half needs a rebuild loop).

Reproduced build cost (5 seeds, best-of): UA4 ~482-503 ns/board (section 1's 502 holds);
**Full ~3.06-3.10 us/board** at 34.2 UAs/board (the cap-14 library). `perf stat` over the
Full hot loop, per board: **~11.2k cycles, ~21.4k instructions, IPC 1.92**, L1-dcache
load-miss **0.06%** (no cache misses — purely latency/throughput bound, exactly as section
1 assumes), frontend-stall ~14.7% of cycles, branch-miss **126/board (~2.3%)**.

What the premise gets right:
- It IS stall-bound, not memory-bound (IPC 1.92 against Zen4's ~6-wide retire, zero cache
  misses). "Roughly half is stalls" is fair against a ~4-IPC throughput model.
- The per-pair cycle walk (`random.rs:408-419`) is the #1 hot region by cycles, **~21%**,
  and `r = row_in_col[col_of[r][a]][b]` (line 417) is two genuinely dependent serial loads
  per step, as claimed. Box-join `root()` union-find adds ~11%. Precompute and emission are
  throughput-diffuse (their instructions land in inlined core helpers — bounds checks, slice
  indexing — no single hot instruction), not stall-concentrated.

Two corrections to the section-1 mechanism list:
- **Branch mispredicts are a stall source section 1 omits**, comparable to the dependent
  loads: ~126 misses/board ~= ~17% of all cycles. Sampling `branch-misses` and attributing
  to source, the walk's variable trip count dominates — `random.rs:413` (`while !visited[r]`)
  alone is **9.5% of all misses** (the single largest), the walk region ~19%, `root()`
  (`378`) ~5%, emission's alloc/capacity branches (`462`/`474`) ~8%. The pshufb engine is
  branchless, so it removes this incidentally — the conclusion survives, the diagnosis was
  just half the stall story.
- The **emission store-to-load-forwarding claim is not borne out.** Emission is not a
  concentrated cycles hotspot; its cost is diffuse instruction count plus the unpredictable
  branches above, not a visible `lens`/`cell_uas` forwarding chain. Down-weight it.

Implication for the ~1 us target: at ~21.4k instructions/board, even a *stall-free* build at
~3 IPC lands near ~1.9 us. Hitting <= ~1 us therefore requires cutting **instruction count**
(the 9-step scalar walk -> ~12 vector ops), not merely removing stalls — which is exactly
what the packed engine does, so section 6's estimate stands. The flip-bar math in section 2
is unaffected: it constrains the *future* packed build (`build_cappedfull`), not the scalar
status quo, so the stale 3683 never enters it.

## 11. Measured outcome (landed 2026-06-11, x86_64/Zen4)

`enumerate_2digit_packed` is live for `UaTier::Full` on x86_64 + SSSE3
(`cfg(all(target_arch = "x86_64", target_feature = "ssse3"))`, met by the crate's
`-C target-cpu=native`). Scalar `enumerate_2digit` is the wasm/other-arch path and the
`packed_equals_scalar` differential oracle. Output is **bit-identical** to the scalar build
(`packed_equals_scalar` asserts equal `nua`/`counts`/`cell_uas`/`lens` over 200 seeds), so the
`run_attempts` fingerprint is unchanged (`0x4621f425` across off/ua4/full, both modes; the
`tests/ua_filter` per-engine/per-tier trajectory-identity pins stay green).

Build cost (`examples/bench`, seed 1, 8000 att, pinned core; isolated pooled loop in
parentheses): scalar Full **~3.36 us/board (~3.06)** -> packed Full **~2.44 us/board (~2.33)**,
**~27%** off at the same 34.3 UAs/board. Scalar e2e `full` improves ~83.1 -> ~82.0 us/att
(train), ~79.1 -> ~78.3 (drill).

An implementation note worth keeping: portable `Simd::<u8, 16>::swizzle_dyn` does **not** lower
to a single `pshufb` on this AVX-512 target — its "index >= 16 -> 0" contract scalarizes into
long masked `vpor`/`vpand`/`vpternlogd` blends, and a first cut using it measured **~5.37
us/board (worse than scalar)**. Switching to the raw `_mm_shuffle_epi8` intrinsic (which zeroes
on the index high bit, matching the `0x80` high-lane fill) restored the win — 9 `pshufb`/pair,
one instruction each. Use the intrinsic, not `swizzle_dyn`, for any port of this.

Where the remaining ~2.4 us goes (perf-annotated, fill amortized out so build is ~99% of the
process): the hot instructions are **all in the scalar tail** — the box-join `root()`
union-find and the emission (`alloc_ua` cap check, `counts += 2`, the `cell_uas` byte stores).
The vector min-doubling is essentially free. This confirms the mechanism: the packed engine
removed *only* the 9-step cycle walk (section 10's #1 hot region, ~21% of cycles + ~19% of
branch-misses), which is exactly the ~27% it delivered. The box-merge and emission are
**unchanged from the scalar build** — so the residue is the *shared* tail, not the new vector
code.

This lands **above** the section-2 SIMT flip bar (~1.5-1.8 us), so SIMT stays `UaTier::Ua4`
(unchanged) and no tier unification happens yet; the win is the scalar/wasm production build.
Section 10's ~1 us target was optimistic about emission: the scalar build floors near ~2 us
once the walk is gone (emission is irreducible output work — ~273 membership writes — shared
with scalar). Closing the rest is the section-6 follow-up, and a **separate** change
(one-change-at-a-time): vectorize the box-merge (`M = pshufb(M, M)` propagation over the edge
list) and/or restructure emission (buffer-and-flush, drop bounds/cap checks via the known
`<=144`/`<=8` invariants). Measure each on its own `ua_build_cost` before/after.

## 12. Follow-up landed (2026-06-11): unchecked emission

First of the section-11 follow-ups: the packed engine's emission now runs **unchecked**,
dropping the two soundness fallbacks `enumerate_2digit` carries — `alloc_ua`'s `UA_CAP`
truncation and the per-cell `UA_PER_CELL` membership cap — and the bounds checks they imply.
Both are provably dead for a 2-digit library: a board emits `<= 144` UAs (`<= 4` even
components per pair x 36 pairs, pinned by `library_sizes_match_anchors`), so `nua` never reaches
`UA_CAP` (192) and the id is never `u8::MAX`; a cell of digit `d` joins one UA per partner digit
(the 8 pairs containing `d`, once each), so `lens[cell] <= 8 = UA_PER_CELL` and the slot index is
always in range. Output is unchanged — still bit-identical to scalar (`packed_equals_scalar`,
200 seeds), fingerprint still `0x4621f425`, `tests/ua_filter` per-engine/tier identity green.
This is exactly the two scalar-tail hot spots section 11 names (`alloc_ua` cap check, the
`cell_uas` byte stores); the scalar build keeps the checks (it is the unchanged differential
oracle and the wasm/other-arch path).

Build cost (`examples/bench`, 8000 att, core-pinned, best-of, paired A/B by stashing the diff):
packed Full **~2270 -> ~2110 ns/board, ~6-7% off** at the same 34.3 UAs/board (seeds 1/2/3:
2275->2149, 2264->2097, 2275->2114). e2e shifts are within noise (the build is a small slice of
the strip). Remaining section-6 follow-ups (box-merge vectorization, emission buffer-and-flush)
are still open and stay separate, one-change-at-a-time measurements.

## 13. Follow-up landed (2026-06-11): vectorized box-merge (min-relaxation, no `root()`)

Second of the section-11 follow-ups, and the big one: the box join no longer runs the scalar
union-find. With the cycle walk and emission cap checks already gone, an isolated **pooled**
profile (fill amortized out via `ua_build_cost_pooled` / `examples/uabuildprof`, so the build is
~99% of the process) put the box-merge `root()` at **~41% of the build** — the single dominant
hot region, all of it serial dependent-load pointer-chasing (the 18 union `root()` calls + the
9 per-row resolution `root()` calls per emitting pair). Everything vectorized before it (the
min-doubling) was ~3%.

It is replaced by branchless `pshufb` **min-relaxation**. The merged components are the
connected components of the cycle edges (`pi` and its inverse `pii`) plus one box edge per box;
expressed as two row-space neighbor maps `na[r]` / `nb[r]` (both directions of every box edge),
seeding `cur = lab` (cycles already collapsed to their min row) and relaxing
`cur = min(cur, cur[pi], cur[pii], cur[na], cur[nb])` drives each lane to its component's
minimum row — the same canonical, union-order-free label `root()` produced, so emission stays
bit-identical. The `na`/`nb` maps are built scalar (a flat 9-iteration loop, no dependent
chain); `pii` is one extra `pshufb`.

`ROUNDS` (the fixed relaxation count) must reach the eccentricity of every component's min row,
and reach it *fully*: an under-converged 9-row component would fail its all-zero cap-14 drop
test and emit non-genuine UA fragments (a real correctness bug, not a verdict-safe drop). The
exact tight bound is **4**, proven by exhaustion rather than sampled. The four maps
(`pi`/`pii`/`na`/`nb`) depend only on where `a` and `b` sit, and in any completed grid that is a
pair of *disjoint single-digit placements* (one cell per row/column/box — 46656 of them). So the
**419,250,816 unordered disjoint placement pairs** are a sampling-free superset of every 2-digit
configuration any board can contain. `examples/uaroundsall` enumerates them all: max
rounds-to-fixpoint = **4** (round-4 = 746,496 pairs = 0.178%, 5+ never occurs), and round-4 is
realized in real boards (the 36M-board `examples/uarounds` sample hits it ~0.2%). Hence `ROUNDS =
4` always converges and `3` does not — exactly tight. (The generic graph bound is looser: a
9-node min-degree-2 component graph — every row sits on a `>= 2`-cycle — has eccentricity `<= 7`,
attained by a 2-2-2-3 cactus chain, but the Sudoku box structure never realizes that chain.)
Output unchanged — bit-identical to scalar (`packed_equals_scalar`, 200 seeds), fingerprint still
`0x4621f425`, `tests/ua_filter` per-engine/tier identity green.

Build cost: isolated **pooled** loop (`uabuildprof`, 256 boards x 4000 rebuilds, core-pinned,
best-of-5) **~1770 -> ~800 ns/board, ~55% off**; `examples/bench` `ua_build_cost` Full (with
fill) **~2110 -> ~1355 ns/board, ~36% off**, same 34.3 UAs/board. e2e shifts stay within noise
(the build is a small slice of the strip). Re-profiling the pooled build confirms `root()` is
gone; the new tail is the relaxation shuffles/mins (where 41% of dependent-load chasing used to
be) and the emission byte stores. This lands the build at ~800 ns/board pooled — still **above**
the section-2 SIMT flip bar, so SIMT stays `UaTier::Ua4`. The last section-6 follow-up, emission
buffer-and-flush, is still open and stays separate.

Methodology note for the next fixed-iteration-bound decision: when a loop count gates correctness
AND the relevant structure factors through a small enumerable configuration space, *exhaust the
space* (here: all disjoint placement pairs) instead of sampling boards. It turns "proven 7,
observed 4 over 36M boards" into "exactly 4" — no headroom guesswork, no rare-grid risk.

## 14. Follow-up landed (2026-06-12): emission restructure (per-digit slot counters)

Last of the section-6/11 follow-ups (the "buffer memberships per pair and flush" item), landed
in a cheaper form than buffering: the per-cell `lens` read-modify-write is deleted from emission
outright. The enabling structural fact: post-cap, **an emitting pair emits all nine rows** (every
surviving component is kept), so each emitting pair containing digit `d` appends exactly one
membership to *each* of `d`'s nine cells. The next free slot is therefore identical across a
digit's whole cell set — a single per-digit counter `cnt[d]` (number of emitting pairs so far
containing `d`), bumped twice per emitting pair. Emission's inner loop keeps only the two
`cell_uas` byte stores (slot = `cnt[a]`/`cnt[b]`, hoisted out of the row loop), and `lens` is
reconstructed after the pair loops in one trailing pass: `lens[cell] = cnt[g[cell]]`, i.e. five
`pshufb`s of the counter vector by the grid bytes plus one scalar tail cell. Same slots, same
values, same final `lens` — still bit-identical to scalar (`packed_equals_scalar`, 200 seeds),
fingerprint still `0x4621f425`, `tests/ua_filter` per-engine/tier identity green.

Measured (paired A/B by stashing the diff, one warm shell, pinned core): pooled
(`uabuildprof`, 256 boards x 4000 rebuilds) **~815 -> ~785 ns/board, ~4%**; `perf stat` puts it
entirely in instruction count — **-657 instructions/board (-6.2%), -140 cycles/board (-4.9%)**,
IPC ~3.7 unchanged, branch count unchanged. One-shot `ua_build_cost` (fresh board per build,
~1.35-1.42 us/board) and e2e are unchanged within noise.

Two honest corrections this measurement forces:
- Section 13's post-change profile put **23% of cycles on the `lens` load line**; removing the
  whole RMW recovered only ~5%. Most of that attribution was sampling skid / latency the OoO
  window already hid — the forwarding-chain story (section 6) was real but already absorbed.
  Per-line cycle attribution overstates serial-chain lines; trust the paired A/B, not annotate.
- The *full* buffer-and-flush variant (per-pair id-vector store, per-digit 8x16 transpose at
  flush) was analyzed and is estimated a wash: it moves the ~277 membership byte stores out of
  the pair loop but adds a comparable volume of transpose + scatter work at flush. Not attempted;
  estimate, not measurement.

Where the build stands (post-change profile, pooled): the vector core (min-doubling + relaxation
shuffles) ~16%, the scalar `na`/`nb` box-map build ~15%, the precompute table fill ~11%, emission
~irreducible byte stores + the small alloc loop. The named section-6 follow-up list is now
exhausted; the two NEW candidates a future change could take, separately and A/B'd on
`uabuildprof`: (a) vectorize `na`/`nb` — precompute per-digit `bx_d[r]` (box of `d`'s cell in row
`r`) and `rbox_d[bx]` (row of `d` in box `bx`) maps once per board, then `na_v =
pshufb(rbox_b, bx_a)` / `nb_v = pshufb(rbox_a, bx_b)`, two instructions replacing the 18-load
scalar gather with its `/3` arithmetic; (b) shrink the precompute — `col_of` is `r_map`
transposed (`col_of[r][d] == r_map[d][r]`), so one of the two is redundant store traffic.

**Flip-bar status correction:** section 13 claimed the ~800 ns pooled build was "still above"
the section-2 SIMT flip bar; the arithmetic says otherwise. With one-shot builds Full ~1.36 us
and UA4 ~0.58 us, the section-2 condition reads `(1.36 - 0.58) + ~0.4 = ~1.18 us < 1.73 us` —
the bar is **crossed** (and was already at section 13's numbers). Whether capped-full actually
displaces UA4 on SIMT needs the SIMT-side measurement (the bar is a cost model, not a result);
that is its own experiment, not part of this change. (Measured the next day: it does — section
15.)

## 15. SIMT flip measured and landed (2026-06-12): `UaTier::SIMT = Full`

The SIMT-side measurement section 14 called for, on the same workload that set the original
tier split (`docs/UA-FILTER.md` section 7): `combobench --force hidden-quad`, 8 lanes x 20000 =
160k attempts, seed 1, pinned core; interleaved ua4/full runs, 3 reps in each order (ua4-first
and full-first) to cancel warm-up drift. The trajectory is tier-invariant (pinned by
`tests/ua_filter`), so the A/B is pure cost:

    train: ua4 33.58 -> full 32.72 us/att  (-0.86, -2.6%)
    drill: ua4 31.03 -> full 30.04 us/att  (-0.99, -3.2%)

Full now beats UA4 on SIMT by ~0.9-1.0 us/att — a clean reversal of the original verdict (full
was +6.4% worse when its build cost ~3.7 us), and slightly better than the section-2 model's
~0.55 us margin. `UaTier::SIMT` flipped `Ua4 -> Full`; full release suite green (the
cross-engine equiv pins now exercise SIMT at Full), and the default-tier combobench path
reproduces the full-tier numbers (32.7 / 30.1 us/att). UA4 stays as the cheap-build tier and
the `ua4_equals_full_size4` oracle; the section-2 idea of *deleting* the tier split entirely
(one production tier everywhere) is now plausible but is its own cleanup, not part of this
change.

## 16. Follow-up landed (2026-06-12): vectorized `na`/`nb` box maps

Section-14 candidate (a). The precompute pass gains two per-digit 16-byte box maps —
`bx_map` (`B_d[row]` = box of `d`'s cell in that row) and `rbox_map` (`X_d[box]` = row of `d`
in that box; the scalar `row_in_box` array they subsume is deleted) — and the per-pair
`na`/`nb` build collapses to two shuffles:

    na_v = pshufb(rbox_b, bx_a)    // na[r] = row of b in the box holding r's a-cell
    nb_v = pshufb(rbox_a, bx_b)    // nb[r] = row of a in the box holding r's b-cell

replacing the per-surviving-pair 18-load scalar gather with its `/3` box arithmetic, bounds
checks, and `na`/`nb` stack buffers. Lane hygiene holds: index lanes 0..8 are boxes 0..8, so
the low-lane invariant is untouched; lanes 9..15 of `na_v`/`nb_v` zero out (the `0x80` fill),
which only the masked tests could observe and never read. Output unchanged — bit-identical to
scalar (`packed_equals_scalar`, 200 seeds), fingerprint still `0x4621f425`, `tests/ua_filter`
per-engine/tier identity green.

Measured (paired A/B by stashing the diff, one warm shell, pinned core): pooled
(`uabuildprof`, 256 boards x 4000 rebuilds, interleaved best-of-5) **~800 -> ~604 ns/board,
~-24%**; one-shot `ua_build_cost` (`examples/bench`, 8000 att) **~1354-1373 -> ~1193-1232
ns/board, ~-12%**, same 34.3 UAs/board; e2e within noise (the build is a small slice of the
strip). `perf stat` over the pooled run, per build: instructions **9.89k -> 5.53k (-44%)**,
cycles **2674 -> 2014 (-25%)**, branches 979 -> 511, branch-misses flat (~0.3/build) — the
win is pure instruction-count removal (the gather plus its bounds-check branches), the
inverse of the section-14 skid lesson: per-line cycle attribution gave this region ~15%, and
removing it bought ~25%. Attribution mis-weights in both directions; only the paired A/B
counts. IPC drops 3.70 -> 2.75, i.e. the residue is now more chain-bound (the serial shuffle
ladders), so further pure instruction shaving should be expected to buy less than 1:1 in
wall-clock.

Still open, each its own A/B'd change: section-14 candidate (b) (drop the `col_of`/`r_map`
transpose redundancy in the precompute, now ~a larger share of the smaller build — landed in
section 17) and the section-15 tier-split deletion cleanup.

## 17. Follow-up landed (2026-06-12): drop the `col_of`/`r_map` precompute redundancy

Section-14 candidate (b). The scalar `col_of[row][digit]` array (the per-row column lookup
emission reads) was exactly the transpose of the vector `r_map[digit][row]` map — both store
`col` at the same `(row, digit)` placement of a complete grid, so `col_of[r][d] == r_map[d][r]`
identically. It is deleted: emission reads the column from `r_map[a][r]` / `r_map[b][r]` directly
(a contiguous walk along each digit's 9-byte row map, replacing the old 9-byte-stride
`col_of[r][a]`), and the precompute loop drops its 81 `col_of` stores plus the array's zero-init.
Output unchanged — bit-identical to scalar (`packed_equals_scalar`, 200 seeds), fingerprint still
`0x4621f425`, `tests/ua_filter` per-engine/tier identity green, `library_sizes_match_anchors`
green.

Measured (paired A/B by building both binaries from the stashed diff, one warm shell, pinned
core, interleaved both orders): pooled (`uabuildprof`, 256 boards x 4000 rebuilds, best-of-10)
**~579 -> ~544 ns/board, ~-6%** at the same 34.7 UAs/board (every one of 20 interleaved reps had
the changed build faster). `perf stat` over the pooled run, per build: instructions **5529 ->
5374 (-156, -2.8%)**, cycles **2030 -> 1946 (-84, -4.1%)**, branches and branch-misses flat
(~511 / ~0.22 per build) — a pure instruction-count win (the deleted stores + their address math
+ the array init), no branch change, exactly the section-16 shape. One-shot `ua_build_cost`
(`examples/bench`, 8000 att) **~1187 -> ~1155 ns/board, ~-2.7%** (best-of, consistent across
reps); e2e within noise — the one-shot is diluted by the fresh-board fill traffic the pooled loop
amortizes out, so the precompute slice shows smaller there.

Still open: the section-14 candidate (a)/(b) precompute list is now exhausted; the remaining
named follow-up is the section-15 tier-split deletion cleanup (one production tier everywhere) —
landed in section 18.

## 18. Tier-split deletion (2026-06-12): one production tier (Full) everywhere

The section-15 follow-up. With Full now the production tier on *both* engines, the `UaTier`
machinery threaded a now-constant choice through every layer of the strip walk; it is removed.
**No behavior change** — this is a pure plumbing cleanup, the build engine and the produced
puzzles are untouched.

Removed:
- The `UaTier` enum and its `SCALAR`/`SIMT` constants (both were `Full`).
- The `tier` parameter from `attempt`, `StripState::new_ua`, the warp `attempt` coroutine +
  `gate_ticket`, `ua_build_cost`/`ua_build_cost_pooled`. `attempt`/`new_ua` build the full
  library directly (`UaFilter::build_full`); `StripState::new` (the diagnostic no-filter walk)
  builds `UaFilter::empty()` directly. The two constructors now share a private `build(solution,
  ua)`.
- `run_attempts_ua` (folded into `run_attempts`) and `GateStream::new_ua` (folded into
  `new_opts`); the `--ua` flag in `combobench`, the per-tier loop in `bench`. Benches run the
  production Full path.
- `UaFilter::build(sol, tier)` split into `build_full` (production, cfg-gated packed/scalar)
  and `build_ua4` (test-only).

Kept for tests (the "UA4 codepath"): `enumerate_ua4` + `build_ua4`, both
`#[cfg_attr(not(test), allow(dead_code))]` (compiled, warning-free, dead in prod — the same
convention `enumerate_2digit` uses as the x86 oracle). They survive solely as the
`ua4_equals_full_size4` differential oracle.

Soundness coverage after the cut (this is the chosen trade — see below): the strip-walk
tier-invariance test (`tests/ua_filter.rs`, which drove scalar+SIMT at Off/Ua4/Full to pin
"filter changes no verdict" live) is **deleted**. Soundness is now pinned at the build level —
`packed_equals_scalar` (200 seeds, bit-identical packed vs scalar library), `ua4_equals_full_size4`
(UA4 == full size-4 components), `full_uas_are_genuine` (every emitted UA is a genuine unavoidable
set), `library_sizes_match_anchors` (sizes sane, under `UA_CAP`) — plus the cross-engine
`tests/equiv_warp_repr` `stream_matches_scalar_per_seed` (SIMT == scalar per seed, now both at
Full). The "a filter catch only fast-rejects a gate the prober would revert" property is thereby
sound *by construction* (a caught gate is provably non-unique) rather than re-pinned by a live
Off-vs-Full fingerprint each run; the build-level bit-identity to the scalar oracle is what now
guards the library, and the cross-engine pin guards the walk.

Full release suite green (`cargo test --release -p generator-lab --lib` 22/22, incl. the four
UA oracles; `equiv_warp_repr`/`confluence` green). Build cost unchanged (`bench` ~1177 ns/board
one-shot, `uabuildprof` ~559 ns/board pooled — section-17 figures).

## 18. Parked micro-candidates (2026-06-12): what a further squeeze would try

Surveyed after section 17 (~544 ns pooled, 5374 inst, 1946 cycles, IPC 2.76 per build), not
pursued now. Sized by instruction arithmetic only — **estimates, not measurements** — and each
would be its own paired stash-diff A/B on `uabuildprof` (sections 14/16: per-line attribution
mis-weights both ways, only the A/B counts). Expectation-setting: the build is ~1.5% of scalar
e2e (~80 us/att) and ~4% of SIMT e2e (~30 us/att), so even a further -20% of the build is
~0.3% e2e at best; and the section-16 caveat stands — the residue is chain-bound, so
instruction shaving buys < 1:1 in wall-clock. In descending estimated ROI-per-effort:

(a) **Derive `bx_map` from `r_map` in the precompute** (small diff). `bx_map[d][row] =
(row/3)*3 + r_map[d][row]/3`, and a divide-by-3 over values 0..8 is a `pshufb` lookup:
`bx_vec[d] = paddb(pshufb(DIV3, r_vec[d]), ROWBASE)` with `DIV3 = [0,0,0,1,1,1,2,2,2, ...]`
and `ROWBASE = [0,0,0,3,3,3,6,6,6, 0x80 x 7]` — the `0x80` high lanes restore the hygiene
fill exactly (`r_vec`'s `0x80` lanes shuffle to 0, the add puts `0x80` back). Two vector ops
per digit (18 total) delete 81 scalar stores plus `bx_map`'s 144-byte init. The in-loop `/3`
box math stays: it feeds `rbox_map`, which remains a genuine scatter (`bx_map[d]` is a
row->box *permutation* — one cell per row and per box — and `rbox_map[d]` is its inverse;
inverting a permutation is the scatter). ~150-250 inst; by section 17's precedent
(-156 inst -> -4.1% cycles), expect ~2-4% of the build.

(b) **Per-label `counts` instead of nine RMWs in emission** (small diff). Post-cap an
emitting pair keeps every component, so a label's row count is readable straight off `cur`:
at each label's alloc site (<= 4 per pair), `rows = popcount(movemask(cmpeq(cur,
broadcast(label))) & 0x1FF)` and `counts[id] = 2 * rows` as one store — dropping the
alloc-time zeroing and the per-row `counts += 2` RMW. Same values, so bit-identity is free.
~150-200 inst on paper, but discount per section 14's lesson (the forwarding chain it removes
was already mostly absorbed by the OoO window): expect ~1-3%.

(c) **ymm 2-pair batching of the vector core** (the only structural cut left; the largest
diff by far). Pairs are independent until emission, and AVX2 `vpshufb` operates per 128-bit
lane, so two pairs ride one ymm through compose, min-doubling, and relaxation at the same
chain depth and half the shuffle count (Zen4 double-pumps 512-bit ops, so zmm 4-batching
adds little over ymm). Batch consecutive pairs sharing `a` (broadcast the `a`-side tables
once per `a`-group, one `vinserti128` per `b`-side operand), run the component tests
per half (`& 0x1FF` low lane, `& 0x1FF0000` high), run the relaxation when *either* lane
survives (~70% of batches vs ~45% of pairs — the dead lane rides free), and emit surviving
lanes in pair order so the allocation order (and the struct) stays bit-identical. Odd
`a`-group tails fall back to the existing xmm path. Net ~300 inst (~5-6%) after the
insert/broadcast overhead; the < 1:1 discount puts the wall-clock win at maybe ~2-4% —
comparable to (a) for several times the complexity.

Analyzed and dropped as a wash: a label-free single-9-cycle early-out (a fixed-point-free
9-permutation that is not a 9-cycle has a cycle of length 2, 3, or 4, so `pi^3` and `pi^4`
both fixed-point-free iff single cycle — 3 shuffles + 2 compares, ~8 ops vs the doubling's
~14) to spare the ~20 step-3 pairs the min-label doubling. But the ~16 survivors then run
the doubling *on top of* the test, and the totals net out to ~zero.

Emission's ~277 membership byte stores remain irreducible output work (the buffer-and-flush
variant was already judged a wash, section 14). Beyond (a)-(c) the build is at its floor;
the next named change is the section-15 tier-split deletion cleanup, sequenced after the
squeeze is declared done.
