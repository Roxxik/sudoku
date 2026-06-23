# Generation Rules

The abstract model the generator and verifier are built around. The rules a
generated puzzle must satisfy, stated entirely in terms of **solve paths**,
so the generator can be structured around them.

This supersedes the two-check "Verifier contract" in `CURRICULUM.md` and
replaces the premise the (abandoned) folded-forcing work rested on. Where the old
contract spoke of "the in-scope toolbox minus the target" and a single avoid
walk, this generalizes both the toolbox and the requirement, and adds
capabilities the old model could not express: difficulty **ceilings**,
constraints on what the board ever **offers** (forbidding a technique, bounding
or guaranteeing its availability), and per-move **placement payoff** filters.

## 1. The universe: solve paths

Fix a solution grid `G`. A **puzzle** `P` is a set of revealed cells of `G` with
a unique completion.

Starting from `P`'s initial candidate grid, a **solve path** `π` is a finite
sequence of technique applications `a₁ … a_k`, each `aᵢ` a legal firing of some
technique on the *current* state (removing candidates and/or placing digits),
ending at the complete grid `G`.

A solve path is **not** a priority order over techniques and **not** a
deterministic policy. It is one concrete trajectory through the state space; any
legal interleaving is a different path. The object every rule quantifies over is

> `Π(P)` — the set of *all* solve paths of `P`.

This is the one premise kept from the abandoned work: quantify over paths, never
over a fixed ladder. The canonical easiest-first solver has no privileged role;
it is at most a presentation device, never a definition.

Every rule reduces to one of two per-path quantities:

- `uses(π, t)` — how many times the solver **applies** technique `t` along `π`.
- `live(π, t)` — how many distinct **productive opportunities** of `t` the path
  passes through: states where firing `t` *would make progress* (a real
  elimination or placement). Progress-based — a pattern that is present but
  eliminates nothing is not live. Counting along a single path is by *opportunity*
  (a pattern that persists across several states is one opportunity, not many);
  the exact dedup is the one open micro-detail (see §12).

The difference between the two is the whole of §3.

## 2. The bounded toolbox

A **bounded toolbox** `B` assigns every technique a usage cap, per *context*. A
cap bounds `uses` — what the solver is allowed to *do*:

- `0` — **ignored** (UI: "Off"): the solver does not use `t`. This is *not* a
  forbid — `t` may still be live (offering shortcuts); the solver just never
  takes them. "Not in the toolbox" and "cap 0" are the same statement.
- `k` — **capped**: at most `k` applications *on a single path*.
- `∞` — **allowed**: unlimited.

A path is **B-admissible** iff every technique's `uses` stays within its cap.
Write `Π_B(P)` for the B-admissible paths.

Caps are **per `(technique, context)`**, context ∈ {solve, avoid}. The two
contexts let a technique be available to a positive question but not a negative
one. The familiar roles are points in this space:

| role      | solve cap | avoid cap | meaning                                        |
|-----------|-----------|-----------|------------------------------------------------|
| allowed   | `∞`       | `∞`       | usable everywhere                              |
| ignored   | `0`       | `0`       | solver never applies it (may still be live)    |
| capped    | `k`       | `k`       | at most `k` applications per path              |
| conceded  | `0`       | `∞`       | denied to the solver, granted to the adversary |

**Ignored and conceded are different statements** and both are kept: ignoring
withholds a technique from the solver in both contexts; conceding withholds it
from the solver but hands it to the avoid search, so a forcing claim must hold
*even against* the conceded substitute. Conceding a peer makes a forcing claim
*harder* (the target must be unavoidable even with the substitute available — the
strong "genuinely isolates this technique" claim). Drill is the latter.

Note that *none* of these forbids anything: they bound `uses`, never `live`.
Forbidding — constraining what the board *offers* — is a different axis (§6).

## 3. Two quantities, one law

`uses` and `live` differ in *who controls them*, and that single fact organizes
every rule.

