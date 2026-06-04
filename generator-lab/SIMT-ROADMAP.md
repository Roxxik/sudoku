# SIMT generator architecture

The design of the packed/SIMT existence prober: pack the generator's uniqueness
prober **W puzzles per SIMD register** (one puzzle per lane) on native AVX. This
began as a measurement-phase roadmap; the prober is now **built and landed** in
this crate (`src/simt/prober.rs` + `src/simt/host.rs`) — see *Implementation status* below.
Every claim is backed by an example under `examples/` or `solver-lab/`, listed at
the end so the numbers can be re-run rather than trusted from this doc.

**Status (measured here, Zen 4 / AVX-512 w/ `vpopcntdq`, `target-cpu=native`):**
isolated packed prober **~2.75x/2.78x** per core (train/drill, `probebench`),
end-to-end **~1.55x/1.60x** (`packbench`), 0 verdict mismatches across ~103k real
queries. These are measured against the **lean single-layout `ProberBoard` scalar
prober** (the shipped/wasm path), which `probebench` confirms is ~17-20% faster
than the dual-layout prober — so the speedups here are the honest ones, not
inflated by a weak baseline. The remaining ceiling is the scalar baseline gate
(~49%, see *Decoupling* / the cost model's `S`).

The generator is embarrassingly parallel across attempts, so multi-core already
gives a free ~Nx. **Everything here is a PER-CORE multiplier on top of that**, and
it competes with simply using more cores — keep that framing when deciding whether
the rewrite is worth it.

## What we pack, and why this granularity

The hot path is the uniqueness prober (`any_alt_solves`): ~60% of attempt time, a
depth-first existence search. The baseline technique solver (~25%) is a separate
concern (see "Decoupling" below).

Pack at **granularity C: a warp of W independent per-lane DFS searches, lockstepped
at the propagation-pass level, fed by a refill queue of pending queries.** When a
lane's DFS finishes (verdict reached), it is immediately refilled with the next
query from the queue.

Rejected alternatives, with the reason:

- **Pack whole attempts (granularity A).** This is a *static* warp — lanes are
  locked to their attempt's current strip position and diverge at every position
  with no way to rebalance. Measured utilization ~0.47 at W=8. Refill is the whole
  game, and A can't refill. Dominated.
- **Flat work-queue / BFS frontier** ("propagate a pool of boards, branching =
  push children"). Tempting because it makes all warp work uniform propagation and
  sidesteps per-lane recursion. But it sacrifices DFS early-exit, and ~46% of
  queries are non-unique (find a 2nd solution and stop) and hold ~78% of all DFS
  nodes. BFS would explore the whole frontier on exactly those, inflating the
  dominant work several-fold. Keep recursive per-lane DFS with early-exit.
- **Vectorize only the branchless closures, evict branchy queries to scalar.**
  Backwards: the branchless (unique) queries are ~54% of calls but only ~22% of
  nodes; scalar would inherit the 78% bulk.

Refill drives utilization to ~1.0 (queries are plentiful — tens per attempt,
hundreds of thousands per run — and independent, so a greedy queue load-balances
them). So utilization is *not* the limiter; the scalar residue is (below).

## The cost model that governs this

```
T_packed = P · 9 · R / (W · U)  +  S
speedup  = (P_s · 9 · R_s + S_s) / (P · 9 · R / (W · U) + S)
```

- **P** = propagation passes a probe needs · **9** digit-boards. The vectorizable
  band work.
- **R** = registers/vector-ops to hold one digit-board across the warp =
  `ceil(3 · W · 32 / reg_width)`. On this machine AVX-512 is *double-pumped*, so the
  throughput width is 256-bit → at W=8, R=3. (Registers are 512-bit so pressure is
  fine; throughput is not — do not size W for 512.)
- **R_s** (scalar) is **op-dependent**, and this is the key subtlety: the current
  `band_update` is *scalar-per-band* (it extracts each band to a `u32`), so its
  R_s = 3 and it packs a full `W·U` (~7×), not `W/3·U`. The `sieve` already packs 3
  bands into one register (R_s = 1), so it only packs `W/3·U` (~8/3). The headline
  "8/3" applies to the sieve; the dominant `band_update` does better.
- **U** = utilization (≈1.0 with refill).
- **S** = the **scalar residue** — per-lane work that never amortizes: branch
  frame copy (clone), placement scatters, the `place_singles` peer accumulation.
  **S is layout-invariant and sets the ceiling**: as W→∞, speedup → T_scalar / S.
  More lanes do not help past this.

The practical consequence: the win is the uniform sweep; the ceiling is `S`
(dominated by `place_singles` and the per-branch clone). Don't expect the sweep's
raw packing factor end-to-end.

## Data layout: single-layout AoSoA

- **Single layout (row-major bands only).** The dual row/column view exists in the
  scalar engine so every unit is in-lane; the existence prober does **not** need
  it. Dropping the column view is verdict-preserving (existence completeness comes
  from branching) and removes: the column hidden-single + claiming-LC pruning
  (costs a small node inflation), **and** half the `place` work and half the
  per-branch clone (the win). Validated correct at W=1 (`miniprober`) and a net
  win even as a scalar prober (`solver-lab` `banded-sl`/`banded-sl-nolc`).
  Under SoA the column view's only real value (cheap columns) is moot anyway —
  columns within a puzzle are cross-*register*, not cross-lane.
- **AoSoA, not pure SoA.** Pure SoA (`state[word][puzzle]`) makes the sweep fast
  but puts one puzzle's ~30 state words `P` apart — cloning one lane touches ~30
  cache lines and the branch term collapses. Pack each warp's W puzzles' full
  state in one contiguous `30·W`-word block; the sweep reads it identically (one
  vector per (group,word)), but per-lane clone becomes an L1-hot strided extract
  instead of a scatter across the whole array (`aosoabench`).

One lane's state = 9 digit-bands × 3 + 1 unsolved × 3 = **30 `u32`** (single
layout).

## The vectorized primitive and the kernel

Everything is built on one primitive: **propagate W boards to a fixpoint.** It must
be gather-free (vector gathers lose to scalar repeatedly in our measurements):

- **Naked singles** — the cross-digit sieve (`ones/twos`), already pure band ALU.
- **Hidden singles** (rows + boxes, row-major in-lane) — detect exactly-one by ALU
  (`v != 0 & v & (v-1) == 0`), **not** the `SINGLE9` table (table = a gather across
  lanes). The ALU sweep is the engine and packs best (`sweepbench`).
- **`place_singles` via the smear formula** — the peer union of the placed wave as
  uniform band ops (row-smear | box-expand | column-occupancy-broadcast
  `occ * 0x40201`), **no per-cell peer gather**, plus an occupancy-popcount conflict
  check. Validated bit-exact (`placesmear`) and the in-context correctness is
  pinned by `miniprober`. NOTE: the smear is a win **only single-layout** (one
  smear, no `group_c` transpose) — it regressed the dual-layout scalar engine.
  `place_singles` is **fully uniform** this way and packs ~3x at realistic wave
  sizes (`psbench`, ~2.8x at 1 single/wave up to ~8x at 3; checksum-verified equal
  work): it loops all 9 digits explicitly, so unlike single-cell
  `place(cell,d)` there is no per-lane-varying-digit selection — each digit is a
  clean vector op across lanes, and the SIMT cost is constant (data-independent).
  The same smear trick extends to **hidden-single placements** (batch the found
  singles per digit into a group, then smear-clear — sound for an existence prober
  since placement order doesn't change the verdict).
- **No locked candidates.** LC barely prunes the existence search but runs a
  `DROP_TRIP`/`triplet_occ` lookup on every scan; dropping it is a net win *and*
  removes the only forced gather. (No-LC also already landed as a ~12% scalar win
  in `generator-lab`'s prober `propagate()`.)
- **Hidden singles via the masked place** — when a lane finds a single, placing it
  is a per-lane peer-mask scatter. This is part of the residue `S`; keep it
  branchless/masked.

The **subset ladder** (naked/hidden pair..quad) is divergent (`for_each_combination`,
popcounts) and lives only in the baseline, not the prober — it stays a scalar
fallback.

## Control flow

- **Per-lane explicit DFS stack** (no recursion): each lane owns a stack of saved
  states + (branch cell, remaining-candidate mask). On stuck → push, descend; on
  contradiction → pop to the nearest frame with a candidate left. Validated at W=1
  in `miniprober::Mini::exists`.
- **Refill queue.** A global FIFO of pending `(board, cell, alts)` queries. A lane
  that returns a verdict pulls the next. This is what makes U≈1.0.
- The branch "clone" is the per-lane state snapshot pushed on the stack — an AoSoA
  strided copy of 30 words. It regresses vs a scalar memcpy no matter what (trail
  doesn't help — `trailbench`); AoSoA just softens it. It's a bounded fraction of
  the residue; accept it.

## Decoupling from the baseline

Give the **prober its own single-layout board**; do **not** share a `BitBoard` with
the baseline. The baseline is a deterministic technique solver and genuinely needs
the column view (it stalls into the subset ladder without column hidden singles) —
removing the column view from a *shared* board regresses the whole engine even
though it helps the prober. Keep the baseline dual-layout (and optionally batch its
branchless closures on the same vectorized primitive later); keep the prober lean.

## Implementation status

The measurement phase validated each piece, the kill switch cleared its bar, and
the SIMT prober is now built:

- **Validated (W=1, scalar, microbench).** The control flow + single-layout +
  smear-place + no-LC is **correct** — 0 verdict mismatches vs the shipped prober
  across hundreds of thousands of real queries (`miniprober`). The design wins
  **even as a scalar prober** — `banded-sl-nolc` is the fastest of the banded
  family in the decoupled `solver-lab` harness; this is the win that landed here as
  `bb::ProberBoard`. The kernel pieces each have a microbench (see *Instruments*).
- **Kill switch — cleared.** `killswitch` (un-tuned integrated W=8 refill prober)
  beat the scalar prober by a clear per-core margin on real boards, so the residue
  (`S`) did not eat the closure win. Go.
- **Built — `src/simt/prober.rs` + `src/simt/host.rs`.** The W=8 packed-DFS prober:
  1. `Probe`/`PackedProber` lift the bands to `Simd<u32, 8>` SoA, fed from
     `BitBoard::export_r`.
  2. Per-lane explicit DFS stacks; the gather-free `warp_pass` kernel (naked +
     hidden singles by ALU, smear placement, occupancy-popcount conflict).
  3. **Streaming refill** (`run_stream`): each of the 8 SIMD slots streams one
     logical lane's gates and refills on demand — no two-phase barrier, no
     FIFO-depth knob, ~99% utilization at 8 lanes. `warp::run_warp` drives it from
     the host strip loop.
- **Correctness — exact.** The packed prober reproduces the 0-mismatch check vs the
  scalar prober (`probebench`, ~103k queries), and each logical lane's output is
  byte-identical to the sequential generator (`tests/equiv_warp.rs`, incl. the
  emit/verify success path). Existence is verdict-deterministic, so this is exact.

**Open direction:** pack the **baseline** gate (~49% of cost, fully deterministic,
no branching) — it is the whole remaining Amdahl ceiling now that the prober is at
its full per-core speedup, and it sits on the streaming refill's scalar critical
path.

## Instruments (re-run for current numbers)

All under `examples/` unless noted; the diagnostics need `--features count`.

The landed prober + its end-to-end diagnostics:
- `probebench` — **the headline.** Isolated prober: scalar (`ProberBoard`) vs packed
  W=8 on the real query stream, with the 0-mismatch verdict cross-check.
- `packbench`  — warp vs sequential end-to-end throughput on equal work.
- `infl`       — branch inflation + warp utilization of the actual packed prober.
- `packdiag`   — settled-by-propagation vs needed-branching split of real probes.
- `packrun`    — warp-only run, a clean profiler target (`--features profiling`).

The microbenches that validated each kernel piece (kept so the conclusions are
re-runnable per hardware):
- `miniprober` — W=1 integrated prototype; the correctness gate.
- `killswitch` — the un-tuned W=8 refill prober; the go/no-go kill switch.
- `closurekernel` — W=8 vectorized propagate-to-fixpoint on real boards vs scalar;
  the in-context kernel gate (~2.4x with hardware `vpopcntd`). No DFS/branch/refill.
- `sweepbench` — the hidden-single sweep: scalar-per-band vs SIMT-ALU vs SIMT-gather.
- `placesmear` — validates + benches the smear peer-union formula (one digit).
- `psbench`    — full 9-digit `place_singles` packing: scalar vs uniform smear.
- `pswavebench`— faithful `place_singles` peer-accumulation packing.
- `placebench` — single-cell vs wave `place` packing (the per-lane-digit problem).
- `aosoabench` — AoSoA vs SoA clone locality (the layout fix for the branch term).
- `clonebench` / `trailbench` — branch state-save: clone vs trail (both regress; a
  negative result kept to record what was tried).
- `packsim`    — the original call-level packing-efficiency predictor that motivated
  the warp.
- `solver-lab` `bench` — decoupled scalar-prober comparison; `banded`, `banded-nolc`,
  `banded-sl`, `banded-sl-nolc` are the {dual,single}×{LC,no-LC} matrix that pinned
  single-layout-no-LC as the win (now `bb::ProberBoard`).

Retired (conclusions folded into the implementation; numbers preserved above):
`inflation` (node-inflation of dropping LC/column-view — settled, the prober is
single-layout no-LC) and `warpsim` (static-vs-refill scheduler simulation —
superseded by the streaming `run_stream`, whose real utilization `infl` measures).
Both relied on a const-generic LC/HS/DUAL instrumentation harness in `bb.rs` that
was dropped to keep the shipped engine lean.

## Caveats

- **Deployment target nuance.** This is the *native* (AVX-512, double-pumped, W=8)
  analysis. The shipped generator also runs on mobile wasm (simd128 = 128-bit,
  W=4), where the packing ceiling is `W/R = 4/3` — much smaller. The SIMT rewrite
  is a native play.
- **The ceiling is the per-branch machinery**, NOT placement. `place_singles` and
  hidden-single placements uniformize via the smear (`psbench`: ~2-3x). What's left
  is the branch placement (one `(cell,digit)` per lane, varying — the smear handles
  the cell, but selecting `r[d_j]` needs a 9-register masked clear, ~neutral) and
  the per-branch clone (AoSoA strided copy, ~0.27x, regresses). Both are per-branch
  and concentrated in the ~46% non-unique queries that hold ~78% of DFS nodes — so
  pruning branches (stronger cheap propagation) is the lever that matters most, and
  the clone is the irreducible cost a recursive existence DFS pays.
- **The deployment path is a separate prober board, not a shared-board cfg.** This
  is what landed: `bb::ProberBoard` is a lean single-layout no-LC board built from
  the dual-layout `BitBoard` per query, and `BitBoard::export_r` hands the same
  row-major state to the packed prober. (An earlier whole-engine `single_layout`
  cfg that compiled out the column view for *both* prober and baseline existed only
  to measure the upper bound — it changed the generator output and was removed.)
