# Per-operation cost of the SIMT generator primitives

Measured cost of three operations the W=8 SIMT generator path runs, in isolation:

| op | function | what it does |
|----|----------|--------------|
| **warp pass** | `warp_pass_full` (`solve/simt.rs`) | one cheap-closure pass over an 8-lane warp: naked + row/box/col hidden singles, digit-staggered, placed in one sweep |
| **gather** | `snapshot_lane` (`probe/simt.rs`) | pull ONE board out of the SoA warp — strided read of 30 `u32` (9 digits x 3 bands + 3 unsolved) from lane `l`, materialised into scalar bands |
| **scatter** | `load_query` (`solve/simt.rs`) | push ONE board into the SoA warp — strided write of 30 `u32` into lane `l` |

All numbers are for a single board / single warp-pass. Two methods were used: criterion
wall-clock (quick, but includes harness overhead) and `perf stat` cycle/instruction counts
with a differential control (the firm numbers).

Machine: Zen4, `target-cpu=native` (znver4), nightly-2026-05-29, governor `performance`,
measured frequency ~3.46 GHz. Cycle and instruction counts are frequency-independent; the ns
column is `cycles / 3.46 GHz`.

## Input pool

Both harnesses use the same fixed pool (seed `0xC0FFEE`): partial boards sampled by stripping
a full solution in random order and keeping those whose clue count lands in `[24, 54]` — the
non-minimal "working band" the warp's baseline gate actually sees mid-strip, not sparse minimal
puzzles. 8 warps x 8 lanes = 64 boards, clue spread min 24 / mean 39.5 / max 54. The warp data
(pristine + work copies ~15 KB) is L1-resident, matching how `warp_host` streams one resident
warp for a few passes before refilling.

## Method 1 — criterion wall-clock

`benches/warp_pass.rs` (run: `cargo bench --features bench --bench warp_pass`). The kernel
mutates the warp toward solved in place, so each timed call gets a fresh copy via
`iter_batched` (the clone is setup, not timed). The gather is materialised into an observable
buffer and the scatter's warps are `black_box`'d, so neither is optimised away.

| measurement | per-call time | per board / per warp-pass |
|---|---|---|
| `warp_pass_full` one pass (8 warps) | 2.041 µs | **255 ns** / warp-pass |
| `warp_pass_full` to singles fixpoint (8 warps) | 5.976 µs | 747 ns / warp, 93 ns / board |
| gather, 64 boards | 937 ns | **14.6 ns** / board |
| scatter, 64 boards | 1.236 µs | **19.3 ns** / board |
| gather+scatter round-trip, 64 boards | 1.930 µs | 30.2 ns / board |
| scalar singles closure, 64 boards | 38.35 µs | 599 ns / board |

(The last row: `FusedLogicSolver` gated to singles only, driven to fixpoint, one board at a
time. It reaches the same singles fixpoint as the warp but via a different schedule — all naked
singles to fixpoint, then row/box hidden, then col hidden — not the kernel's digit-staggered
naked+hidden fuse.)

Note an early gather attempt that XOR-folded the lanes into a scalar accumulator measured 0.6
ns/board — LLVM collapsed the 8 per-lane reads into one vectorised XOR-reduction. Materialising
the snapshot into memory (so the strided extraction + store actually happens) gives the 14.6 ns
above.

## Method 2 — `perf stat`, differential

`examples/opcost.rs` (build: `cargo build --release --features bench --example opcost`). Each
op runs in a tight loop over the L1-hot pool for `rounds` rounds; counts are taken with
`perf stat -r 5 -e task-clock,cycles,instructions`. To remove loop, re-stamp, and process
overhead, every op is paired with a `*_ctrl` twin that runs the identical loop with the op
removed, and the counts are subtracted:

```
per warp pass = (cyc[warp]    - cyc[warp_ctrl])    / passes
per gather    = (cyc[gather]  - cyc[gather_ctrl])  / boards
per scatter   = (cyc[scatter] - cyc[scatter_ctrl]) / boards
```

`warp` re-stamps the fresh pool each round and drives every warp to singles fixpoint, tallying
the exact pass count as the divisor (its control re-stamps too; the kernel reads the board so
the re-stamp is live in both and cancels). `gather` and `scatter` re-stamp nothing — `gather`
is read-only, `scatter` overwrites the whole board — so their controls are the bare loop. Raw
5-repeat counts (rounds = 50 000):

| mode | cycles | instructions | ops |
|---|---|---|---|
| warp | 940,901,116 (±0.18%) | 2,976,981,842 | 1,550,000 passes |
| warp_ctrl | 14,859,723 | 23,893,198 | — |
| gather | 165,489,503 (±1.18%) | 64,077,589 | 3,200,000 boards |
| gather_ctrl | 2,279,092 | 3,242,643 | — |
| scatter | 101,541,221 (±1.05%) | 210,209,996 | 3,200,000 boards |
| scatter_ctrl | 2,340,538 | 3,237,989 | — |

### Results

| op | cycles | instructions | IPC | ns @3.46 GHz |
|----|-------:|-------------:|----:|-------------:|
| **warp pass** (8 lanes) | **597** | 1905 | 3.19 | **172.9** |
| **gather** (1 board) | **51.0** | 19.0 | 0.37 | **14.8** |
| **scatter** (1 board) | **31.0** | 64.7 | 2.09 | **9.0** |

## Observations

- **The warp pass perf-stat number (597 cyc / 172.9 ns) matches the production `findpar-bench`
  attribution** (kernel 8.661 µs/attempt over 50.3 warp-passes/attempt = 172 ns/pass, hidden-quad
  toolbox=full, 300k attempts) almost exactly. Criterion's 255 ns/pass for the same op is ~1.5x
  higher — that gap is criterion harness + `iter_batched` clone + colder streaming, removed by
  the differential.

- **Criterion and perf agree on the gather (14.6 vs 14.8 ns) but not the scatter (19.3 vs 9.0
  ns).** Criterion overstated the scatter ~2x; the perf differential is the firm value.

- **Gather costs more than scatter** (51 vs 31 cyc) despite touching the same 30 `u32`. The
  gather is latency-bound — 19 instructions but IPC 0.37: a chain of strided lane reads feeding
  the materialised scalar board. The scatter is throughput-bound — 65 instructions at IPC 2.09:
  independent strided stores that pipeline.

## Reproduce

```
# criterion wall-clock
cargo bench --features bench --bench warp_pass

# perf-stat differential (per op, with its control)
cargo build --release --features bench --example opcost
BIN=./target/release/examples/opcost
for m in warp warp_ctrl gather gather_ctrl scatter scatter_ctrl; do
  perf stat -r 5 -e task-clock,cycles,instructions $BIN $m 50000
done
```

Harness state: `examples/opcost.rs` and the `gather_all`/`scatter_all` seams in
`bench_seam` (`lib.rs`) are present but uncommitted.