- **`uses` is controllable.** The solver chooses which techniques to apply, so an
  *upper* bound on `uses` is **definitional**: it just restricts the solver's
  choices — that is what a cap (and `ignored` = cap 0) is, baked into
  admissibility, never verified. A *lower* bound on `uses` is a necessity claim
  and must be **verified** over the path set (that is forcing, Rule 2).
- **`live` is not controllable.** Whether `t` could make progress at a state is a
  property of the board, not a choice. So *every* bound on `live` — upper or
  lower — is a **verified** fact about the puzzle, never definitional. This is
  why forbidding is *strictly harder than a cap*: a cap restrains behaviour;
  forbidding constrains the uncontrollable board.

**The aggregation law.** A bound that must hold "no matter how you solve" is the
worst case over `Π(P)`, and the worst case differs by direction:

> **Upper bounds aggregate by `max` over all paths; lower bounds by `min`.**

- "at most `m`, however you solve" = `max_π count(π, t) ≤ m` — even the path that
  offers/uses it *most* stays under `m`.
- "at least `m`, however you solve" = `min_π count(π, t) ≥ m` — even the path that
  offers/uses it *least* stays over `m`.

Existential count bounds ("*some* path uses/offers it `≥ n`") are toothless under
no-ladder — no player is bound to that path. `max`/`min` are the only sound
aggregations. The four families:

| quantity            | upper (`max` over paths)            | lower (`min` over paths)          |
|---------------------|-------------------------------------|-----------------------------------|
| `uses` (controlled) | cap / ignored — **definitional**    | forcing (Rule 2) — verified       |
| `live` (offered)    | forbidden / soft-cap — verified     | guaranteed presence — verified    |

Three of the four are verified properties; only the `uses` upper bound is
definitional (it shapes `Π`). The verified `uses` lower bound (forcing) reduces
to bounded solvability (§4–5); the `live` bounds do **not** — they need a search
that tracks live-occurrence counts over paths, which is separate, heavier
machinery (§6).

## 4. The core primitive: bounded solvability

The `uses`-side rules all reduce to a single predicate:

> `solvable(P, B)` — does a B-admissible solve path exist? (`Π_B(P) ≠ ∅`)

asked with a **polarity** and a **proof discipline**:

- A **positive** assertion (`solvable` must hold) is cleared by a **witness** —
  one admissible path found. Cheap to prove.
- A **negative** assertion (`solvable` must not hold) is cleared by
  **exhaustion** — the admissible path space searched out with no solving path.
  Expensive. Finding one solving path is an immediate *disproof*.

There is **no three-valued verdict at the rule level.** A budget-limited search
that neither finds a witness nor exhausts has not produced a proof, so the
assertion is **not cleared** and the puzzle is rejected. Budget exhaustion only
ever pushes a verdict toward **reject**, never a false accept. Every emitted
puzzle is *definitely* conforming; the only cost of a tight budget is missed
yield, not unsound output.

The `uses`-side of the spec is thus a list of `(toolbox, polarity)` assertions
over this one function.

## 5. The uses-rules: polarized solvability assertions

### Rule 1 — Solvable (positive, base caps)

`solvable(P, B_solve)` must hold: at least one admissible solve path exists under
the spec's solve-context caps. All upper caps (`capped`, `ignored`) bite here —
they shrink `Π` to the paths that count as a real solve.

### Rule 2 — Required (negative, tightened caps)

A **requirement** is a set of threshold atoms `{ (t₁ ≥ n₁), …, (t_m ≥ n_m) }`,
read **disjunctively**: a path *meets* it iff `uses(π, tᵢ) ≥ nᵢ` for some `i`.
Rule 2 demands **every admissible path meets it** (`min` over paths, §3),
verified negatively: tighten `B_avoid` so every atom sits just below threshold
(`tᵢ` at `nᵢ − 1`) and require `¬ solvable(P, B_avoid tightened)`.

- single forced `f`: `{ f ≥ 1 }` → cap `f → 0`.
- **`force_any{A, B}`** (disjunctive unavoidability): `{ A ≥ 1, B ≥ 1 }` → cap
  `{A, B} → 0`. No path can avoid both; the avoid search removes the **whole
  set**. This *may collapse* (it holds even if only `A` is ever forced and `B`
  never comes up); for a forced choice that provably does not collapse and is
  never doubled up, see §7.
