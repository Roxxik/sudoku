# wing_step — optimization state & next steps

`solve::techniques::wing_step` (the XY-/XYZ-/W-Wing bivalue-chain branch) runs inside
**baseline servicing**, which is the largest single slice of generator e2e. Production
toolboxes **mix branches** (no branch scoping), so every baseline solve runs the full
ladder and `wing_step` fires on essentially every stall — its cost is paid broadly, not
just on a wing-forced benchmark. This doc records what has landed and the remaining
cache-free levers, ranked, so the next session can pick up without re-profiling from zero.

## Method (reproduce before trusting any number)

Profile the SIMT path (`wing_step::<CellMarks>`), which is what `findpar-bench` exercises:

```
cargo build --release -p generator-lab --example findpar-bench
perf record -m 8 -F 2500 -o /tmp/ws.data -- \
  target/release/examples/findpar-bench --force xyz-wing --toolbox full --attempts 300000
perf report  -i /tmp/ws.data --stdio        # symbol share
perf annotate -i /tmp/ws.data --stdio --source \
  'generator_lab::solve::techniques::wing_step::<generator_lab::solve::simt::CellMarks>'
```

`-m 8` is required here (default mmap buffer fails with "Cannot allocate memory").
`--force xyz-wing --toolbox full` keeps the whole wing family hot (xy fails, then xyz,
then w). A `--force w-wing` target maximizes the conjugate-pairs path specifically.

**A/B harness.** `findpar-bench` folds an order-independent puzzle fingerprint (`fp:`)
and reports `us/att`. Any wing change that claims to be behaviour-preserving must produce
the **identical `fp`** on the same seed range. Build two binaries (stash the change for
the baseline), then interleave runs in one shell — B N B N B N — and compare medians; do
NOT block-A-then-block-B. Note zsh does not word-split unquoted vars: pass a multi-word
target with `${=tgt}`.

Correctness gates (all must stay green, `cargo test --release`):
`faithful` (train/drill {xy,xyz,w}-wing forced-spec equivalence to core,
`determinism_fp_pinned`, `verifiers_agree_on_every_verdict`), `confluence`
(`reorder_preserves_fixpoint_wings`), `logic_equiv` (bivalue toolbox), `harvest_reconstructs`.

## Landed

**Branchless `ConjugatePairs::scan`** (commit "branchless W-Wing conjugate-pairs scan").
The W-Wing strong-link table was the dominant cost inside `wing_step` (~38% of its
self-time; `wing_step` ~7.5% of total). It tallied holders with a per-cell
`trailing_zeros` loop + per-digit `match cnt` — the data-dependent-branch shape the subset
cache deliberately avoids. Replaced with the same fixed 9-wide branchless transpose
(`pos[di] |= ((row>>di)&1)<<i`, then emit on `pos[di].count_ones()==2`). Byte-identical
(same `(lower-slot, higher-slot)` pairs in unit order). **~2.3% e2e** on
`findpar-bench --force {xyz,w}-wing --toolbox full`; `wing_step::<CellMarks>` 7.54% -> 6.27%.

## Next steps (cache-free, self-contained in techniques.rs, ranked)

Post-landing internal breakdown of `wing_step::<CellMarks>` (perf annotate, weights are
indicative — annotate mis-attributes; the e2e A/B is the truth):

| area | ~% of wing_step | lever |
|---|---|---|
| `ConjugatePairs` emit `pairs[di][len]=(unit[s0],unit[s1])` | ~20% | (1) shrink storage to `(u8,u8)` |
| `cells_with_n_candidates` (`buf[k]=(c,m)` + guard) | ~12% | (2) combine bivalue+trivalue into one pass |
| `BivalueBuckets::run` slice build | ~10% | (4) prune empty-bucket lookups in xy_wing |
| `xyz_wing` `sees(pivot,c) && subset` rescan | ~6.5% | (3) bucket the xyz wing search |

### 1. `ConjugatePairs` u8 storage  (lowest risk, attacks the current #1 line)

