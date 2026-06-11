# The SIMT generation architecture

This document describes the target design of `generator-lab`'s vectorized
puzzle-generation machinery as if it were built from scratch with everything
measured so far. It is a vision of a future state, not a migration path.
Vocabulary is chosen fresh; nothing inherits a name for legacy reasons, and
every term is challenged against one criterion: does the name describe the
role of the thing it names. A correspondence table to the current code sits
in the appendix, separate from the vision itself.

## 0. Problem statement

A generated puzzle costs thousands of board questions. Two kinds dominate:
existence probes ("does the stripped board still have exactly one
completion?") and toolbox solves ("does this technique set crack the board,
and what does it use?"). Both questions share a shape: a propagation core
that is lane-uniform band ALU — vectorizable W wide — interrupted by scalar
decisions (branch picks, technique ladders) that are divergent per board.

The architecture's job:

- keep every vector unit full of useful lanes,
- keep the scalar interludes cheap, rare, and composable,
- make every experiment a small wiring change instead of a rewrite,
- resolve all composition at compile time, so the most intricate
  experimental pipeline still compiles to the flat, fully-inlined loops a
  hand-fused implementation would have.

## 1. The model

Boards flow; logic stands still. A **warp** holds W boards in SoA lockstep;
a **kernel** advances all of them at once, one pass at a time, and reports
each lane's **halt** truthfully (stalled, solved, or dead). A **station**
owns one warp and one kernel, loops the passes, and decides *when* they run
and *what* occupies the lanes. **Engines** own the in-flight questions: at
every halt they either steer the board onward or conclude a verdict. A
**rig** wires stations and engines into one concrete way of generating
puzzles and exposes a **puzzle stream** to the consumer.

One lane is *not* one attempt. Lanes hold queries; attempts — the seeded,
goal-pursuing walks that pose the queries — live above the stations, as
suspended continuations, and there may be many more of them in flight than
there are lanes anywhere.

## 2. Vocabulary

Each name, the thing it names, and why the name holds up.

| Term | The thing | Why this name |
|---|---|---|
| `simt` | The module: one instruction stream over many board lanes, scalar divergence serviced per lane. | The execution model's proper name. |
| board family | A board representation plus everything that speaks it: geometry, kernels, engines, transforms. `banded` (3x27-bit row bands) is the founding family; a cell-major family is anticipated. | Families group what must agree on layout; "family" says members are kin and non-members need a transform to visit. |
| warp | W boards resident in SoA form, advanced in lockstep or not at all. | GPU loan-word with exactly this meaning. |
| lane | One board's slot in a warp. | SIMD's own word for the element position. |
| `LaneBoard` | The scalar, one-lane form of a family's board (candidates + unsolved mask). The currency of loads, snapshots, moves, and scalar services. | It is literally the board of one lane. One definition per family; query types embed it rather than re-spelling its fields. |
| kernel | The lane-uniform pass function over a warp's active lanes. Named by the technique set it saturates: `singles`, `singles_rowbox`, `singles_lc`. Its pass shape — full sweep, internal fixpoint, first-fire — is kernel-private; the station sees only truthful halts. | "Kernel" = the uniform code all lanes execute. Naming by saturation set makes the contract part of the name; intent-names like "lean"/"full" are banned. |
| pass | One application of a kernel. | One sweep, many per query. |
| saturated | A board at fixpoint under a technique set S: nothing in S fires. | Standard fixpoint vocabulary; the precondition/guarantee language of every station contract. |
| halt | A lane a pass cannot advance: `Stalled` (saturated, unsolved), `Solved`, or `Dead` (contradiction). Only halted lanes receive scalar attention. | The lane stopped; the three variants say how. |
| station | An occupancy machine: one warp + one kernel + the bookkeeping of which ticket sits in which lane. Two disciplines: **resident** and **batch**. | Boards arrive, are worked on, and leave; cycles are allowed, so "stage" (linear) would lie. "Unit" is sudoku vocabulary and is off-limits. |
| ticket | The claim check a query carries through stations: a small id that routes halts and verdicts back to the owning engine's per-query state. | You submit work, you get a ticket, results are claimed by ticket. In degenerate wirings the lane index doubles as the ticket. |
| engine | The owner of one kind of question: per-ticket state (stacks, memos, counts) plus the service logic that reacts to halts. Two engines: **prober** and **solver**. | It drives queries forward between passes; the machine metaphor matches "it has internal state and does work when called". |
| service | The scalar visit an engine pays one halted board: steer (edit and continue) or conclude (emit the verdict). | Maintenance performed while the machine is stopped. |
| query / verdict | The question one board poses, and its final answer. `Probe { board, cell, alts } -> bool` (an alternate completion exists). `Solve { board, scope } -> Trace` (solved + per-kind fire counts under the scope). | Plain words for ask and answer. Gates branch on verdicts; a `Trace`'s counts are tallies and pre-filter fodder, never semantics (Law 4). |
| scope | The kindmask bounding what a solve may use — the upper half of every solver contract. Travels per query, so one solver engine answers baseline gates, avoid-walks, and anything else a kindmask can express. | The extent within which the solver operates; nothing outside the scope may touch the board. |
| edit log | The output form of every scalar service: a short list of board edits (candidate eliminations, digit restrictions) computed on a scalar form. | Services *describe* changes; appliers write them. One log, two appliers: targeted bit-writes into a lane, or direct writes into a free `LaneBoard`. |
| gate | An attempt-level checkpoint: a question whose answer decides keep vs revert of a strip step. | The strip walk passes through it or doesn't. Gates belong to attempts; engines see only the queries a gate decomposes into. |
| lock | The per-forced-technique bit an attempt sets the first time the f-avoiding solve goes `Stalled`. Sound to carry forward only under a per-f monotonicity certificate from the compiled spec; once set, never re-checked. | Once set it cannot unset — locked. |
| attempt | One seeded try to produce one puzzle: fill, strip walk, gates, finalize. A coroutine, suspended at gates, resumed with verdicts. | The domain word. Its straight-down body is the readable spec of the generator's semantics. |
| rig | One assembled way of generating: instantiates stations and engines, owns the attempt pool, encodes all routing and firing policy. The unit of experimentation. | A rig is purpose-built apparatus assembled from standard parts — exactly what an experiment is. "Formulation" names the idea; the rig is the artifact. |
| compiled spec | The validated artifact rigs execute: the baseline scope (Rule 1), the per-f avoid scopes (Rule 2), all dependency-closed; confluence and monotonicity certificates; fast-path applicability flags. | Specs compile, rigs execute. Rejection of ill-formed specs happens at compile time, not as rig nondeterminism. |
| outbox / tallies | The rig-owned channel of finished puzzles awaiting the consumer, and the attempt accounting (attempts, successes, failure modes, givens). | Mailbox metaphor for the one, and "tallies" because they are counts, not statistics. |
| ledger | The rim's per-seed account: every seed drawn from the feed is eventually recorded as concluded (yielded, or failed with its mode) or in-flight. | A ledger records dispositions, not totals — that is what separates it from tallies. |
| puzzle stream | The consumer rim: `pump(ticks) -> Pumped { Found(seed, p) | Pending | NoMorePuzzles }` plus `tallies()` and the ledger. A trait every rig implements. | The consumer sees a stream of puzzles and nothing else. `Pending` = tick budget used up; `NoMorePuzzles` = seed feed drained and every lane idle. |

## 3. Layers

Five layers, strict downward knowledge:

```
rim        PuzzleStream, Pumped, Tallies, Ledger, Ticket   consumers: find, findpar, combobench
rigs       direct.rs, <experiment>.rs                      wiring, attempts, policy
engines    prober, solver                                  per-ticket state + services
stations   Resident<W,K>, Batch<W,K>                       occupancy + firing
substrate  per family: warp, LaneBoard, kernels            layout + lane-uniform math
```

- The substrate knows nothing above it.
- Stations know a family only through a minimal warp interface (load,
  snapshot, lane count, pass); they never inspect boards.
- Engines are family-resident: they use the family's geometry and scalar
  forms directly. They know nothing of stations or rigs; they are handed a
  halted board (in a lane or free) and a ticket.
- Rigs know everything below and compose it. All policy lives here.
- The rim knows only rigs-as-streams.
- The compiled spec (section 7) enters from above: rigs and engines consume
  its masks and certificates and may assert them, but nothing below the rim
  ever derives spec semantics.

Module tree:

```
src/simt/
  mod.rs            this map; PuzzleStream, Pumped, Tallies, Ledger, Ticket
  station.rs        Resident<W,K>, Batch<W,K>
  banded/
    mod.rs          family doc: geometry, the saturation ladder
    warp.rs         Warp (SoA state), LaneBoard, load/snapshot, edit-log appliers
    kernels.rs      singles, singles_rowbox, singles_lc, ...
    prober.rs       prober engine
    solver.rs       solver engine: ladder steps, caches, memos
  rigs/
    direct.rs       the production rig (+ its faithful corpus harvester)
    <experiment>.rs siblings, one file each
```

A second board family adds a sibling directory under `simt/`; stations and
the rim are untouched by construction.

## 4. The substrate

A family provides:

- **`Warp`** — the SoA resident state, sized by the family's natural lane
  count (the `banded` family: nine per-digit band triples plus the unsolved
  mask, eight u32 lanes wide).
- **`LaneBoard`** — the scalar one-lane form. Everything that moves a board
  in or out of a warp speaks `LaneBoard`: refills, snapshots, inter-station
  moves, scalar services. Query types embed it (`Probe` adds the stripped
  cell and its alternates; `Solve` adds the scope mask) instead of
  re-declaring band arrays.
- **Kernels** — `fn(&mut Warp, active) -> (changed, dead, solved)` lane
  masks, one per saturation set. The set is the name and the contract:
  after a pass returns a lane unchanged-undead-unsolved, that lane is
  saturated under the kernel's set. At the station boundary, masks are
  plain `u64` bitmasks; SIMD mask types stay inside the family.
- **Lane edits** — the primitive writes services need: restrict a cell to a
  digit set, clear single candidate bits. These are the appliers of edit
  logs, in two forms (into a lane, into a free `LaneBoard`), and they are
  branchless where measurement demanded it.
- **Transforms** — re-representations a service prefers (a cell-major marks
  view for per-unit scans) and, eventually, cross-family conversions. A
  transform is read-only; changes return as edit logs. An inter-station
  move is `snapshot -> [transform] -> load` — the same mechanism whether
  the destination is a scalar ladder or another warp.

**Kernel shapes.** The contract above fixes only observables — truthful
halts — so a kernel's per-pass granularity is its own business, and three
shapes are legitimate:

- *One sweep* (the closure kernels): a stall is detected as "a full sweep
  changed nothing"; saturation emerges across passes. The station tick is
  the scheduling quantum, and at the bottom of the ladder this is settled
  by measurement: lockstep bills every lane for the deepest lane's drain,
  so internal fixpoint loops regress monotonically with their depth.
- *First-fire* (the natural shape at altitude): scan until one technique
  fires, emit the edit, stop. A high-rung kernel cannot meaningfully run
  to its own fixpoint in isolation — its first fire un-saturates the board
  for cheaper sets, and progress wants to descend the ladder. Its clean
  full scan ("nothing fires") is itself the valuable verdict, and the
  solver's read-set memo is what makes re-certifying it cheap.
- *Staged to scope* (the one coherent until-stall at altitude): closure
  plus ladder rungs fused into one pass with per-lane rung masking,
  saturating an entire scope inside one station — a gallery experiment.

Granularity and firing order are a free performance knob exactly when the
scope is confluent (the certificate the compiled spec provides); the prober
is unconditionally indifferent. A kernel choice may therefore never change
a verdict — only its cost.

The family also states its **saturation ladder** — the partial order of
kernel sets (`singles_rowbox` < `singles` < `singles_lc` < ...) — because
station contracts are phrased in it. The ladder is a tested claim, not
documentation: property tests pin each kernel's name (a stalled lane
admits no fire under the reference scalar implementation of its set) and,
for certified-confluent sets, fixpoint equality with the scalar reference.
Every contract check in the system bottoms out in these two properties.

