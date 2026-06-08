# Deep-dive observation log (append-only, write-only)

Fundamental, non-obvious truths about Sudoku, this generator, and this machine,
discovered while stepping through the run_warp_unified hot core instruction by
instruction. Append-only; one observation per block, blank line between. Not read
back during the dive -- this is the raw field notebook.

Machine: AMD Ryzen 5 7640U (Zen 4 "Phoenix"), 6c/12t, L1 32KiB/core, L2 1MiB/core,
L3 16MiB shared, AVX-512. Measurement env: governor=performance, boost OFF,
perf_event_paranoid=-1. Boost off => cycle/instr counts are stable run-to-run and
cycles<->time is a fixed ratio, so attribution is clean.

---
The four study workloads (combobench, fixed work, 8 lanes x 10000 = 80000 attempts,
boost off ~3.5GHz) all sit at 42-54 us/att, ~150-186k cycles/att, ~320-384k instr/att.
HQ=hidden-quad 43.7us, WJ=w-wing+jellyfish 53.8us, XQ=xyz-wing+naked-quad 49.7us,
SN=swordfish+naked-triple 42.7us. They are all genuinely RARE (10k-80k+ attempts per
puzzle) -- which is the whole justification for tuning them: a common puzzle's cost
never dominates total generation time.

IPC is only ~2.0-2.2 on a core that retires up to 6 ops/cycle. So the hot core spends
~63-66% of its dispatch slots NOT retiring. This is a STALLED workload, not an
instruction-throughput-bound one -- cutting instruction count only helps to the extent
those instructions sit on the critical resource.

Zen-4 top-down (slots = 6 x cycles) for the four workloads:
  HQ: retiring 37.7%  frontend 17.8%  backend 29.4%  bad-spec 11.2%  smt 3.9%
  WJ: retiring 33.5%  frontend 26.3%  backend 24.3%  bad-spec 11.3%  smt 4.6%
  XQ: retiring 37.8%  frontend 19.9%  backend 27.0%  bad-spec 11.1%  smt 4.2%
  SN: retiring 36.1%  frontend 19.0%  backend 29.3%  bad-spec 11.0%  smt 4.6%
The BACKEND is the single largest stall bucket on every workload except the wing path
(WJ), where the frontend overtakes it. This OVERTURNS the long-standing memory claim
that this code is primarily "frontend-bound / op-cache / taken-branch". Frontend matters
(esp. for wings) but backend is bigger for subset/fish/quad work.

The ~4% "smt_contention" slots persist even pinned to one core with the SMT sibling idle.
That is Zen's static SMT slot partitioning, not real contention -- a fixed measurement
pedestal, not a tunable cost. Offlining the sibling would remove it but it is not work
the code does.

hidden-quad backend stall is NOT resource-token exhaustion in the main: the integer
scheduler queues (int_sch0+int_sch1 mostly) account for ~7% of cycles of full-dispatch
stall, store-queue ~2.5%, fp-reg ~1.6%, load-queue/int-phys-reg/retire-token all <1%.
The rest of the 29% backend-bound is execution LATENCY (dependent integer ALU chains fed
by loads), not queue-full. So the backend lever is shortening dependency chains / cutting
ops on the critical path, not adding more ports' worth of parallel work.

hidden-quad does ~95k L1d loads/att at ~1% miss, and only 1.6 integer DIVIDES per attempt
(ex_div essentially free -- the scan/fish per-cell divides noted in old memory are already
gone from this path). So neither cache misses nor division is a cost here; backend stall
is L1-hit load-use latency feeding dependent ALU ops.

Frontend stalls are NOT op-cache misses: op-cache hit rate is 96.5% and 95.7% of dispatched
ops come from the op-cache (4.3% decoder, loop_buffer literally unused = 0). The frontend
cost is the OP-QUEUE running empty ~12.5% of cycles -- delivery bubbles after taken
branches, not capacity misses. So the correct frontend framing is "taken-branch fetch
redirects", and the lever is taken-branch DENSITY and code layout, not I-cache/op-cache
footprint.

hidden-quad branch profile: ~24,750 retired branches/att, 50.9% taken (~12,600 taken/att,
1 taken branch per ~27 instructions), 5.37% mispredict rate => ~1330 mispredicts/att.
At ~16 cyc/mispredict that is ~21k cyc/att ~ 14% of the ~150k cyc/att, matching the ~11%
bad-spec slot bucket. Half of all mispredicts are on taken branches; return mispredicts
are negligible. Mispredict reduction is a real but third-rank lever (~11%).

