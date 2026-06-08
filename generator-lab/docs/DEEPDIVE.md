# run_warp_unified deep-dive — instruction-level analysis (native, this machine)

A meticulous, profile-ranked walk of the SIMT generator's hot core, from
`new_attempt` to accept/reject, on the four rare+slow workloads that justify
tuning. Native-only: the wasm path falls back to the scalar solver, so every
verdict here is about THIS machine. Raw field notes are in `DEEPDIVE-LOG.md`;
this is the synthesis.

Study vehicle: `combobench` (fixed work, `run_warp_unified`, 8 lanes x 10000 =
80000 attempts, yield-independent us/att). Caller reality check: `findpar`
(fixed seed, `find_puzzles`). Both bottom out in the same warp kernel.

---

## 1. The machine (Zen 4, the facts this code touches)

AMD Ryzen 5 7640U "Phoenix", 6c/12t. Per core: L1d 32 KiB, L1i 32 KiB, L2 1 MiB;
shared L3 16 MiB. AVX-512 (F/VL/BW/DQ/VNNI/VBMI2/BITALG/VPOPCNTDQ/GFNI/VAES).
`amd_lbr_v2`. Dispatch/retire width 6 ops/cycle. Op-cache delivers ~8-9 ops/cyc;
taken branches end a fetch window. Mispredict penalty ~14-18 cyc.

Measurement env: governor `performance`, **boost OFF** (so cycles<->time is a
fixed ~3.5 GHz ratio and counts are stable run-to-run), `perf_event_paranoid=-1`,
pinned `taskset -c 3`. AMD pipeline-utilization events drive the top-down
(`de_no_dispatch_per_slot.*`, `de_src_op_disp.*`, `op_cache_hit_miss.*`,
`ex_ret_ops`). Slots = 6 x cycles.

Gotchas worth keeping: `perf annotate -s <name>` does NOT match Rust generic
symbols (dump full + filter by address range from `nm -S`); passing example args
through a shell variable under perf/taskset silently drops them (combobench exits
at startup, ~30us — inline args always); the `de_no_dispatch_per_slot.smt_contention`
~4% persists pinned with the sibling idle (Zen static SMT slot partition, a fixed
pedestal, not real contention).

---

## 2. The cost model (Zen-4 top-down, per region)

The old "speedup = (8/3) x utilization / inflation" SIMT model is retired — it
compared SIMT to scalar; we now run native and absolute. The replacement is a
throughput model that, for each hot region, names the **binding term**, because an
alternative only helps if it cuts the term that actually binds.

```
cycles_region ~= max(
    uops_region / 6,                       # dispatch/retire ceiling
    sum_p (uops_on_port_p) / throughput_p, # backend: busiest exec port (vector ALU)
    critical_path_latency,                 # backend: dependency chain
    mispredicts_region * ~16,              # bad-spec
    L1_miss*~4 + L2_miss*~14 )             # memory (tiny here: ~1% L1d miss)
```

Realized per-attempt cycles = sum of the per-region binding terms. The
whole-program top-down is that sum bucketed:

| workload (80k att) | us/att | cyc/att | instr/att | IPC | MPKI | retiring | frontend | backend | bad-spec | smt |
|---|---|---|---|---|---|---|---|---|---|---|
| hidden-quad        | 43.7 | 150k | 336k | 2.24 | 4.0 | 37.7% | 17.8% | **29.4%** | 11.2% | 3.9% |
| w-wing+jellyfish   | 53.8 | 186k | 374k | 2.01 | 5.7 | 33.5% | **26.3%** | 24.3% | 11.3% | 4.6% |
| xyz-wing+naked-quad| 49.7 | 174k | 384k | 2.21 | 4.2 | 37.8% | 19.9% | **27.0%** | 11.1% | 4.2% |
| swordfish+naked-tri| 42.7 | 149k | 319k | 2.15 | 4.2 | 36.1% | 19.0% | **29.3%** | 11.0% | 4.6% |

Reading this: IPC ~2.1 on a 6-wide retire => ~63% of slots stall. **Backend is the
largest stall bucket everywhere except the wing path**, where the frontend (more
code, more taken branches in the scalar ladder) overtakes it. This overturns the
prior memory framing of this code as primarily "frontend / op-cache / taken-branch":
op-cache hit rate is 96.5% and 95.7% of ops come from it — the frontend cost is
op-queue-empty bubbles after taken branches (~12.5% of cycles on HQ), not capacity
misses. Bad-spec is a flat ~11% generated almost entirely in the scalar fish/wing
ladder's rare-pattern prune branches. Memory and divide are non-costs (1% L1d miss,
1.6 divides/att).