- **"two X-Wings or one XY-Wing"**: `{ XWing ≥ 2, XYWing ≥ 1 }` → cap
  `XWing → 1, XYWing → 0`.
- **count** `force(f, k)`: `{ f ≥ k }` → cap `f → k − 1`.

A spec carries a **conjunction** of requirements. The avoid search inherits the
base caps, so caps and requirements compose: `f ≤ k` (cap) with `f ≥ n` (Rule 2)
pins every admissible path's `f` into `[n, k]`.

### Rule 1c — Ceiling (positive, tightened caps)

The dual of Rule 2: assert the puzzle stays *easy enough*.

> `solvable(P, B_ceiling)` must hold, where `B_ceiling` is the solve toolbox
> tightened — typically by ignoring everything above some difficulty.

Polarity gives the two halves of "how hard is this puzzle":

- a **floor** ("at least this hard") is negative — `¬solvable` without the hard
  technique. That *is* Rule 2.
- a **ceiling** ("at most this hard") is positive — `solvable` using only
  techniques no harder than a chosen bound.

Combine: *exactly X-hard* = `¬solvable(without X) ∧ solvable(with nothing harder
than X's tier)`. A ceiling with one technique ignored also expresses an
*alternative-route / anti-trap guarantee* ("there is a way around `X`"). This
puts a slice of grading inside the `solvable` call, before any heavyweight grader.

## 6. The live-rules: constraints on what the board offers

These constrain `live` (§1) and so are verified facts about the puzzle (§3),
evaluated over the worst-case path. They do **not** reduce to `solvable`; they
need a search that tracks live-occurrence counts. Forbidden is the one to lean
on; the others are flagged.

### Forbidden — `max_π live(π, t) = 0`

No solve path ever offers productive `t` — equivalently, no reachable state
offers it. At zero the `max`/`min` aggregations coincide (every reachable state
lies on some path), so forbidden is **metric-free** — the robust member of the
axis. It is *not* "ignore": ignoring (cap 0) lets `t` sit there live as a
shortcut you don't take; forbidding guarantees `t` is never even available, no
matter where the player wanders. Neither caps, concede, nor force can express it
(each is about `uses`, not `live`).

Most useful on a **rare** technique ("no X-Wing pattern is ever live here") —
cheap-ish and a clean cleanliness guarantee. On a *common* technique (e.g. hidden
single) it is expensive and low-yield: placements keep regenerating the pattern,
so killing all occurrences across all orders forces a dense board.

### Soft cap — `max_π live(π, t) ≤ m`

"However you solve, you see `t` at most `m` times." Pointed at the **allowed
peers**, not the target, this is the complement of forcing: forcing adds
*necessity* to one technique; a soft cap removes *optionality* from the rest, so
the board rarely offers the easy alternatives. Forcing alone leaves the
bottleneck floating in easy filler; soft-capping the peers concentrates the solve
and takes away alternative routes. Forbidden is the `m = 0` end of this knob.

### Guaranteed presence — `min_π live(π, t) ≥ m` *(maybe)*

"However you solve, you encounter `t` at least `m` times." This is the only
no-ladder-sound reading of "`t` comes up": availability on *some* path guarantees
nothing (the player routes around it), but a `min` bound guarantees every solver
is *offered* `t`. It still does **not** guarantee *engagement* — a peer that is
also live can preempt it ("I'd just place the digit"). For sound practice prefer
exclusive alternatives (§7), which force engagement and exclude preemption. Kept
as a flagged maybe; its cost vs forbidden is to be settled empirically.

## 7. Exclusive alternatives (strict xor)

A forced crux that **each member independently routes around, and that no solve
cracks with both** — "you need this one or that one, not both." Distinct from
`force_any`, which is a forced disjunction that may **collapse** to a single
forced technique; exclusive alternatives provably does *not* collapse and
provably never doubles up. It is the sound form of "offer a technique for
practice": engagement is unavoidable, the choice is free.

For a pair `{A, B}` it is four polarized `solvable` assertions (§4):