## 5. Stations

A station is an occupancy machine over one warp and one kernel. It owns no
question logic; it knows which ticket occupies which lane and when to run a
pass. One warp has one kernel; a board needing different propagation moves
to a different station.

**Resident** — lanes hold occupants across many passes and services. A
`tick` is: one pass over active lanes, then service every halted lane via a
caller-supplied handler, in lane order, against the halt set computed by
that pass (services during a tick do not see each other's effects; a lane
whose occupant changes mid-tick is not re-serviced until the next pass).
The handler steers in place or vacates the lane; vacated lanes are refilled
by the rig. Resident discipline fits occupants serviced on nearly every
pass — branching searches — and is the production choice: a full resident
warp runs at effectively complete utilization.

**Batch** — a buffer of `(LaneBoard, Ticket)` tokens in front of the warp.
The station accepts tokens until told to fire, runs every lane to its first
halt, emits all `(halt, board, ticket)` outcomes, and is empty again. It
services nothing — emit-only by design; what happens to an outcome is the
rig's business. Batch discipline fits work that arrives irregularly but is
uniform once batched: a specialized kernel gets eight lanes that all
satisfy its precondition.

Both are generic over the warp interface and the kernel, monomorphized per
instantiation, and both take their handlers as closure parameters that must
inline into the pass/service loop (a verdict that travels through memory on
every serviced lane is a measured ~2% regression; the type system is the
delivery mechanism for "fully unrolled").