Absolute ceiling if every stall vanished: 1/0.37 ~= 2.7x. Unreachable — the stalls
are dependency chains and genuinely-unpredictable rare-pattern branches. This code
is near its reachable floor; see section 7.

---

## 3. The hot-loop map (new_attempt -> verdict)

`run_warp_unified_impl` drives `UnifiedWarp::run_stream` with a refill closure. ONE
warp runs BOTH the uniqueness probe and the baseline solve on the same 8 SIMD lanes,
each lane tagged probe- or baseline-mode; the kernel `warp_pass_full` (singles + LC
closure) is sound for a probe lane (extra propagation only prunes the existence
search). Per slot, per tick:

```
start_attempt -> fill (random_solution: recursive DFS full grid)
  -> step_to_gate: walk shuffled strip order, strip.strip(cell) [== clear_clue]
       alts==0 -> keep_trivial, continue              (cheap, no gate)
       alts!=0 -> pending gate, hand probe to the warp
  -> UnifiedWarp tick = warp_pass_full over 8 lanes, then per-lane service:
       probe lane:    solved->unique-exists; dead->backtrack_lane; stuck->branch_lane
       baseline lane: solved/dead->verdict; stuck->subset_step (scalar harder ladder)
  -> unique probe  => flip slot to baseline in place (reuse exported board)
  -> baseline done => apply_baseline, advance strip to next gate
  -> attempt ends  => finalize (verify best) => accept | reject; refill next attempt
```

Utilization is **99.8%** (uwstat, all L) — the warp is always full, so
`warp_pass_full`'s vector work is fully amortized across 8 real attempts. There is
no scheduling/idle-lane win left. ~77 `warp_pass_full` ticks run per attempt.

---

## 4. Profile ranking (99% coverage, union of the four)

Self-time per region; the spine (target-independent) is marked. Each target lights
a different ladder arm; the union is full coverage.

| region | file | HQ | WJ | XQ | SN | binding term |
|---|---|---|---|---|---|---|
| warp_pass_full<false> | solve/simt.rs | 28.1 | 22.7 | 24.8 | 28.3 | backend (vector port + dep latency) |
| subset family (step+cached+combos) | solve/simt.rs | 24.6 | ~2 | ~20 | ~12 | retiring scalar bitops + bad-spec |
| fish_step | solve/techniques.rs | ~0 | 14.1 | ~0 | 4.7 | retiring scalar + bad-spec (combo nests) |
| wing_step | solve/techniques.rs | ~0 | 10.9 | 3.7 | ~0 | bad-spec + retiring (get bounds-check) |
| fill | fill.rs | 9.1 | 6.9 | 8.1 | 9.4 | backend sieve + stack spills + bad-spec |
| clear_clue (+place) | dual_solver_state.rs | 8.9 | 7.4 | 8.5 | 9.2 | backend/memory band traffic (all 9 boards x2 views) |
| run_stream service + driver refill | simt.rs / random_simt.rs | ~16 | ~12 | ~13 | ~15 | scalar service loop (trailing_zeros dispatch) + strip walk |
| prober branch_lane | probe/simt.rs | 5.6 | 4.5 | 5.0 | 5.5 | scalar move throughput (double 124B clone) |
| prober branch_cell | probe/simt.rs | 3.5 | 2.7 | 3.2 | 3.6 | GPR->kmask->vpermi2d in-register gather |
| prober backtrack_lane | probe/simt.rs | 1.1 | 1.0 | 1.1 | 1.3 | scalar restore copy + Vec frame walk |
| load_probe / eliminate_common_peers / w_wing_link / BivalueBuckets | simt.rs / techniques.rs | ~2 | ~4 | ~2 | ~2 | scalar load + branch |

The whole hot path is allocation-free and syscall-free (kernel+libc+allocator <1%
of samples): the incremental `DualSolverState` earns this. There is no "stop
allocating" win to find — every cycle is the algorithm.

---

## 5. Per-region instruction-level dossiers

### 5.1 warp_pass_full<false> — the spine (#1 everywhere, ~545 cyc / 8-lane tick)