- `¬ solvable(cap A→0, B→0)` — can't avoid both = `force_any{A, B}` (the crux is
  real).
- `solvable(cap A→0)` — a full solve without `A` exists (the B-route).
- `solvable(cap B→0)` — a full solve without `B` exists (the A-route).
- `¬ ∃` admissible solve with `uses(A) ≥ 1 ∧ uses(B) ≥ 1` — no solve uses both
  (the strict-xor clause; a both-using witness *disproves* it, exhaustion proves
  it).

Together every player is forced to use exactly one of `A`, `B`. Three things fall
out:

- **No force-both.** The two middle positives are the exact negation of "both
  required," so a puzzle that needs both *fails* — exclusive alternatives and
  force-both are opposites by construction.
- **Trivial preemption is excluded.** "I'd just place a digit" means avoiding
  both is solvable via the single → `force_any` fails → rejected. The first
  assertion *is* "no easy shortcut for this crux."
- **A and B are pure alternatives.** The xor clause forces them to do no
  independent work anywhere a solve could also reach the other — the precise
  meaning of "this puzzle is about choosing one of these two." Stronger than a
  per-crux reading (it forbids "A here, B there" across separate cruxes), hence
  lower-yield, but crisp.

**The crux is emergent, not tracked.** Strict xor is satisfiable essentially only
when A and B keep colliding as alternatives (using one forecloses the other), so
you get the "two ways to see one deduction" behaviour without ever defining or
tracking a crux. The crux is the intuition for *why* a puzzle qualifies, never an
object. Counts generalize as in `force_any` (cap to threshold−1); the n-ary form
is "force the set, each member avoidable, no solve uses two members."

**Deferred — per-crux (deduction-localized) substitution.** A weaker, more
permissive version would allow "A here, B there" and ask only that *each crux* is
either/or. That needs a crisp notion of crux, and the only crisp atom is a
**deduction** (one elimination or one placement, both a `(cell, digit)`
resolution); localizing further requires per-deduction load-bearing analysis (a
forcing sub-question) and breaks entirely for divergent-route substitutes (A and
B making *different* deductions, no shared crux). Too fuzzy/expensive to define
now — **deferred**; strict xor is the shippable form.

**Optional — demonstrable shared deduction.** If you ever want the crux *shown*
("here is the deduction — spot it as an x-wing or an xy-wing"), add a positive
`∃ reachable state where A and B are both live with a common deduction`. That
guarantees the same-deduction case, costs a shared-deduction search, and is a
bolt-on — not needed for strict xor itself.

## 8. The clue lattice (Rule 3 and "wanted state")

Treat the revealed cells as a point in the subset lattice of `G`. Let

> `V(P) ≔ uniqueness(P) ∧ Rule 1 ∧ Rule 2 ∧ ceilings ∧ live-rules` — **validity**.

`V` is **not monotone**: removing a clue can break Rule 1, and **adding** a clue
can break Rule 2 (a clue that pre-resolves a forced bottleneck makes it
avoidable). That non-monotonicity is why Rule 3 is a search, not a closed form.

"Minimal" is only *one* objective over `V`'s region:

- **minimal / irreducible** — `V(P)` and no clue removable while keeping `V`.
- **a presentation target** — `V(P)` *and* an extra predicate that may require
  *more* clues than minimal. Example: *"the forced move is needed immediately,"*
  reached by replaying an admissible path's easy preamble as givens.

Generator shape: finding *any* `V`-point is the expensive search; **advancing** a
`V`-anchor to a wanted presentation is a cheap local walk plus a re-verify.
Advancing changes the puzzle, so the re-verify is the **full stack — validity
*and* the filters of §9** — not just Rules 1–2. A "wanted state" means *still
valid and still passing its filters after advancing.* Presentation predicates
gate clue add/remove; the path-event filters only select.

## 9. Path-event filters

Not about whether a path exists, but what *happens* along one.

**Placement payoff.** Technique `t` *pays off* iff there exists an admissible path
`π` and a firing of `t` in it such that, immediately after `t`'s removals, a
placement becomes available **that was not available immediately before** `t`
fired (caused-by-`t`, not an idle firing next to a pre-existing single).

