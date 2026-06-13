# Ladder-step redo — plan & session findings

Status. **Step 1 (FOLD ONLY) is landed and committed** on the worktree `ladder-redo` —
measured neutral on both paths; see Step 1 below for numbers and the implementation note.
The branch is now **rebased on `master`** (which brought the `harvest` seed->puzzle
fixtures), and the yield gate has switched from the slow full-range `findpar` diff to the
fast **both-paths `harvest_reconstructs` test** (scalar + SIMT) — see Verification
methodology. **Step 2 is DONE: 2a SEPARATE landed, then 2b SHARED built + A/B'd, and the
A/B picked 2b SHARED** (output-identical; flat where the shared transpose can't fire,
~0.4% faster on the cross-branch hidden+fish specs where it does — see the 2b Outcome /
A/B verdict). **Step 3a is DONE: the hidden subset is now marks-free (positions-only),
landed as a standalone clarity + small perf win** (~0.7% faster on hidden-heavy specs,
flat where no hidden is in scope — see the Step 3a Outcome). That removes hidden's last
cell-major (`cm`) dependency, which **unblocks** the still-open Step 3b (the lazy-`cm`
reorder). A first *bundled* attempt (Step 2's shared layout, NOT the fold) was once built, verified
output-identical, measured to regress slightly, then accidentally discarded with `git
restore` (recoverable only from the session transcript — see Reconstruction notes). This
doc captures everything; the 2b adopted here is the "shared, done right" the bundle wasn't.

## Goal

Redo the SIMT scalar harder-ladder (`solve/simt.rs`: `ladder_step` /
`cellmarks_step_harder` and the cache structs) for:

1. **Clarity** — each technique reads ONE clearly-owned representation; fold the duplicated
   subset bodies into the shared `solve/techniques.rs`.
2. **Remove the `sr -> cm -> positions` double transpose** for the hidden subsets.
3. **Keep structures efficient** — no regression. A head-to-head perf comp decides
   shared-vs-separate position caches.

Primarily a clarity gain; at most ~1% e2e was expected from not re-deriving data.

## Decisions already locked (by the user)

- **Order is a free performance variable.** The specs in use are confluent and correctness
  is a fixpoint property (never trace-dependent), so technique order affects only speed.
  Reordering is allowed; yields are the gate (verify identical via `findpar` diff). There
  is no "canonical" order.
- **Fold the subset bodies** (`cached_naked_subset` / `cached_hidden_subset` in `simt.rs`)
  back into the generic `naked_subset` / `hidden_subset` in `techniques.rs`, mirroring how
  `fish_step` already takes an optional memo. The duplication is a maintenance hazard.
- **Share the position cache only if it has no cost.** A perf comparison decides
  shared-vs-separate.
- **Decompose; do not bundle.** Each change must be individually attributable
  (one-change-at-a-time). The bundled first attempt hid the regression cause.

## Representation model (target)

Two representations, each derived once from the stalled snapshot:

| Technique     | Reads                         | Natural source            |
|---------------|-------------------------------|---------------------------|
| LC            | digit-major bands             | `sr` directly (unchanged) |
| naked subset  | per-cell marks (cell-major)   | `cm` (CellMarks)          |
| hidden subset | per-unit digit positions      | digit-major, from `sr`    |
| fish          | per-line (row/col) positions  | digit-major, from `sr`    |
| wings         | per-cell marks + bivalues     | `cm`                      |

So **digit-major** = {LC, hidden subset, fish}; **cell-major** = {naked subset, wings}.
`cm` is also the shared elimination sink (techniques log `(cell, digit)` elims, replayed
into the warp lane).

## The double transpose

The hidden subsets' position masks are digit-major data currently built from the
**cell-major** `cm` (which was itself transposed from the digit-major `sr`): `sr -> cm ->
positions`. Meanwhile the fishes build the *same* row/col masks straight from `sr`. So the
row/col position masks are derived twice, once via a needless cm round-trip.

The clean removal: build the hidden-subset per-unit positions from `sr` directly
(digit-major), incrementally per dirty digit (cross-stall, in the `LadderMemo`), the same
way the fish position cache already works.

## What the bundled (discarded) attempt did, and WHY IT REGRESSED — key learning

It collapsed the fish cache and the subset positions into ONE shared `UnitPositions`
structure stored `pos[orientation][digit][line]` (+ a `boxes[digit][box]`), rebuilt
incrementally per dirty digit, feeding both fishes and hidden subsets; deleted
`SubsetCache`; naked read `cm.get` directly per size.

Output verified **byte-identical** (all `findpar` diffs empty, all `findpar-bench`
fingerprints matched, full test suite green). But interleaved `findpar-bench` (OLD master
binary vs NEW, 3 rounds, per-lane 100000) showed a **net small regression**:

| spec                      | OLD     | NEW     | delta    |
|---------------------------|---------|---------|----------|
| hidden-quad (subset-heavy)| ~31.9us | ~32.7us | **+2.3%**|
| swordfish+naked-triple    | ~30.3us | ~30.6us | +0.7%    |
| w-wing+jellyfish          | ~37.3us | ~37.6us | +0.8%    |
| xy-wing                   | ~30.7us | ~30.5us | ~0       |

Root causes (the lessons for the redo):

1. **Forced single layout strided the subset reads.** The shared store was
   per-*orientation* (fish-favoring). Hidden subsets want per-*unit* contiguous; reading
   them out of the per-orientation store (`unit()` gathering `pos[o][digit][line]` for a
   fixed line) is strided -> worst on subset-heavy `hidden-quad` (+2.3%).
2. **Unconditional both-layout / box compute.** `rebuild_digit` always computed the box
   masks even for fish-only specs (no hidden subsets in scope) -> wasted work -> the
   fish-spec regression (`w-wing+jellyfish` +0.8%).
3. **naked lost the marks-gather-share.** Switching naked from a pre-gathered per-unit
   marks cache (`SubsetCache.marks`, shared across the 3 naked sizes) to `cm.get` per size
   (3x re-gather) is a suspected additional subset-heavy cost (was NOT isolated).

**Takeaway:** each consumer needs its *natural* layout (contiguous reads); build a layout
only when its consumer is in scope; keep the marks-gather-share. Forcing one shared layout
is not free — exactly the case the "share only if no cost" steer was meant to catch.

## Decomposition plan (resume here)

### Step 0 — baselines (DONE)
Captured `findpar` yields + `findpar-bench` perf; see Artifacts.

### Step 1 — FOLD ONLY  ✅ DONE (measured neutral, committed)
Pure relocation of the subset bodies; same data flow, same layouts, byte-identical, perf
flat. This isolates "the fold is free."

**Outcome.** Landed. `findpar` byte-identical on all 5 specs; full `cargo test --release`
green (incl. `equiv_warp_repr`). Perf (interleaved OLD master vs NEW, `find` scalar /
`findpar-bench` simt):
- scalar hidden-quad (subset-heavy): 80.99 -> 80.20 us/att (**-1.0%**)
- scalar xy-wing (6 tight rounds):   78.13 -> 78.25 us/att (**+0.16%**)
- simt   hidden-quad:                 32.53 -> 32.25 us/att (**-0.87%**, fp match)

**Implementation note (deviation from the signature sketch below).** The first cut used the
exact "one body + per-unit `Option` checks" shape sketched here and it regressed scalar
hidden-quad by **+3.4%** — the non-inlined generic carried a per-unit `Option`/dynamic-
provenance cost (the `match cache` returns a reference of two possible provenances, so the
read can't stay single-source). Fix that kept the fold: **dispatch on `(cache, memo)` ONCE
at the top** into a scalar arm (fresh per-unit gather, no memo) and a cached arm (cache +
no-fire memo), both calling a shared `#[inline] naked_unit` / `hidden_unit` that holds the
combination/elimination body once. Each arm then specialises to its provenance — the scalar
arm is byte-identical to the old scalar body, the cached arm to the old `cached_*` body — so
no duplication of the hard logic and zero per-unit `Option` overhead. The planned signatures
(below) are unchanged; only the body shape differs.

- `techniques.rs`: give the generic bodies optional caches + memo, mirroring `fish_step`:
  - `naked_subset(v, size, marks: Option<&[[Mark;9];27]>, memo: Option<(&mut [u8;27], u8)>)`
    — `None` derives via `v.get` (scalar, byte-identical); `Some` reads the cache and
    no-fire-gates per unit (`ladder_bit` in `memo`).
  - `hidden_subset(v, size, pos: Option<&[[u16;9];27]>, marks: Option<&[[Mark;9];27]>, memo: ...)`
    — `None` derives from `v.get`; `Some` reads the per-unit positions + marks.
  - Add a `subset_tally(run: bool)` helper (LSTAT [1]/[2]) twin of `fish_tally`, called
    only on the memoized path.
- `simt.rs`: **keep `SubsetCache` (marks + positions from `cm`) and `FishPositions`
  exactly as today.** `cellmarks_step_harder` builds `SubsetCache`, then calls
  `techniques::naked_subset(cm, n, Some(&cache.marks), Some(memo))` /
  `hidden_subset(cm, n, Some(&cache.positions), Some(&cache.marks), Some(memo))`. Delete
  `cached_naked_subset` / `cached_hidden_subset`. Fish path unchanged.
- `logic.rs` / `fused.rs` scalar callers pass `None` for the new params.
- **Verify**: `findpar` diff = 0 (all 5 specs) + `cargo test --release`; `findpar-bench`
  interleaved OLD-vs-NEW must be flat. Commit.

### Step 2 — KILL THE DOUBLE TRANSPOSE, efficient, with the shared-vs-separate A/B
Build the hidden-subset per-unit positions from `sr` (digit-major) **incrementally in the
`LadderMemo`** (per dirty digit; cross-stall; built only when `ANY_HIDDEN` in scope), in a
**per-unit-contiguous** layout (rows/cols/boxes) so reads stay contiguous (the bundle's
mistake #1) and the box masks are built only when hidden subsets are actually in scope
(mistake #2). Keep the naked per-unit marks cache (mistake #3).

Then A/B two efficient variants (perf decides):
- **2a SEPARATE  ✅ DONE** (see Outcome below): two memo caches — `subset_pos: [[u16;9];27]`
  (per-unit, hidden) and the existing per-orientation fish cache — each rebuilt only when its
  consumer is in scope. Row/col computed twice (cheap, incremental). **Deviation from the
  sketch: TWO stale masks, not one shared `pos_stale`.** A single shared mask cannot be
  cleared correctly for two consumers at different ladder points — a subset fire returns
  before the fish rebuild, and a fishless toolbox (e.g. `hidden-quad`) never reaches the fish
  block, so a shared mask would either never clear (defeating the incremental rebuild) or
  clear before fish read it. `subset_stale` + `fish_stale`, each set by the entry diff and
  cleared right after its own rebuild loop, is the faithful SEPARATE design and preserves
  Step 1's lazy properties (subset fire skips fish maintenance; fishless skips fish entirely).
- **2b SHARED  ✅ BUILT + MEASURED** (see 2b Outcome below): one `techniques::UnitPositions`
  struct holding BOTH layouts (per-orientation `FishPositions` for fish + per-unit `subset:
  [[u16;9];27]` for hidden); one `rebuild_digit(di, rows, want_fish, want_hidden)` computes the
  column transpose ONCE and scatters into whichever layouts are in scope; one shared
  `pos_stale` mask. **Deviation from the sketch (the dual-arm rebuild placement).** A single
  rebuild point cannot serve both consumers naively: hidden needs its positions *before* the
  subset scans, fish *after*; and the 2a lesson (mistake #2) is that paying the fish transpose
  on an entry that fires at a naked subset is wasted. The faithful SHARED design therefore
  invokes the *one* `rebuild_digit` (and clears the *one* mask) at whichever consumer runs
  first: up-front in the subset block when `ANY_HIDDEN` (where the transpose is needed anyway,
  so it also fills the fish layout for free — the genuine share), else *lazily* right before
  the fishes when fish-without-hidden (so a subset fire returns first and the transpose is
  never paid — 2a's laziness preserved). Net: the transpose is computed once, never wasted.

`findpar-bench` (the 4 specs, interleaved) picks 2a vs 2b; the both-paths harvest yield gate
(`cargo test --release`, scalar + SIMT) must stay green for each.
Expectation: 2b ("shared, done right") does the row/col derivation once and should be
neutral-or-better than 2a; but the perf comp is the decider, per the user.

**2a Outcome.** Landed (worktree `ladder-redo`). `subset_pos` (rows `u` 0..9 / cols 9..18 /
boxes 18..27) built incrementally per dirty digit from the diffed `prev` bands via
`rebuild_subset_digit`, gated on `ANY_HIDDEN`; `SubsetCache` reduced to marks-only; the old
`sr -> cm -> positions` double transpose is gone. Byte-identical to Step 1 (the cm transpose
and the band-derived masks read the same per-cell candidacy with the same `UNITS[u][i]` slot
order). Verified: full `cargo test --release -p generator-lab` green incl. BOTH harvest paths
(`..._scalar` / `..._simt` — the latter drives the changed SIMT subset path over dozens of
hidden-pair/-triple specs, exercising the box masks) and `equiv_warp_repr`. Perf (interleaved
OLD Step-1 vs NEW 2a, `findpar-bench`, 3 rounds, `--attempts 400000`, median us/att; `fp`
matched OLD==NEW every spec):
- hidden-quad (subset-heavy/fishless): 31.83 -> 31.80 (**flat**)
- xy-wing (sensitivity workhorse):     30.74 -> 30.53 (**-0.7%**)
- w-wing + jellyfish (fish+wing):      37.43 -> 37.30 (**-0.3%**) — the SEPARATE row/col
  double-compute case; the discarded bundle regressed +0.8% here, 2a does NOT
- swordfish + naked-triple (mixed):    30.09 -> 30.07 (**flat**)

Net: neutral-to-slightly-faster everywhere, no regression — matching the expectation. **Next:
2b SHARED, then the 2a-vs-2b A/B decision.**

**2b Outcome.** Built on the worktree (uncommitted at the time of the A/B): `UnitPositions`
folds 2a's `fish_pos` + `subset_pos` into one cache behind one `pos_stale` mask;
`FishPositions::rebuild_digit` and `simt::rebuild_subset_digit` collapse into one
`UnitPositions::rebuild_digit` that computes the col transpose once and scatters into both
layouts (dual-arm placement above). Output-identical to 2a — full `cargo test --release -p
generator-lab` green incl. BOTH harvest paths + `equiv_warp_repr`; `findpar-bench` `fp` matched
2a==2b on all 5 specs. Perf (interleaved 2a vs 2b, **5 rounds**, `--attempts 400000`, median
us/att; `fp` matched every spec, every round):

The result keys cleanly off whether the spec's `allowed` mask co-activates `ANY_HIDDEN`
**and** `ANY_FISH` (the only case where 2b shares the transpose 2a computes twice). NB the
toolbox is the train-union of the forced targets, so a target pulls in its simpler same-branch
peers: `swordfish + naked-triple` is NOT "fish + naked" — naked-triple's Subset-branch scope
(difficulty <= 50) includes **hidden-pair** (44), so it co-activates hidden + fish just like
`hidden-triple + swordfish`.

| spec                          | `allowed` co-scope        | 2a med | 2b med | delta   |
|-------------------------------|---------------------------|--------|--------|---------|
| hidden-quad                   | Subset only (no fish)     | 31.71  | 31.69  | -0.06%  |
| xy-wing                       | Bivalue only              | 30.63  | 30.63  |  0.00%  |
| w-wing + jellyfish            | Fish + Bivalue (no hidden)| 37.29  | 37.26  | -0.08%  |
| **swordfish + naked-triple**  | **hidden-pair + fish**    | 30.20  | 30.07  | **-0.43%** |
| **hidden-triple + swordfish** | **hidden + fish**         | 31.39  | 31.26  | **-0.41%** |

**Both** hidden+fish co-scope specs show ~-0.4%; the three non-co-scope specs are flat. That is
2b's shared transpose firing — a real, repeatable signal, NOT noise (the consistent ~0.4% on
exactly the two specs where the share is reachable, and only those, is the tell). 2b is
output-identical and never slower elsewhere.

**A/B verdict — ADOPT 2b SHARED.** Perf is neutral-or-better: flat where the share can't fire,
~0.4% faster where it can. The win fires whenever a hidden subset and a basic fish are in scope
on the same stall — i.e. on any cross-branch combination spec, which is a class the generator is
growing toward (more multi-`--force` combinations planned). It is therefore NOT dormant; the
"share only if no cost" gate is passed (no cost — a small win), and 2b additionally consolidates
2a's two caches / two masks / two rebuild methods into one of each. The cost is the dual-arm
rebuild placement (up-front under `ANY_HIDDEN`, else lazy before the fishes), documented at the
2b bullet and in `cellmarks_step_harder`. Reproduce: `perf-ab/ab.sh <perf-ab-dir> 400000 5` (the
2a/2b binaries + raw results lived under the worktree's `perf-ab/`, gitignored; deleted after).

*Possible follow-up (separate, measured):* the `ANY_HIDDEN` arm rebuilds at the subset-block top
(before naked-pair), matching 2a. It could be deferred to just before the first hidden scan
(hidden-pair) to also skip the transpose when a naked subset fires first — a narrow extra
laziness, equally applicable to 2a, left out here to keep the dual-arm logic readable.

### Step 3a — hidden subset marks-free (positions-only)  ✅ DONE (cleaner + faster)
The hidden subset still read TWO representations: per-digit positions for the SEARCH
(Step 2's digit-major cache) and per-cell `marks` (gathered off `cm`) for the
ELIMINATION (`marks[i].without(keep)`). 3a folds the elimination onto the positions too,
so the hidden subset reads ONLY its positions and the marks dependency is gone:
`hidden_unit`/`hidden_subset` drop the `marks` param; `SubsetCache` (the per-unit marks
cache) is now consumed by the NAKED subsets alone.

**The win is not free-by-construction — it took three tries, perf-gated each time** (the
elimination genuinely lost the pre-gathered marks, exactly the bundle's lesson #3 risk):
1. **Naive** (per union cell, iterate all 9 digits, `combo.contains`): **+0.6% on
   hidden-quad** — a no-op exact cover (a hidden pair whose two cells hold only those two
   digits) now walked a per-cell 9-digit loop where `marks[i].without(keep).iter()` was
   zero iterations. The exact-cover block runs on EVERY cover (firing or not), so it is
   not cold. Caught immediately; this is why hidden's marks looked load-bearing.
2. **Digit-driven** (for each non-combo digit, eliminate from `positions[di] & union`):
   fixed hidden-quad (no-op covers now have `positions[di] & union == 0`, inner walk
   skipped) but **+0.5% on pair-heavy `swordfish + naked-triple`** (real, controlled
   against an old-vs-old order-bias run) — iterating all 9 digits per cover still cost,
   and pair-heavy specs hit far more covers than quad-heavy ones.
3. **Present-digit** (walk only `present & !combo`, `present` = digits with >=1 candidate
   cell, gathered for free in the existing count loop): **faster than the marks baseline.**

**Step 3a Outcome.** Landed. Byte-identical eliminations (same `(cell, digit)` SET; the
emission order is digit-major now, but the per-bit writeback in `ladder_step` and the
solve fixpoint are order-independent). Full `cargo test --release -p generator-lab` green
incl. BOTH harvest paths + `equiv_warp_repr`; `findpar-bench` `fp` matched OLD==NEW on all
specs. Perf (interleaved OLD post-2b vs NEW present-digit, `--attempts 400000`, median
us/att; read against the **`xy-wing` control = +0.23%**, the code-layout/thermal floor —
`xy-wing` has no hidden in scope so `hidden_unit` never runs and its delta is pure jitter):

| spec                          | scope                       | raw delta | vs floor |
|-------------------------------|-----------------------------|-----------|----------|
| hidden-quad                   | Subset only (heavy hidden)  | -1.13%    | **~-1.1% faster** |
| hidden-triple + swordfish     | hidden + fish               | -0.51%    | **~-0.5% faster** |
| swordfish + naked-triple      | hidden-pair + fish          | +0.23%    | ~flat    |
| xy-wing (control)             | Bivalue only (no hidden)    | +0.23%    | 0 by construction |

Net: faster on hidden-heavy specs, flat where no hidden fires. The marks gather is no
longer paid for hidden at all; `present`-restricted, digit-driven elimination beats the
per-cell `marks[i].without(keep)` walk. *Noted follow-up (separate, trivially-safe):* the
`SubsetCache` build is still gated on `ANY_SUBSET`; it could narrow to `ANY_NAKED` (marks
are now a naked-only cache), skipping the gather for a hidden-only-no-naked toolbox — but
the Subset branch pulls naked in alongside hidden, so this is dormant in production.

### Step 3b — OPTIONAL, separate, measured: lazy-`cm` reorder (now unblocked by 3a)
Order is free, so try digit-major techniques first (LC -> hidden subsets -> fishes ->
naked subsets -> wings) so the 81x9 `cm` transpose is built only when a cell-major
technique is reached. **3a unblocks this**: hidden is now positions-only, so the only
`cm`-marks consumers left are the naked subsets and wings — a stall where a hidden/fish
fires first need never build `cm`. Caveat from the 3a measurements: the win is confined to
toolboxes with nothing cell-major *forced* (a naked-forced spec like `swordfish +
naked-triple` reaches naked on most stalls and needs `cm` anyway), so A/B a hidden/fish-
pure toolbox to size it. A/B; keep only if faster; both-paths harvest yield gate green.

## Reconstruction notes (the discarded bundle)

The discarded version = `master` + the edits below (all present verbatim in the session
transcript, if an exact rebuild of the *regressed* WIP is ever wanted — but the redo should
follow Step 1/Step 2, not this):

- `techniques.rs`: `FishPositions` -> `UnitPositions` (+`boxes[[u16;9];9]` field, `unit(u)`
  accessor, `scan` also building boxes); `naked_subset` gained `Option<(&mut [u8;27], u8)>`
  memo; `hidden_subset` gained `cache: Option<&UnitPositions>` + the same memo; `fish_step`
  / `fish_sized` param `FishPositions` -> `UnitPositions`; added `subset_tally`.
- `simt.rs`: `LadderMemo.fish_pos` -> `pos: UnitPositions`, `fish_stale` -> `pos_stale`
  (struct, `INVALID`, the `ladder_step` diff, both reset spots); `cellmarks_step_harder`
  rebuilt to rebuild `pos` at the top (gated `ANY_HIDDEN | ANY_FISH`), `naked!`/`hidden!`
  macros over disjoint field borrows (`&memo.pos` + `&mut memo.no_fire`), `fish_arg` from
  the shared `pos`; **deleted** `SubsetCache` + `cached_naked_subset` +
  `cached_hidden_subset`; dropped now-unused `for_each_combination` + `UNITS` imports;
  updated the LSTAT [5] comment.
- `logic.rs` / `fused.rs`: passed `None` at the subset call sites.

## Verification methodology (reuse)

- **Yield identity (the gate) — now the harvest fixtures, BOTH paths.** `cargo test
  --release -p generator-lab --test harvest_reconstructs` replays the
  `tests/fixtures/harvest/*.txt` seed->puzzle fixtures and asserts the generator still maps
  each seed to its exact puzzle, through **both** the scalar `attempt`
  (`harvest_fixtures_reconstruct_scalar`) AND the W=8 SIMT warp `GateStream`
  (`harvest_fixtures_reconstruct_simt`). This is the fast replacement for the old full-range
  `findpar` diff: it only drives the known yielders plus each window's small negative control
  (the `xy-wing` exhaustive window is 2000 seeds), not millions of empty seeds — ~0.3s for
  both paths. Deterministic. The fixtures cover singles/doubles/triples incl. hidden-pair /
  hidden-triple (so the SIMT subset path — incl. box-unit positions — is exercised), naked
  subsets, fishes, and an exhaustive xy-wing window. A window fixture pins BOTH directions
  (yields <=> recorded); sample fixtures pin the forward direction only. **This is the Step 2
  gate** — a `subset_pos` transpose bug shifts an elimination, which shifts a yield, which
  fails this test loudly with the seed. Regenerate/extend via `examples/harvest.rs` +
  `scripts/rarity` (do NOT re-run by default — the committed fixtures ARE the baseline).
- **Tests (full).** `cargo test --release -p generator-lab` (release: real generation): lib
  (22), `confluence` (2), `equiv_warp_repr` (2 — the SIMT==scalar gold standard, but only
  `train(NAKED_PAIR)`), `faithful` (12), `harvest_reconstructs` (2 — the both-paths yield
  gate above; covers the hidden/fish/wing SIMT paths `equiv_warp_repr` does not),
  `logic_equiv` (6), `prober_equiv` (3). Whole suite ~5.5s.
- **`findpar` diff (heavy fallback, optional).** The original gate, still valid for specs the
  fixtures don't cover or for a broader sweep. `findpar` over a FIXED seed range (one seed =
  one attempt; `--count` huge so it never early-stops, `--max` = the seed-range size), output
  `sort`ed and diffed pre/post. Specs + budgets once used: `hidden-quad --max 2000000` (44,
  subset-heavy/fishless), `jellyfish --max 1500000` (132, fish), `w-wing + jellyfish --max
  1000000` (134, fish+wing), `swordfish + naked-triple --max 1000000` (106, mixed), `xy-wing
  --max 200000` (35266, the heavy sensitivity workhorse); all `--toolbox train --seed 1`.
- **Perf.** `findpar-bench` (fixed budget, yield-independent, folds an order-independent
  puzzle-set fingerprint). Build the OLD binary from the main repo at
  `/home/roxxik/repos/sudoku` (on `master`); run it interleaved with the worktree NEW
  binary, 3 rounds/spec, `--per-lane 100000`, comparing `us/att` and confirming `fp` match.
- **Process.** Run long commands in the FOREGROUND with a big `timeout`; do not let them
  auto-background. If something is backgrounded, the harness sends a completion
  notification — just read its output file; never busy-poll with `sleep` loops.

## Artifacts (durable)

- **Found puzzles** (pre==post identical) + run stats:
  `/home/roxxik/sudoku-ladder-puzzles/*.base`, `*.stats`. Format per line:
  `seed N: <81-char puzzle> (G givens)` then `solution: <81-char>`. The user has an idea
  for these (pending — to be shared once the work is finished).
- `findpar` raw baselines: `/tmp/ladder-verify.*` (path stored in
  `/tmp/ladder-verify-path-ladderredo.txt`). `/tmp` is volatile — re-capture if missing.
- Worktree: `.claude/worktrees/ladder-redo`, branch `ladder-redo` (currently == `master`).

## Open / pending

- User has an idea for the found puzzles, to share after the work is finished.
- Shared-vs-separate (2a vs 2b) decision: **RESOLVED — 2b SHARED adopted** (see the A/B verdict).
- Step 3a (hidden subset marks-free): **DONE — cleaner + ~0.7% faster on hidden-heavy specs**
  (see Step 3a Outcome). It removes hidden's `cm`-marks dependency and unblocks 3b.
- Step 3b (lazy-`cm` reorder) remains optional/unstarted, now unblocked by 3a — A/B a
  hidden/fish-pure toolbox to size the win (naked-forced specs need `cm` regardless).
- Two noted micro-opts, both separate + measured: narrow the `SubsetCache` build gate from
  `ANY_SUBSET` to `ANY_NAKED` (marks now naked-only; dormant in production); and make the
  `ANY_HIDDEN` position rebuild lazy past naked-pair.