Standalone symbol `0x33b70..0x342fb`, 256-bit ymm SoA (8 lanes), AVX-512VL. One
big basic block per digit: naked-single sieve (ones/twos), row+box hidden singles
via `one_bit`, column hidden singles via the gather-free fold+broadcast
(`m | m<<9 | m<<18`), then `smear_v` placement + conflict. Hot instruction families:
`vpopcntd` (from `one_bit` / count_ones, the single biggest at ~10-12%),
`vpternlogd` (fused 3-input logic — good codegen), `vptestmd` (mask compares),
`vpandd` (candidate masking), masked `vmovdqa32 {%k}{z}` (`.select`).

Verdict: pure **backend** — vector-port throughput + dependency-chain latency.
Almost no internal branches => contributes ZERO to bad-spec, frontend is fine (one
block). On Zen 4 vpopcntd ymm is 1/cyc, so the popcount form is correct here; the
old "popcount-free wins" is a wasm/ARM-only result (no 16-bit SIMD popcount there)
and regresses native. The only levers are fewer vector ops per tick or fewer ticks.

### 5.2 The subset ladder — subset_step + cached_naked/hidden_subset + for_each_combination

The rare ~2% of baseline gates that the closure can't finish drop to a scalar
per-lane subset step: snapshot the lane, transpose to cell-major `CellMarks` (O(1)
get), build the shared `SubsetCache` once (per-unit marks + branchless 9x9 position
transpose), then run naked/hidden pair..quad first-fire. Dominated by scalar
bit-ops (mark unions, count_ones, the combination iterator). Heavy on HQ/XQ because
the forced quad makes every gate reach the ladder. Cache-once already landed; the
residual is the enum ladder + per-unit re-gather, now O(1) so low ceiling. Binding
term: retiring scalar + some bad-spec.

### 5.3 fish_step — the combination-nest cost (WJ 14%, SN 4.7%)

Symbol `0x28bb0`, 7001 bytes. One `FishPositions::scan` per stall (no per-cell
divide), then per (digit, orientation) a nested combination loop (2/3/4-deep for
X-Wing/Swordfish/Jellyfish) building the cover-union incrementally with
`count_ones` prefix-pruning. Cost is the **search overhead, not the eliminations**:
loop-bound compares ~12%, incremental union `or` ~12%, prune `popcnt` ~5%,
`fish_eliminate` blsr bit-walk ~7%, plus stack-spill pressure in the 4-deep
jellyfish nest (posv/basebit/union register-pressured). u16 masks in GPRs, no
divide/gather/vector. Binding term: retiring scalar + bad-spec (the prune branches
are value-dependent and usually fall through on the rare pattern).

### 5.4 wing_step — the bounds-check standout (WJ 11%, XQ 3.7%)

Symbol `0x2c160`, 5403 bytes. One bivalue scan -> `BivalueBuckets` (CSR over 36
pairs, O(1) partner lookup), then xy/xyz/w dispatch. The single biggest line is
**`cmp $0x51,%rdi; jae <panic>` at simt.rs:624 = ~12%** — the un-elided bounds
check on `CellMarks::get`'s `marks[cell]` (verified in objdump: bound 81, then
`movzwl 0x54(%rbx,%rdi,2)`). The cell index is provably < 81 (it comes from UNITS /
cell iteration), so `get_unchecked` is a safe lever. Rest: 16-bit CSR-bucket load
traffic (movzwl 10.6%) + data-dependent sees/is_empty/without filter branches (the
bad-spec source). No divide/gather; the only AVX-512 is a zmm stack spill.

### 5.5 fill — the recursive DFS (~9%, spine)

Recursive DFS, digit-transposed cell-sets in 128-bit xmm (`Bands = Simd<u32,4>`,
3 active bands), sieve+MRV, popcount-free, ~83 nodes/grid, ~1.7 backtracks. The
26% sample on `vzeroupper` at fill.rs:106 is a **skid onto the self-recursive call
boundary** (verified: `e8` call byte follows it) — discount it. Real cost: sieve
SIMD mask algebra (vpor/vpternlogd/vpandn, branchless), ~22% stack-spill traffic
from holding 9 live xmm digit-boards across the sieve + `branch()` candidate gather
(vpshufd lane-extract, not vpgatherdd), and the data-dependent variable-trip loops
(candidate-extract `while m!=0`, the digit loop, the MRV k-loop) — fill's
mispredict suspects. Binding term: backend sieve + a little bad-spec + spills.

### 5.6 clear_clue — the strip's fixed band traffic (~9%, spine)