**Contracts — two-sided.** Every station instantiation declares: the family
it speaks, the saturation it *requires* of entering boards, and the
saturation its stalled emissions *deliver* (the kernel's set). A wiring is
valid when every edge satisfies both bounds:

- *lower*: the edge's delivered saturation implies the destination's
  required saturation — entering boards are propagated enough;
- *upper*: for edges admitting solve queries, the station's kernel set is
  contained in the scope of every query class the edge admits — boards are
  never propagated beyond what their question permits.

Both are const-mask assertions in the station constructor, so the compiler
— not a reviewer reading the rig — rejects an illegal wiring; a debug
assertion at solver intake (`kernel set ⊆ query.scope`) catches the dynamic
per-query case, since scopes travel and wirings are static. The lower bound
is what makes specialized stations sound (a subset-ladder warp may assume
all eight lanes are singles+LC-saturated because its intake edge says so);
the upper bound is what keeps them honest (see the engines' asymmetry,
section 6).

**Firing.** Fullness is a trigger, never the only trigger. The measured
law: waiting for full warps deadlocks the tail and starves the pipeline;
opportunistic firing wins. A batch station exposes pressure (buffer depth);
the rig fires the fullest station when no resident work can proceed, and
drains everything when the seed feed is dry. Utilization of a batch
station is bought with feed depth (measured: ~48% at depth 16 rising to
~86% at 64) — the cure is more attempts in flight, not smaller buffers.

## 6. Engines

An engine owns one kind of question end to end: the per-ticket search state
and the service logic. Engines are deliberately ignorant of occupancy — a
halted board is a halted board, whether it sits in a resident lane, arrived
as a batch outcome, or never touched a warp at all.

**Prober** — answers `Probe { board, cell, alts } -> bool`: does a
completion exist with `cell` restricted to `alts`? Per ticket it keeps a
branch stack of frames (`LaneBoard` + untried digits). Service on `Stalled`
picks a branch cell (bivalue-first) and descends; on `Dead` backtracks or,
with the stack exhausted, concludes `false` (unique); on `Solved` concludes
`true`. The descent and backtrack are edit logs (restrict cell to digit)
applied wherever the board lives. The prober declares **no upper bound on
its kernel**: correctness comes from search, and any sound elimination
preserves the set of completions, so over-propagation can only accelerate
`Solved`/`Dead` — it can never flip existence. Any kernel is a sound
partner; pick the cheapest.

**Solver** — answers `Solve { board, scope } -> Trace`: run the technique
toolbox masked by `scope` to solved/stuck, counting fires per kind. Here
the scope is an **upper bound on every elimination performed on the
query's board — kernel and ladder alike**. A kernel set exceeding the
scope does not produce wrong-faster, it produces wrong: `Solved` verdicts
the spec's player cannot reach (Rule 1 violations emitted as puzzles), and
on avoid scopes, wrongful `Solved` that silently rejects valid puzzles.
The wiring's upper-bound check and the intake assertion (section 5) exist
for this engine. The solver additionally debug-asserts that every scope it
sees is dependency-closed — it executes compiled scopes (section 7), it
never repairs them.

Per ticket the solver keeps the fire counts and the cross-stall memo. The
memo is **read-set-shaped**: each ladder step declares the partition of
board state it reads — subsets: the unit; fish: the digit plane; wings:
the bivalue/pair buckets — and a no-fire verdict is keyed by partition
cell, invalidated exactly when its cell has changed since the last scan.
The memo stays mutation-source-agnostic (it diffs state, not events);
unit granularity is merely the subset ladder's instance of the mechanism.
Service on `Stalled` runs one **ladder step**: the first scope-permitted
technique beyond the kernel's saturation set that fires, as an edit log.
The ladder is built from named, individually pluggable steps —
column-singles recovery, locked candidates, the subset ladder over a
per-unit cache, fish, wings — and the rig composes exactly the steps its
kernel choice leaves uncovered.

Engines never call stations and never block. They are libraries of "given
this halt, here is the edit or the verdict" — which is what lets a rig run
the same engine against a resident lane today and a batch pipeline
tomorrow.

## 7. The compiled spec

What a generated puzzle must satisfy — the three rules every rig serves:

1. **Solvable** by `allowed + forced` (the baseline scope).
2. **Forcing**: for each forced technique `f` with requirement `need(f)`,
   the puzzle is not solvable by `allowed + conceded + forced \ {f}` using
   fewer than `need(f)` applications of `f` (the avoid scope for `f`;
   `need = 1` is the set-subtraction form, and `need > 1` is first-class).
3. **Minimal relative to the toolbox**: every clue derivable from the
   remaining clues by `allowed + forced` techniques is removed.

Specs compile, rigs execute. Compilation happens above the rim, once, and
produces the validated artifact everything below trusts:

- the **baseline scope** and the per-f **avoid scopes**, all
  dependency-closed (a technique's prerequisites are in every scope that
  contains it) and role-overlap-free;
- rejection of incoherent specs: a forced technique that is another's
  dependency, masks whose closure is contradictory — compile errors, not
  generator behavior;
- a **confluence certificate** per scope the spec will ever pose: the
  scope's fixpoint (and hence its `Solved`/`Stalled` verdict) is
  order-independent. Non-confluent scopes make ladder order, kernel choice,
  and pass granularity semantically visible — such specs are ill-formed
  and rejected here, never discovered as rig nondeterminism;
- a **monotonicity certificate** per avoid scope: clue removal never turns
  `Stalled` into `Solved`. This is what makes a lock (section 8) sound to
  carry forward. Avoid scopes are the likeliest to fail it — they exclude
  `f` by construction, and an `f` from the singles trunk leaves a
  trunk-broken scope — so certification is per `f`, and an uncertified `f`
  simply falls back to the cold reference check;
- **fast-path applicability flags**: each attempt-level fast path is a
  verdict-equivalence theorem with a precondition on the spec; the
  compiled spec evaluates the precondition once, the rig branches on the
  flag.

The engines assert against this artifact (dependency closure, scope
containment); they never derive it.

## 8. Rigs

A rig is the whole answer to "how do we generate": which stations exist,
which engines own which queries, how attempts pose gates, in what order
things fire. Rigs are hand-written, straight-line, monomorphic code — and
small, because everything heavy lives below. There is deliberately no
generic dataflow executor: the scheduling policy *is* the experiment, the
station count per rig is small, and a graph held in types is the only
graph that fully unrolls.

**Attempts** are coroutines: fill a solution, walk the shuffled cells,
strip, gate, finalize; suspended at each gate, resumed with its verdict.
Attempt-level fast paths (trivially-kept strips, re-force detection,
precomputed non-uniqueness patterns) live in the strip-walk state shared
with the scalar generator, upstream of any query — the cheapest gate is
the one never posed. Each fast path asserts a verdict-equivalence theorem
(it carries the gate's verdict; it never infers one from counts), and its
applicability precondition arrives as a compiled-spec flag. The rig holds
attempts in a slab; the slab's depth is the oversubscription knob that
keeps batch stations fed (a suspended attempt costs on the order of a
kilobyte, so depth is a measured cache-pressure tradeoff, not a free
maximum).

**The direct rig** — production. One resident station on the `banded`
family's `singles` kernel; both engines; one attempt per lane (the
degenerate slab where ticket = lane). An attempt's gate is fused: the
probe query loads with its restriction; a non-unique verdict resumes the
attempt immediately; a unique verdict hands the lane to the solver *in
place* — a same-lane move, no buffer, no copy beyond the cached raw query
board — and the solve's trace answers the gate's second half in the same
occupancy. Solver stalls run the ladder step composed for the `singles`
kernel (locked candidates first, then subsets, fish, wings, per the
scope). This rig is pinned byte-for-byte to the sequential scalar
generator: same seeds, same puzzles, lane for lane.

**The direct rig's expected evolution — folded forcing.** Rule 2 today is
a cold end-of-walk check, and its avoid walk re-solves repeatedly. But
under a monotonicity certificate, `Stalled` under an avoid scope is a
**lock**: clue removal only weakens the board, so once the f-avoiding
solve sticks at a partial strip, it sticks for every descendant, and the
question never needs asking again. Folding the avoid solves into the walk
as ordinary solver queries (per-query scopes make this zero new machinery)
turns Rule 2 from a per-success cold verify into one stuck-verdict per
forced technique, batchable on the warp like any other solve; an attempt
that finishes its walk with a lock unset has failed Rule 2 and is
accounted, not verified. The reference checker remains the oracle in tests
and the fallback for any `f` whose avoid scope lacks the certificate. For
the rare-spec regime this is the asymptotic lever, which is why it is the
direct rig's planned evolution and not a gallery curiosity.

**The gallery** — each a sibling file, each reusing the toolset, each
listed here with only its wiring delta:

- *deferred* — strip several cells before gating; bisect on a non-unique
  verdict. New attempt body and revert bookkeeping; same engines, same
  station.
- *pipelined* — probe lanes resident on `singles`; unique verdicts queue
  into a batch station on `singles_lc` for the solve side; solver stalls
  return via the ladder. Buffer depth and firing pressure are the
  experiment. Edge legality is the two-sided check at work: this wiring
  only compiles for specs whose solve scopes contain LC.
- *staged scope kernel* — one kernel whose pass stages closure plus ladder
  rungs with per-lane rung masking, saturating the full scope inside one
  station: until-stall at altitude done coherently. Tests bouncing between
  specialized stations against staging within one warp.
- *ladder warp* — a batch station whose kernel is a vectorized subset
  scan, intake edge requiring singles+LC saturation; the solver's scalar
  subset step becomes that station's feed and drain.
- *solver-only* — no probes; the solve verdict gates directly. Cheap to
  build, historically 4-5x slower — the rig exists to keep that fact
  re-measurable for new specs.
- *kindred* — several lanes carry variants of one fill to share
  propagation. Breaks attempt independence by design: needs its own
  ordering definition and is verify-gated, not equivalence-pinned.
- *common snapshot* — one shared propagation per gate, then fork: restrict
  the saturated board for the probe, keep it as the solve query. A
  three-phase occupancy in the direct rig's wiring.

**Corpus harvesters** — a rig that claims fidelity to a scalar walk also
exports its harvester: the same gate sequence run scalar, pairing each
query with its reference verdict. It lives beside the rig because the two
must not drift.

## 9. The rim

```rust
trait PuzzleStream {
    fn pump(&mut self, ticks: usize) -> Pumped;  // Found(seed, p) | Pending | NoMorePuzzles
    fn tallies(&self) -> Tallies;
    fn ledger(&self) -> &Ledger;                 // per-seed: concluded(outcome) | in-flight
}
```

`pump` semantics, pinned: drain the outbox first (one puzzle per call,
no ticks spent); otherwise tick until a puzzle lands (`Found`, remaining
budget unspent), the budget runs out (`Pending`), or the feed is dry with
every lane idle (`NoMorePuzzles`).

Aggregate tallies are not enough for the resumption story, so the rim
keeps a **ledger**: every seed drawn from the feed is eventually recorded
as concluded — yielded a puzzle, or failed with its mode — or is
in-flight (drawn but suspended at `Pending`). Law 2 makes replay lossless;
the ledger is what tells the consumer *what* to replay: resumption is a
fresh rig over the feed minus the concluded seeds. Failure modes are
per-seed dispositions here, not just counters.

Consumers — `find`, `findpar`, `combobench` — hold `impl PuzzleStream` and
know nothing else. Parallelism is sharding: one rig per thread over
disjoint seed ranges; stations and warps are never shared across threads.
Benches cap work externally on `tallies().attempts`; rigs carry no budget
of their own.

## 10. Laws

The rules the design is built to obey. Each earned by measurement or by a
correctness argument; none is a style preference.

1. **The graph lives in the types.** Stations and engines compose through
   generics and inlined closures; no trait objects, no runtime-configurable
   topology, no interpreter. The most complex rig must disassemble like a
   hand-fused loop.
2. **An attempt's outcome is a pure function of its seed.** Scheduling —
   discipline, firing order, oversubscription — may permute completion
   order only. Any rig that breaks this (kindred) must say so and define
   its own determinism story.
3. **Equivalence is per rig.** A rig claiming scalar identity is pinned by
   the equivalence anchors (seed -> puzzle, lane for lane) against its own
   scalar twin — same ladder order. Every other rig is gated by the
   reference checker — which accepts no fast-path debt — plus honest
   accounting: cost is total work over puzzles found, failures included.
4. **Gates read verdicts; traces are tallies.** Attempt semantics branch
   on verdicts (`Solved`/`Stalled` under a scope) and on count-filters
   only where a later verdict catches their false positives; emitted
   puzzles are justified by verdicts alone, and traces never cross the
   rim. Corollary: count-filters are path artifacts, so reordering a
   ladder may change *which* puzzle a seed yields — never its validity —
   and equivalence claims are scoped accordingly (Law 3).
5. **Contracts are two-sided.** Saturation bounds propagation from below
   (delivered implies required); scope bounds it from above (kernel set
   contained in every admitted query's scope). The prober is exempt from
   the upper bound by argument; the solver is held to it statically per
   edge and dynamically per query.
6. **Rigs execute compiled specs only.** Every scope a rig poses comes
   from spec compilation — dependency-closed, confluence-certified, with
   monotonicity certificates where locks rely on them. Engines assert;
   they never derive or repair.
7. **Services emit edit logs.** Scalar work computes on scalar forms and
   describes its changes; appliers write them as targeted bit edits.
   Boards are never rebuilt wholesale, and the same service body works
   in-lane and on free boards.
8. **Fullness is a trigger, never the only trigger.** Fire on pressure,
   drain on quiescence. A pipeline that waits for full warps starves its
   own tail.
9. **Utilization is bought with depth.** Batch stations are fed by
   oversubscribed attempts, not by shrinking buffers; slab depth is a
   measured knob trading lane utilization against cache footprint.
10. **Instrumentation costs zero when off.** Knobs are type parameters
    (ZST policies), counters are feature-gated to nothing, and bench-only
    affordances never burden the production rig.
11. **Hot returns stay in registers.** Handlers inline into station loops;
    a fat enum returned through memory once per serviced lane is a
    regression, and the closure-parameter design exists to prevent it.
12. **One rig per thread.** Cross-thread coordination buys nothing the
    seed shard does not already give, and it would put a lock where the
    hottest loop is.

## Appendix: correspondence to the current code

For later chunking only; the vision above does not depend on this table.

| Vision | Today |
|---|---|
| `banded::Warp` + `LaneBoard` | `WarpBoards`; `Probe.r/unsolved`, `SolveQuery`, `Frame.r/unsolved`, snapshot tuples (four spellings of one type) |
| `banded::kernels::singles` | `solve::simt::warp_pass_full` |
| `banded::kernels::singles_rowbox` + column-singles ladder step | the deleted lean `warp_pass` + `scalar_col_assign` (kept for this) |
| prober engine | `probe::simt` branch machinery + `solve::simt::prober_service` / `resolve_probes` |
| solver engine | `solve::simt::subset_step`, `SubsetCache`, `CellMarks`, `LadderMemo`, `scalar_lc_fast` |
| read-set memo | `LadderMemo` (unit-granular: the subset-ladder instance only) |
| resident station + direct rig | `generate::warp_host`: `PuzzleStream`/`WarpJob`/`GateJob`/`lane_co`/`GateStream` (host, occupancy, engines' per-query state, and wiring interleaved — the entanglement the layering removes) |
| batch station | `run_warp_pipelined`'s buffer mechanics (archived branch) |
| compiled spec | `Spec` + `baseline_fast_applicable` + informal spec checks (non-confluent specs exist today but are never used) |
| folded forcing locks | `verify`'s `min_target_uses` avoid walk (cold, per accepted attempt) |
| `PuzzleStream` trait / `Pumped::{Found,Pending,NoMorePuzzles}` | `GateStream::pump` / `Pumped::{Found,StepCountReached,NoMorePuzzles}` |
| ticket | implicit (lane index) |
| corpus harvester | `collect_probes` |
| `Tallies` / `Ledger` | `Stats` (aggregate only; the ledger does not exist yet) |

## Note (2026-06-11): partial landing

A first slice of this vocab is in `warp_host.rs`: `WarpJob`->`Engine`,
`GateJob`->`GateEngine`, `lane_co`/`LaneCo`->`attempt`/`Attempt`, and the `Slot`
band-aid -> `Ticket<E, A>` behind an `Occupant` trait the host is generic over.
Only the engine+attempt unit moved; stations, rigs, and the rim are still fused.
