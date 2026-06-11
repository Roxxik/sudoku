# branch_lane frame-fusion — fusing the double copy (native, this machine)

The packed prober's `branch_lane` (`probe/simt.rs`) snapshots a stalled lane's board
into a pushed `Frame` at every branch node. The old form copied that board **twice**:

```rust
let (sr, su) = snapshot_lane(r, unsolved, l);          // copy 1: strided SoA read -> locals
let (cell, mask) = branch_cell(&sr, &su);
stack.push(Frame { r: sr, unsolved: su, .. });         // copy 2: locals -> Vec slot
```

The strided SoA read is the design's irreducible per-branch clone. The **second copy**
(31 words, local -> `Vec` slot) is not: the slot address is only known after `push`, so
LLVM cannot construct `sr`/`su` in place. Fusing it means pushing a placeholder frame and
snapshotting straight into its slot (`snapshot_lane_into`).

## Question

Is the second copy real, or did LLVM already elide it? Wall-clock can't tell — the copy
is a small fraction of a branch node and the signal sits inside ~2% run-to-run noise. So
the verdict is **Intel SDE `-mix`**: the exact dynamic instruction count, noise-free.

## Method

`examples/framefusebench` harvests realistic pre-branch boards from a real probe corpus
(each probe driven to its first stall, then snapshotted), then replays one branch op per
iteration. The board load (`restore_lane`) and a read-back checksum are fixed overhead
identical across variants, so any delta is the branch op alone. The checksum is also an
equivalence pin (all variants must return the same value) and a DCE guard.

Three variants were measured (single-variant SDE runs, identical corpus, 1e6 ops):

```
SDE=$HOME/opt/sde-external-10.8.0-2026-03-15-lin/sde64
B=target/release/examples/framefusebench
$SDE -mix -omix mix.<v>.txt -- $B <v> 400 1000000   # v in {base,safe,unsafe}
```

`branch_cell` is a separate symbol (so the `branch_lane` row excludes it), identical at
124.5 insts/call across all three; harness overhead identical at 88.0. So the only
difference is the branch function body.

## Result

| variant | branch fn | insts/call | vs base |
|---|---|---|---|
| base   | `branch_lane` (two copies)            | **245.0** | — |
| safe   | push zeroed frame + `snapshot_lane_into` | **134.0** | **−111 (−45%)** |
| unsafe | reserve + snapshot into uninit + `set_len` | **132.0** | **−113 (−46%)** |

Global instruction-category diff (corpus identical, so the diff is the branch body × 1e6):

```
                base -> safe   per-call
  *total        −111            the whole second copy + its local materialization
  *mem-write    −82             redundant stores (the dominant term)
  *mem-read     −46
   of which stack-write −62, stack-read −35   (sr/su staged to stack, then re-read+copied)
```

## Verdict

1. **LLVM did NOT already fuse it.** The second copy genuinely costs ~111 instructions per
   branch node, dominated by 82 redundant memory writes. The strided read is irreducible;
   the copy was not.
2. **No `unsafe` needed.** Safe (134) is within 2 insts/call of unsafe (132). The 2 are two
   64-byte `vmovups %zmm0` stores zeroing the placeholder `Frame` before `snapshot_lane_into`
   overwrites it (dead stores LLVM keeps across `push`; they hit L1 and are near-free). The
   unsafe variant avoids them by snapshotting into uninitialized capacity — not worth the
   `unsafe` block.

**Landed:** `branch_lane` now uses the safe fused form. Verdicts stay byte-identical
(`tests/prober_equiv`, `tests/faithful` pass). `examples/framefusebench` is kept as the
standing criterion for the per-branch-node cost.

## Criterion (regression guard)

Build WITH `--features profiling` so `branch_cell` stays a separate symbol (else it inlines
into `branch_lane` and the row reads ~260 = 134 + ~124):

```
cargo build --release --features profiling -p generator-lab --example framefusebench
$SDE -mix -omix mix.txt -- target/release/examples/framefusebench 400 1000000
awk '/branch_lane / && $5>0 {print $2/$5" insts/call"}' mix.txt   # expect ~134.0
```

To A/B a future change, restore the prior `branch_lane` body alongside the new one and
drive both (git history has the original three-way harness with the `BranchVariant` enum).