Measurement gotcha (this machine, this harness): passing combobench's args through a shell
variable under `perf stat`/`taskset` silently feeds combobench NO args, so it exits at
startup (exit 2) and perf measures ~30us of process spin-up instead of the workload. The
tell is "elapsed ~0.00003s" and absurdly small counts. Always inline the example args on
the perf command line; never `$ARGS`.
Flat self-time across the four workloads is dominated by ONE function regardless of
target: solve::simt::warp_pass_full::<false> (HQ 28.1% / WJ 22.7% / XQ 24.8% / SN 28.3%).
This is the baseline logic solver's per-warp pass (singles + locked-candidates closure
over all 8 lanes). It is the shared hot core; every target runs it. A win here helps all
four workloads at once -- it is the single highest-leverage region in the codebase.

The hot path is almost entirely userspace and allocation-free: across all four profiles
the kernel + libc + allocator samples sum to <1%. No malloc/free churn, no syscalls in
the loop. The incremental DualSolverState (clear_clue/place_clue) earns this -- there is
no per-attempt heap traffic. So every cycle is the algorithm; there is no "stop allocating"
win left to find.

Subsystem split of the hot path (union over the four targets):
  baseline logic solver  solve::simt (warp_pass_full + subset_step + cached_*_subset +
      for_each_combination + run_stream + load_probe)  ~55-63%  <- the dominant half
  technique ladder  solve::techniques (fish_step up to 14% on jelly, wing_step up to 11%
      on w-wing, plus w_wing_link/eliminate_common_peers/BivalueBuckets)  0-26% by target
  grid fill  fill::fill  7-9%
  clue strip  dual_solver_state::clear_clue (+place_clue)  ~8-9%
  prober (uniqueness)  probe::simt (branch_lane + branch_cell + backtrack_lane)  ~8-10%
  driver  random_simt (run_warp_unified_impl closure + step_to_gate + start_attempt) ~8-9%
The OLD memory framing "prober is ~45-49% of the warp" is WRONG for this unified path:
the prober (probe::simt branch_*) is only ~8-10%. The baseline LOGIC SOLVER is the half
that dominates now. The memory predates the unified warp + the cheap-closure baseline.

Per-target technique weighting confirms the ladder is pay-for-what-you-force: fish_step is
14.1% on w-wing+jelly and 4.7% on swordfish+naked-triple but 0.01% on the two non-fish
targets; wing_step is 10.9% on w-wing, 3.7% on xyz, ~0 on the fish/quad-only targets;
the subset family (cached_naked/hidden_subset + for_each_combination) is ~13-19% on the
three subset-bearing targets and only ~2% on w-wing+jelly. warp_pass_full + fill +
clear_clue + run_stream + prober are the target-INDEPENDENT spine (~60% everywhere).

99% coverage universe (the regions the deep dive must cover), ranked by peak self-time:
  1 warp_pass_full <false>      solve/simt.rs        22-28%  (spine)
  2 fish_step                   solve/techniques.rs  0-14%   (fish targets)
  3 wing_step                   solve/techniques.rs  0-11%   (wing targets)
  4 subset_step                 solve/simt.rs        8-10%   (spine-ish)
  5 fill                        fill.rs              7-9%    (spine)
  6 clear_clue                  dual_solver_state.rs 7-9%    (spine)
  7 run_stream closure          solve/simt.rs        7-8%    (spine)
  8 cached_naked_subset(+cl)    solve/simt.rs        6-10%   (subset targets)
  9 run_warp_unified_impl cl#1  random_simt.rs       5-7%    (spine, baseline gate)
 10 branch_lane                 probe/simt.rs        4-6%    (spine, prober)
 11 cached_hidden_subset(+cl)   solve/simt.rs        2-5%    (subset targets)
 12 branch_cell                 probe/simt.rs        3-4%    (spine, prober)
 13 for_each_combination        solve/combinations.rs 2-4%   (subset targets)
 14 load_probe                  solve/simt.rs        2%      (spine)
 15 backtrack_lane              probe/simt.rs        1%      (spine, prober)
 16 step_to_gate / start_attempt random_simt.rs      ~2%     (spine, driver)
 tail: w_wing_link, eliminate_common_peers, BivalueBuckets::build, place_clue,
       requirement_met, xyz_wing closure  (<1% each, wing targets)