Payoff is the **existential corner of `live`**: `∃` a live `t` whose progress is
a *placement* — where forbidden / soft-cap / presence (§6) are the `max`/`min`
corners of the same quantity. So `live` unifies the whole offer-side axis.

**Visibility grade.** A grading of the *enabled placement*, not a separate rule:
**simple** (the newly enabled single is glance-obvious) vs **involved** (takes
scanning). A filter reads "`t` pays off *with a simple placement* on some path."

Asked only of puzzles that already pass Rules 1–3, so they may explore many paths
for one witnessing event. (These revive the SE "direct variant / placement
payoff" axis that `CURRICULUM.md` lists as out of scope — now opt-in.)

## 10. Grading

A function of `Π(P)` as a whole (or a heavy sample), thresholded into a band,
computed only on already-valid puzzles so it can invest heavily and explore
multiple paths. Same shape as the §9 filters — read the path space, don't
constrain the inner search — but aggregate rather than existential. A slice is
pre-empted by the ceiling/floor assertions of §5 (graded by construction); the
rest is left open.

## 11. The stack

1. **Bounded toolbox** — `uses` caps per `(technique, context)`: allowed /
   ignored / capped / conceded. Defines admissible paths `Π`.
2. **Two quantities** — `uses` (controlled; upper = definitional cap, lower =
   verified forcing) and `live` (offered; all bounds verified). Upper bounds
   aggregate by `max` over paths, lower by `min`.
3. **`uses`-rules** — polarized `solvable(P, B)`: Rule 1 (positive, base), Rule 2
   requirements (negative, tightened — sets and counts), Rule 1c ceilings
   (positive, tightened). Accept only on witness / exhaustion; budget-unknown
   rejects.
4. **`live`-rules** — over the worst-case path, separate from `solvable`:
   forbidden (`max live = 0`), soft-cap (`max live ≤ m`, peer-tightening),
   guaranteed presence (`min live ≥ m`, *maybe*). Plus **exclusive alternatives**
   = `force_any` + each-avoidable + a no-both exhaustion (four `solvable`
   searches) — a forced either/or, never both, the crux emergent. Per-crux
   (deduction-localized) substitution is *deferred*.
5. **Clue lattice** — validity `V` = conjunction of (3)+(4), non-monotone.
   Objective is *minimal* or a *presentation target*, reached by advancing a
   `V`-anchor and re-verifying the full stack.
6. **Path-event filters** — existential caused-by-`t` placement payoff (the
   `∃` corner of `live`), with a simple/involved visibility grade. Heavy,
   post-validity.
7. **Grading** — aggregate functional over the path space, partly pre-empted by
   ceilings. Heavy, post-validity. Open.

## 12. Open / not yet decided

- The **opportunity-dedup metric** for counting `live` along one path (when are
  two productive occurrences "the same opportunity"). `= 0` (forbidden) is immune;
  every nonzero `live` count depends on it.
- Whether **guaranteed presence** (`min live ≥ m`) is worth its cost vs forbidden
  or exclusive alternatives — which is harder to satisfy / steers yield better is
  to be settled empirically.
- **Per-crux (deduction-localized) substitution** — deferred: no crisp crux
  definition (a crux is at most a single deduction; localizing further needs
  per-deduction load-bearing analysis and breaks for divergent-route
  substitutes). Strict-xor exclusive alternatives ships instead.
- The exact **visibility metric** for simple vs involved (§9).
- The **grading** function itself (§10).
- Budget policy for the negative (exhaustion) searches — a yield/cost knob, not a
  correctness one.

## Relation to other docs

- `DESIGN.md` — generation principles (diversity, "necessity not appearance,"
  measure-don't-guess). Unchanged; this doc makes "necessity" precise and adds
  the offer-side (`live`) axis.
- `CURRICULUM.md` — tiers, branches, train/drill, and the original two-check
  verifier contract. Train/drill remain the common builders; the two checks are
  the singleton-requirement special case of §5 (Rule 1 + one Rule 2 requirement).