`pairs: [[(CellIdx,CellIdx);27];9]` is `[[(usize,usize);27];9]` = **3888 bytes**, the
largest local in `wing_step` and the main reason its stack frame is ~6 KB (triggering a
stack-probe page: `sub $0x1000; movq $0,(%rsp)` at entry). Cell indices are `0..81` — they
fit in `u8`. Store the table as `[[(u8,u8);27];9]` = **486 bytes**; cast back to `usize` in
`w_wing_link` on read. Wins: 8x smaller store/zero-init on the hot emit line, smaller stack
frame (likely drops below the 4 KB probe threshold), better cache footprint. Byte-identical
output. Caveat: part of that ~20% may be the `count_ones()==2` branch (inherent), which u8
won't touch — measure, don't assume.

### 2. Combine the bivalue + trivalue scan  (low risk, simple)

`wing_step` runs `cells_with_n_candidates(v, 2, ..)` always, then a second full 81-cell
pass `cells_with_n_candidates(v, 3, ..)` when XYZ is allowed. Fuse into one 81-cell pass
that routes each empty cell into `bivbuf` (len 2) or `tribuf` (len 3) by length — saves the
second pass whenever XYZ is in the toolbox (the mixed-branch prod case). Both buffers stay
in ascending cell order, so byte-identical. Keep the single-pass (bivalue-only) form for
the XYZ-absent path, or just always collect both (tribuf is cheap when unused).

### 3. Bucket the XYZ-wing  (medium risk: reorders pair search — verify fp)

`xyz_wing` still does an O(n) rescan of every bivalue per trivalue pivot
(`for &(c,cands) in bivalues { if sees(pivot,c) && cands.without(pcands).is_empty() }`),
the one wing not yet bucketed. For pivot `{a,b,c}` the candidate wings live in exactly the
three buckets `{a,b}`, `{a,c}`, `{b,c}`, and the only firing combos are the three
cross-bucket pairs (each shares one digit = the eliminated digit, union = the pivot):
`(ab x ac)` eliminates `a`, `(ab x bc)` -> `b`, `(ac x bc)` -> `c`. Add `XYZ_WING` to
`BUCKETED` so the buckets are built when XYZ is allowed. This reorders the within-technique
pair search; per `tests/confluence.rs` the fixpoint is confluent under elimination order,
so the verdict and generator fingerprint should stay identical — but VERIFY with the `fp`
A/B (the existing xy/w bucketing established this empirically; a new reorder must re-prove
it). Note: this still needs the trivalue pivot list, so it composes with (2) rather than
replacing it.

### 4. Prune empty bucket lookups in xy_wing  (small)

`xy_wing` builds a fresh slice for `buckets.with_mask(x_bit|z_bit)` and
`...(y_bit|z_bit)` across 7 z-digits per pivot; many are empty. The `(y,z)` empty-skip is
already there; an early `(x,z)` emptiness check (or a precomputed per-slot non-empty mask
over the 36 slots, ANDed against the candidate z-digits) avoids constructing the inner
slice and the `sees(pivot,a)` loop for empty buckets. Marginal; do last, if at all.

## The big deferred lever (NOT cache-free): reuse the subset cache

A conjugate pair for digit `d` in unit `u` is exactly `positions[u][d].count_ones()==2`,
and `simt::SubsetCache.positions[u][d]` **already computes that transpose**, built in the
same `cellmarks_step_harder` call just before `wing_step`, on an unchanged board. Threading
those positions into `wing_step`/`w_wing` (mirroring how `fish_step` takes an optional
precomputed `fish_pos`) would make `ConjugatePairs` nearly free in the subset-allowed case
(i.e. mixed-branch prod) — far larger than any item above. **Deferred**: the subset cache
is being reworked in a parallel effort; wings may be moved onto the cache later. Do not
touch `SubsetCache` until that lands. See memory `project_wing_conjpairs_cache_reuse`.