The hot path runs on TWO distinct SIMD data-parallelism axes, both AVX-512VL-encoded but
neither 512-bit-wide. (1) The strip + fill operate on ONE board, three 27-bit bands packed
in a 128-bit xmm (`Bands<_> = Simd<u32,4>`, lane 3 dead) -- parallelism is "all three
bands of one board at once". (2) warp_pass_full + the prober operate on 8 independent
attempts, `V = Simd<u32,8>` in a 256-bit ymm -- parallelism is "8 lanes (attempts) at
once". The compiler uses AVX-512VL throughout (k-mask predication `{%k}{z}`, vptestmd,
vpternlogd, vpopcntd) but the only genuine 512-bit zmm in the whole hot path is the array
memset/copy in BivalueBuckets::build. So this is an AVX-512-feature / 128-256-bit-data
machine for this code, not a 512-bit-throughput one.

warp_pass_full (the spine, 22-28% everywhere) is straight-line dependency-chained vector
ALU with almost NO internal branches: vpopcntd (from one_bit hidden-single detection, the
single biggest op family ~10-12%), vpternlogd (fused 3-input logic, good codegen),
vptestmd (mask compares), vpandd, masked vmovdqa32 (.select). It is therefore a pure
BACKEND contributor -- vector-port throughput + dependency latency -- and contributes ZERO
to bad-spec. The top-down "backend 24-29%" bucket is largely THIS function. Cutting its op
count or shortening its dependency chains is the only lever; it has no mispredict to fix
and its frontend is fine (it is one big basic block).

one_bit(x) ("exactly one bit set", the hidden-single test) is implemented branchlessly as
x!=0 & (x & x-1)==0 in the prober's warp_pass, but in warp_pass_full the hottest popcount
family is vpopcntd == count_ones. On Zen 4 vpopcntd ymm is cheap (1/cyc), so on THIS
machine the popcount form is fine -- the old memory's "popcount-free wins" is a wasm/ARM
result (no 16-bit SIMD popcount there) and does NOT apply to native; confirmed it regresses
native. Native-only tuning must not chase popcount removal.

fill (7-9%) is a recursive DFS, ~83 nodes/grid, ~1.7 backtracks, sieve(scan)-bound. The
26% sample on `vzeroupper` at fill.rs:106 is a SKID: it is the single instruction right
before the self-recursive `call fill`, and the deep recursive subtree's cost piles onto
that call/return boundary. Discount it entirely. fill's real cost is (a) sieve SIMD mask
algebra (vpor/vpternlogd/vpandn, branchless), (b) ~22% stack-spill traffic from holding 9
live xmm digit-boards across the sieve + the branch() candidate gather, (c) the
data-dependent variable-trip loops -- candidate-extract `while m!=0`, the per-cell digit
loop, the MRV capped_min_tier k-loop -- which are fill's mispredict suspects. fill is
popcount-free and gather-free (branch() gathers via vpshufd lane-extract, not vpgatherdd).

