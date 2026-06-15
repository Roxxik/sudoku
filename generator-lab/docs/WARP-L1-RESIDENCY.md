# Warp host L1 residency — what one in-flight attempt costs the cache

How much L1 the SIMT generator's per-attempt state occupies, what stays resident across
a warp tick, and what that says about keeping **more than W=8 attempts in flight**. The
prompting question: "the resident set is union(probing, baseline) + always-resident — how
many attempts fit in L1 before we leave it?"

Short answer: the literal "4176 B/attempt x N vs 32 KB L1d" model overstates the pressure
by ~5x. The per-*tick* hot set is dominated by the single **shared** `WarpBoards` (960 B);
the per-attempt 4176 B is mostly touched in **bursts** (per-attempt fill/verify, or
baseline-only ladder memo), not every tick, and the cold remainder tolerates L2. L1
capacity is **not** the wall for keeping more attempts in flight.

## Machine

Zen 4 "Phoenix" (Ryzen 5 7640U), per core: **L1d 32 KiB** (8-way), L1i 32 KiB,
**L2 1 MiB**, shared L3 16 MiB. Latencies ~L1 4-5 cyc, L2 ~14 cyc. Native-only path
(`generate::warp_host`); the wasm cdylib ships the scalar fallback.

## Method

- **Sizes**: a throwaway `size_of`/`align_of` dump compiled against the real types in
  `generate/warp_host.rs` + `probe/simt.rs` + `solve/simt.rs` (added, run, reverted — not
  in the tree). Every byte count below is measured, not hand-totalled, so padding and
  niche optimisation are exact.
- **Service pressure**: `findpar-bench --techstats --features count`, 300k attempts,
  `--force naked-pair --toolbox train` (a representative low-cost spec; the per-tick
  *shape* is spec-robust, only the probe/baseline *mix* shifts). The lane-state and
  service-gap histograms are exact counts.

---

## 1. Per-attempt footprint (measured)

One in-flight attempt = one `Ticket` (a `GateEngine` + the `attempt` coroutine frame),
held **inline** in the host's `[O; LANES]` occupant array — no per-lane indirection. The
warp runs `LANES = 8` of these at once, so "W=8 attempts in flight" is literally the AVX2
register width (`V = Simd<u32, 8>`), not a tunable queue depth.

| Structure | Bytes | Phase | When touched |
|---|---:|---|---|
| **`Ticket` (per lane / attempt)** | **4176** | | |
| ` GateEngine` | 1184 | | |
| `   LadderMemo` | **960** | baseline only | warm only on a baseline *ladder step* (subset/fish stall) |
| `     - pos cache (UnitPositions)` | 810 | baseline | lazily, per dirty digit |
| `     - prev / no_fire / fish` | ~146 | baseline | |
| `   SolveQuery (cached probe board)` | 160 | both | written at load, re-read at the probe->baseline flip |
| `   counts[NUM=16]` | 32 | baseline | |
| `   Vec<Frame> header` | 24 | probe | frames are on the heap (below) |
| `   allowed / baseline / pad` | 8 | | |
| ` attempt coroutine frame` | 2992 | | |
| `   StripState<RowStrip>` | 1408 | attempt | |
| `     - UaFilter` | **936** | strip walk | per-cell `caught` query; rebuilt once per attempt |
| `     - SolverState (cands+unsolved)` | 160 | strip walk | incremental strip board |
| `     - clue PerDigit<Bands>` | 144 | strip walk | |
| `     - DigitGrid + best + flags` | ~168 | attempt | |
| `   Filled (solution + byproducts)` | 816 | attempt | written by fill, read by ua-build |
| `   positions[81]` | 648 | attempt | shuffled + read per strip walk |
| `   rng / locals / coroutine state` | ~120 | | |
| **`Vec<Frame>` heap (probe DFS stack)** | live-depth x 128 | probe | cap reserves 8 KiB/lane, mostly cold |

Whole host `GateStream` = **34,400 B** = 8 x 4176 (tickets) + 960 (`WarpBoards`) + slop.
That already sits **~5% over the 32 KiB L1d** as a static footprint.

### The one thing that is shared, and hot every tick

`WarpBoards` = the SoA candidate bands the kernel streams: `r: [[Simd<u32,8>; 3]; 9]` +
`unsolved: [Simd<u32,8>; 3]` = **960 B** = 15 cache lines. `warp_pass_full` reads and
writes all of it every pass. Its size is fixed by the 8-wide warp; it does **not** grow
when you queue more attempts behind the 8 lanes (it only grows if you widen the warp or
add warps — see section 4).

---

## 2. Why "4176 B x N vs 32 KB" is the wrong model

The per-attempt 4176 B is not all hot at once. From `--techstats` (300k att, train/naked-pair):

- warp util **1.000** — 8/8 lanes busy every pass (no idle lanes).
- **stuck (= scalar-serviced) lanes/pass: mean 1.02**, ~34% of passes service zero, max 7.
- **service gap: mean 2.74 passes** between two services of the same lane.
- lane-passes/att 389 (probe 79% / baseline 21%); a full attempt spans ~389/8 ~= 49
  ticks/lane, so the heavy coroutine work (fill/verify/strip, ~44% of wall) fires in a
  **burst once per ~49 ticks per lane**, not every tick.