Fully inlined (no standalone symbol). `DualSolverState` = row view + col view, each
`PerDigit<Bands>` + unsolved (128-bit xmm). Every call: drop the clue, a 9-way
survival `.any()` scan (vptestmd, the hottest line ~12%), `open_cell` on BOTH views
(unconditionally clears the cell's bit across all 9 per-digit boards then sets
survivors — ~18 board read-modify-writes), and a digit-reopen peer-mask walk. The
cost is fixed per-call traffic over the two 160B candidate arrays, **paid for all 9
digits regardless of how few reopen** — the clearest "compute fewer boards" lever in
the spine. `place_clue` (0.4%) is the cheap straight-line inverse. Binding term:
backend/memory (L1 band RMW).

### 5.7 prober branch_lane / branch_cell / backtrack_lane (~10%, spine)

`branch_lane` (5.6%) is a **double memcpy**: it copies a lane's ~124B board twice
per branch — into the scalar `sr/su` snapshot AND into the pushed `Frame` — as
strided scalar u32 movs (4-byte lane stride out of the SoA). 94% scalar moves, no
branches, no vector — the irreducible per-lane scalar residue of a SIMT existence
search (each lane branches independently, so the pick/clone can't be vectorized
across lanes). `branch_cell` (3.5%): the compiler emitted an AVX-512 in-register
gather (two `{%k1}{z}` masked zmm loads + `vpermi2d`) to collect the 9 per-digit
candidate words at the branch cell; the lone `kmovd` reading 34.66% is a port skid
on the serializing GPR->kmask->load->permute chain, not a third of the work — the
real arithmetic (bivalue fold) is cheap scalar. `backtrack_lane` (1.1%):
prologue/epilogue + strided restore copy + Vec frame-stack walk.

---

## 6. Memory reconciliation

- **`project_simt_cost_model` (the (8/3)*util/inflation model): RETIRED.** Replaced
  by the Zen-4 top-down throughput model (section 2). The Amdahl framing assumed
  prober+baseline were the two halves; the unified warp makes the baseline closure
  the dominant half and runs at 99.8% util (inflation/util are no longer the knobs).
- **"prober is ~45-49% of the warp": WRONG for run_warp_unified.** The prober
  (probe::simt branch_*) is ~8-10%; the baseline logic solver (warp_pass_full +
  subsets) is the dominant half. That memory predates the unified warp.
- **"frontend-bound / op-cache / taken-branch is the primary story": demoted.**
  Backend is the largest stall bucket for 3 of 4 workloads; op-cache hits 96.5%.
  Frontend leads only on the wing path.
- **"popcount-free wins": confirmed wasm/ARM-only, regresses native.** Native
  vpopcntd ymm is cheap; do not chase popcount removal on this machine.
- **"do not remove the prober" / incremental DualSolverState / cache-once subsets /
  wing bivalue bucketing / PEER_MASK: all confirmed landed and load-bearing.**

---

## 7. Optimization candidates (ranked; predicted gain, risk, validation)

This codebase is heavily pre-optimized; the realistic outcome of the dive is
exhaustive documentation plus a short list of small, honest levers. None is a free
lunch — each is gated on the binding term it cuts.

1. **wing_step `CellMarks::get` -> `get_unchecked`.** Cuts the ~12%-of-wing_step
   bounds check (verified real). Gain ~1-1.3% on wing targets only. Risk: low
   (cell provably < 81; keep `get` checked elsewhere). Validate: interleaved A/B on
   combobench w-wing+jelly, fp-identical.
2. **clear_clue: clear only surviving/changed digit boards, not all 9 x2 views.**
   The hottest spine traffic clears 9 boards unconditionally. Gain potentially a few
   % of the ~9% strip cost. Risk: medium (must preserve exact reopen semantics).
   Validate: fp via combobench + findpar.
3. **branch_lane: fuse the two 124B copies** (snapshot directly into the Frame, use
   it as the working copy). Gain up to ~half of 5.6% on the prober. Risk: medium —
   memory records snapshot-reorder as ILP-defeating; this is a different fusion but
   related, so measure carefully.
4. **fish_step jellyfish-nest register pressure** (cut posv/basebit spills). Gain a
   fraction of fish_step on jelly only. Risk: hard (compiler-driven).

Levers explicitly CLOSED by the data: warp scheduling / idle-lane fill (99.8%
util); the lean column-recovery kernel (+25% ticks, loses); popcount removal
(native-regressing); divide/cache/gather elimination (non-costs here).

The strongest defensible conclusion: on this machine the kernel is backend-bound on
genuine vector work (warp_pass_full) and band traffic (clear_clue) plus an
irreducible scalar per-branch clone, with a flat ~11% bad-spec tax inherent to
searching for rare forced patterns. The remaining wins are small and local.