clear_clue (8-9%, the spine's strip cost) is fixed-size band-array traffic, not arithmetic.
EVERY call unconditionally clears the cell's bit across all 9 per-digit boards in BOTH the
row and column view (open_cell, ~18 board read-modify-writes), runs a 9-way survival
`.any()` scan (vptestmd, the single hottest line at ~12%), and walks the digit's remaining
clues accumulating peer masks. It pays for all 9 digits regardless of how few actually
reopen -- a structural over-computation and the clearest "compute fewer boards" lever in
the spine. place_clue (0.4%) is the cheap straight-line inverse (no survival scan). Both
are popcount/divide/gather-free, 128-bit, well-vectorized, memory-bound on the 160B/view
candidate arrays.

The prober's branch_lane (5.6%) is essentially a DOUBLE memcpy: it copies a lane's ~124B
board TWICE per branch -- once into the scalar sr/su snapshot, once into the pushed Frame
-- as strided scalar u32 movs (4-byte lane stride out of the SoA), 94% scalar moves, no
branches, no vector. This is the irreducible per-lane scalar residue of a SIMT existence
search: the branch pick and frame clone cannot be vectorized across lanes because each lane
branches independently. The two copies of the same 124 bytes are the structural cost; one
of them (snapshot then copy-into-frame) is a candidate to fuse.

branch_cell (3.5%): the compiler turned the 9-iteration "read sr[d][branchcell] for d=0..9"
candidate-mask build into an AVX-512 in-register GATHER (two {%k1}{z} masked zmm loads +
vpermi2d), because the source is strided-by-lane. The lone kmovd reading 34.66% is a
port/latency SKID on the serializing GPR->kmask->masked-load->vpermi2d chain, not a third
of the real work. The actual arithmetic (the ones/twos/threes bivalue fold) is cheap
scalar. High single-instruction percentages on kmovd/push/pop/cmp across these small
functions are skids onto serializing or call-boundary instructions; trust the source-line
rollup and the family mix, not the one hot line.

The scalar harder-technique ladder (fish/wing, 0-25% by target) is SCALAR
BIT-MANIPULATION + BRANCHES: popcnt (count_ones) and blsr/tzcnt bit-walks are the
primitives; NO divides, NO gathers, NO vector anywhere hot (the only AVX-512 is the memset
in BivalueBuckets::build). This is where the workload's bad-spec (~11%) is generated: the
inner prune/sees/identity branches usually fall through without firing (the pattern is
forced but rare), so they are value-dependent and irregular.

fish_step (up to 14%) cost is the 3-deep (swordfish) and 4-deep (jellyfish) combination
nests: loop-bound compares ~12%, incremental cover-union `or` ~12%, the count_ones != size
prune ~5%, the fish_eliminate blsr bit-walk ~7%, plus measurable stack-spill pressure in
the jellyfish nest (posv[9]/basebit[9]/union state register-pressured). The board scan is
nearly free; the cost is the combinatorial search overhead, not the eliminations.

wing_step (up to 11%) has one outsized line: the un-elided bounds check on
CellMarks::get (`marks[cell]`, cmp vs 0x51=81) at simt.rs:624 == ~12% of the function. The
board read is the one place the index bound is not proven. The rest is 16-bit CSR-bucket
load traffic (movzwl 10.6% from the u16-mask/u8-slot BivalueBuckets representation) and the
data-dependent sees/is_empty/without filter branches. The bounds check is a concrete, safe
lever (the cell index provably < 81).
The unified warp runs at 99.8% lane utilization (uwstat, L=8/16/32, train and drill alike).
The warp is essentially ALWAYS full -- all 8 lanes active on nearly every warp_pass_full
tick. This closes an entire class of lever: there is no idle-lane waste to reclaim, no
scheduling/oversubscription win left. warp_pass_full's vector work is fully amortized across
8 real attempts. The only ways to cut warp cost are (a) cheaper per-tick closure, or (b)
fewer ticks -- NOT "fill the warp better". The lean kernel (columns omitted, recovered
scalar) does +25% ticks (7.73M vs 6.19M) for the same work and loses; columns-in-the-closure
is correctly the default.

train(HiddenQuad) runs ~6.19M warp_pass_full ticks for 80k attempts = ~77 closure ticks per
attempt, each tick a full singles+column-fold closure over 8 lanes. warp_pass_full is ~28%
of ~150k cyc/att = ~42k cyc/att, so ~545 cycles per 8-lane tick (~68 cyc per lane-tick) for
the vectorized closure. An attempt fills one grid (~83 fill nodes) then strips ~81 cells,
each alts-bearing strip cell spawning a uniqueness gate (a packed DFS of several ticks +
scalar branches) and, if unique, a baseline solve (more ticks) -- the 77 ticks are spread
across all those per-attempt gates.

Cost-model shape for this machine (Zen 4, boost off, this code): per-attempt cycles decompose
into backend-bound vector regions (warp_pass_full ~28%, clear_clue ~9%, fill sieve, subset
vector) + scalar-residue regions (prober clone branch_lane ~6%, the fish/wing ladder, driver)
+ a flat ~11% bad-spec tax concentrated in the scalar ladder's data-dependent prune branches.
The binding term per region: warp_pass_full / clear_clue = backend (vector-port throughput +
L1 band traffic + dependency latency), fill = backend sieve + a little bad-spec, fish/wing =
retiring scalar bit-ops + bad-spec, branch_lane = scalar move throughput (the double clone).
At IPC ~2.1 / ~37% retiring the absolute ceiling if every stall vanished is ~2.7x, but the
stalls are inherent (dependency chains, genuinely-unpredictable rare-pattern branches), so the
reachable floor is far tighter -- this code is near it.