So the residency splits into three tiers:

- **Tier A - hot EVERY tick (the true L1 residents).** `WarpBoards` 960 B + the ~1
  serviced lane's touch (one 128 B `Frame` for a probe branch, or the `LadderMemo` dirty
  slices for a baseline ladder step). **~1-2 KB, invariant in the number of queued attempts.**
- **Tier B - warm, per service (~every 2.7 ticks/lane).** Each lane's active-phase struct.
  Over a ~3-tick window all 8 lanes are touched once, so the set that must survive to be a
  hit is ~8 x (the lane's active struct). Baseline lanes' `LadderMemo` (960 B each)
  dominate this; probe lanes contribute only a couple of `Frame`s. At the measured 79/21
  mix this is roughly `960 + ~2x960 + ~6x(few frames)` ~= **4-9 KB** — comfortably inside L1.
- **Tier C - burst, per attempt (~every 49 ticks/lane).** The 2992 B coroutine frame
  (`UaFilter` 936, `Filled` 816, `positions` 648) plus `verify` reading the final snapshot.
  This is the **bulk of the 34 KB**, and it is *cold* between bursts.

**The genuinely L1-resident hot+warm working set at W=8 is ~4-9 KB, not 34 KB.** The rest
is Tier-C cold storage that lives happily in L2 (1 MiB ~= room for ~200 attempts' full state
at L2 latency).

---

## 3. The phase-union intuition, corrected

> resident = union(probing, baseline) + always-resident; a stage move misses once.

Right shape, two corrections.

1. **The current layout *sums* the phases, it does not union them.** One `GateEngine`
   carries the probe-only `stack: Vec<Frame>` **and** the baseline-only `LadderMemo`
   (960 B) live simultaneously, although a lane is in exactly one phase at a time
   (`flip_to_baseline` never reclaims the probe stack; `load`/flip just `invalidate()` the
   memo). So you already pay for both phases at once. The flip itself is cheap — rewrite
   the lane's board via `load_query` (~120 B) + invalidate the memo — no large miss,
   *provided the destination state was already resident*.

2. **But unioning them barely helps**, because the two phases are lopsided. Probe-only
   inline state is ~24 B (the `Vec` header; frames are heap). Baseline-only is ~992 B
   (memo + counts). An `enum { Probe, Baseline }` is `max(variants) ~= 992` versus the
   current sum `~1016` — a ~24 B saving. Not worth the churn; the memo is big and
   single-sided, so the union floor is still ~the memo.

The reason the sum is tolerable anyway is Tier C above: that 960 B memo is **cold for
~79% of a lane's passes** (the probe phase never touches it), so its cache lines simply
fall to L2 when not in use. The "union" the hardware already gives you is eviction.

---

## 4. Implications for "more attempts in flight"

L1 **capacity is not the wall.** Tier A (the shared `WarpBoards`) stays ~1 KB regardless
of how many attempts you queue behind the 8 lanes; extra attempts grow only Tier B/C,
which spill to L2 and are touched in amortised bursts. *Which* "more" you mean changes the
cache story entirely:

- **Wider warp (16 lanes / zmm).** Doubles Tier A: `WarpBoards` -> 1920 B **and** 16 lanes
  streamed every pass, with a width-invariant scalar tail (Amdahl). This is the "2x L1
  footprint" half of why zmm is a wash on Zen 4 (double-pumped, no throughput gain). See
  `DEEPDIVE-LANES16.md`. Settled negative — do not re-tread.
- **Multiple 8-wide warps.** Each warp adds a full +960 B Tier-A hot set *and* 8 more
  Tier-C coroutines — linear hot-set growth. 2 warps ~= 1.9 KB hot / ~68 KB total (still
  fits L2 fine).
- **Deeper per-lane refill queue** (the `Occupant` "holds many" path). Extra attempts sit
  **cold** until promoted into a freed lane — **no Tier-A growth at all**, only L2
  pressure and a burst of misses at promotion, amortised over ~389 lane-passes. This is
  the cache-cheapest way to keep more in flight; it is also the path the killswitch refill
  work found "FIFO-depth-gated" (gated by refill depth/scheduling, **not** L1 capacity).

### Bottom line

The binding number is not `4176 B x N` against 32 KiB. It is the fixed ~1 KB `WarpBoards`
(hot every tick) plus how many lanes' active-phase structs you keep warm across the
~2.7-tick service gap (~4-9 KB at W=8). Everything else is Tier-C burst state that L2
absorbs. Keeping more attempts in flight is an L2 / scheduling question, not an L1 one.

---

## Reproduce

```
# Service-pressure histograms (lane states, stuck/pass, service gap):
cargo run --release -p generator-lab --features count --example findpar-bench -- \
    --force naked-pair --toolbox train --attempts 300000 --techstats

# Sizes: drop a #[cfg(test)] module into generate/warp_host.rs that prints
# size_of::<Ticket<GateEngine, Attempt<I>>>() etc. for the structures above,
# `cargo test -p generator-lab --lib -- --nocapture`, then revert it.
```
