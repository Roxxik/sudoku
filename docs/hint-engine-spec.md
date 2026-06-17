# Hint Engine — Specification

Status: prototype spec. This document defines a **new, standalone hint engine** that replaces
the current solver-byproduct hints. It is written so that two independent implementations can be
built from it and their outputs compared field-by-field.

## 0. Purpose

The hint engine helps a human player who is mid-solve. Its job is to **meet the player where they
are** and hand out the *smallest useful piece of information first*, with the option to reveal more.

It must do three things the current hints do not:

1. Be **correct regardless of the player's pencil marks.** Today's hints are computed off the
   player's candidate marks, so a wrong or incomplete mark produces a wrong hint. The new engine
   derives ground truth itself and reasons on that.
2. Understand the player's **marks** — both candidate (center) marks and Snyder (corner) marks —
   well enough to say "you forgot a candidate here", "you can place a Snyder mark there", or
   "this mark is stale, remove it".
3. **Order** hints so they point toward the next placement, not merely by raw technique difficulty.

This is *not* the generator's solver and shares no code or performance constraints with it. It runs
at human-reaction-time scale (milliseconds per call are fine). Correctness and clarity dominate.

## 1. Platform & shape (hard constraints)

- The engine MUST be able to run **client-side in a web browser** (it is invoked from the web
  frontend, which holds the play state).
- The engine MUST be a **pure function**: `analyze(state) -> report`. Same input, same output. No
  I/O, no global mutation, no time/randomness, no hidden state between calls.
- Determinism is required (see §9). Two runs on equal input, and ideally two independent
  implementations, must produce the **same ordered list of hints** in the canonical fields.

The choice of implementation technology is out of scope for this document and must not influence it.

## 2. Coordinates & vocabulary

- The grid is 9×9. **Cell index** is `0..80`, row-major: `index = row*9 + col`, with `row`,
  `col` each `0..8`, row 0 at top, col 0 at left.
- **Digit** is `1..9`. The value `0` means "empty".
- **Box index** is `0..8`: `box = (row/3)*3 + (col/3)` (integer division).
- A **unit** is a row, a column, or a box (9 cells each; 27 units total). A unit reference is the
  pair `(kind, index)` where `kind ∈ {row, col, box}` and `index ∈ 0..8`.
- **Peers** of a cell `i` are all cells sharing its row, column, or box, excluding `i` (20 cells).
- Human-readable prose MAY use 1-based `r{row+1}c{col+1}` (e.g. `r3c5`); the schema always uses the
  0-based cell index.

## 3. Input: the play state

```
State {
  given:      [81] of 0..9   // the immutable puzzle. 0 = not a clue.
  value:      [81] of 0..9   // current board: givens + player placements. 0 = empty.
                             // For every given cell, value == given.
  candidates: [81] of Set<1..9>  // player center/pencil marks. Meaningful only where value==0.
  snyder:     [81] of Set<1..9>  // player corner/Snyder marks.   Meaningful only where value==0.
}
```

Assumptions (the prototype need not validate these): `value` agrees with `given` on given cells.
The board is **not** assumed to be uniquely solvable, and need not be solvable at all — the player
may be exploring a non-unique board, or may have placed digits that create a contradiction. The
engine must produce sound hints in all of these cases. Player placements may be wrong; that is what
error hints are for.

### 3.1 No solution, no uniqueness assumption (important)

The engine is **given no solution grid** and must **not** compute or depend on one. It must not
assume the puzzle has a unique solution, or any solution. Every hint must be **purely logical** —
derivable by the player from the current board and the rules of Sudoku alone.

Consequence: there is no way to single out "the" solution digit (there may be none, or several), so
forgotten-candidate hints cannot flag a solution digit; the honest substitute is "a candidate no
known technique can rule out" (§7.1). Detecting that a board is doomed is limited to the immediate,
search-free contradictions in §6 — pinpointing *which* earlier placement was the mistake would
require search and is out of scope.

> Scope note: computing a solution and rejecting non-unique boards (e.g. on puzzle import) is a
> separate concern of the app and is never a prerequisite for hints. The engine works the same on
> unique, non-unique, and unsolvable boards.

## 4. Derived ground truth

For each empty cell `i`, the engine computes **`trueCandidates[i]`**:

```
trueCandidates[i] = { d in 1..9 : no peer j of i has value[j] == d }     // for value[i]==0
```

This is plain peer elimination from placed digits — no player marks, no oracle.

All technique detection (§5) operates on `trueCandidates`. The player's `candidates` and `snyder`
sets are used only by the housekeeping rules (§7) and never feed technique detection.

The engine does **not** iteratively apply technique eliminations before detection: every technique
is detected against the single basic `trueCandidates` grid. (Ordering, §8, surfaces the easiest
applicable one first.) The only lookahead is the bounded one-step probe in §8.2.

## 5. Techniques (prototype set)

Each detector yields zero or more **technique findings**. A finding records its defining cells,
its digit(s), the units involved, and the candidate **eliminations** it licenses
(`{cell, digit}` pairs that are currently in `trueCandidates` and would be removed). A finding is
emitted only if it has at least one elimination, **or** it is a placement (singles).

All prototype techniques are sound on any rule-legal board, unique or not: each follows from the
per-unit constraint that every digit appears exactly once in each row, column, and box, so none can
eliminate a candidate that some valid completion would need. Techniques that *assume* a unique
solution (unique rectangles, BUG, …) are therefore excluded — not just deferred (§5.8).

### 5.1 Naked single  (kind `naked_single`, placement)
A cell `i` with `|trueCandidates[i]| == 1`. The placement is `{cell: i, digit: that digit}`.

### 5.2 Hidden single  (kind `hidden_single`, placement)
In some unit `U`, a digit `d` is a true candidate of exactly one empty cell `i` of `U` (and `d` is
not already placed in `U`). The placement is `{cell: i, digit: d}`, with `U` as the defining unit.

Dedup: at most **one placement hint per cell**. If a cell is both a naked single and a hidden
single, emit only the naked single.

### 5.3 Locked candidates — pointing  (kind `locked_candidates_pointing`)
Within box `b`, all true-candidate cells for digit `d` lie in a single row `r` (or single col `c`).
Eliminate `d` from the cells of `r` (resp. `c`) that are **outside** box `b`.

### 5.4 Locked candidates — claiming  (kind `locked_candidates_claiming`)
Within row `r` (or col `c`), all true-candidate cells for digit `d` lie in a single box `b`.
Eliminate `d` from the cells of box `b` **outside** `r` (resp. `c`).

### 5.5 Naked pair  (kind `naked_pair`)
Two cells in a unit `U` whose true-candidate sets are both exactly `{a, b}`. Eliminate `a` and `b`
from every other cell of `U`.

### 5.6 Hidden pair  (kind `hidden_pair`)
Two digits `{a, b}` in a unit `U` whose true-candidate cells within `U` are exactly the same two
cells `{i, j}` (each of `a`, `b` appears in `i` and `j` and nowhere else in `U`). In cells `i`, `j`
eliminate every candidate **except** `a` and `b`.

### 5.7 X-Wing  (kind `x_wing`)
For a digit `d`: two rows `r1`, `r2` in each of which `d` is a true candidate in exactly the same two
columns `{c1, c2}` (exactly two per row). Eliminate `d` from all other cells in columns `c1`, `c2`.
The column-oriented mirror (two columns confining `d` to the same two rows) applies symmetrically.

### 5.8 Out of scope for the prototype (design must accommodate)
Naked/hidden triples & quads, swordfish/jellyfish, XY/XYZ/W-wings, coloring, chains, ALS. The
detector registry and the finding shape MUST make adding these later a local change.

## 6. Error hints (category `error`)

Both are derivable from constraints alone — no solution, no search.

- **`rule_violation`** — a placed digit (a cell where `value != 0`) duplicates the same digit in one
  of its units. Report the offending cell(s) and the unit.
- **`contradiction`** — the board, as currently filled, cannot be completed, detected in its
  immediate (search-free) form:
  - an empty cell `i` with `trueCandidates[i] == ∅` (no digit can go there) — report `cells: [i]`; or
  - a unit `U` and a digit `d` not yet placed in `U` for which no empty cell of `U` has `d` as a true
    candidate (the digit has nowhere to go) — report `units: [U]`, `digits: [d]`.

  A contradiction may stem from a wrong player placement or from an inherently unsolvable board; the
  engine reports the conflict but does not claim which earlier move was the mistake (that needs
  search).

Givens are trusted and never produce `rule_violation`.

## 7. Housekeeping hints (category `housekeeping`)

These reconcile the player's marks with ground truth. They are gated so a player who deliberately
marks lightly (e.g. Snyder-only) is not spammed.

### 7.1 Forgotten candidate  (kind `forgotten_candidate`)
For an empty cell `i` **where `candidates[i]` is non-empty** (the player is actively pencil-marking
that cell): a digit `d` with `d ∈ trueCandidates[i]`, `d ∉ candidates[i]`, and `d` is **not**
eliminated by any technique in the implemented set (i.e. it is a genuine surviving candidate, not one
the player could have correctly ruled out).

> Rationale for the gate (decides the open question): a missing candidate that *is* eliminable by a
> known technique is treated as "the player correctly deduced its absence" and is **not** reported.
> Only a missing candidate that survives all known eliminations is "forgotten". Without a solution
> oracle the engine cannot single out *the* solution digit (there may be none or several on a
> non-unique board); "survives all known eliminations" is the honest substitute, and on a uniquely
> solvable board it necessarily includes the solution digit.

Suppress a `forgotten_candidate` for cell `i` if a **placement hint** already exists for `i`
(the placement is the actionable form of the same information).

### 7.2 Impossible candidate  (kind `impossible_candidate`)
For an empty cell `i` with non-empty `candidates[i]`: a digit `d ∈ candidates[i]` with
`d ∉ trueCandidates[i]` (a peer already holds `d`). Report it as a mark to remove.

### 7.3 Snyder convention
A Snyder mark for digit `d` in cell `i` is *valid* iff, within `i`'s box `b`, `d` is a true candidate
in **exactly two** cells of `b`, and `i` is one of them. (The strict 2-cell convention; 3-cell is a
possible future config but the prototype uses 2.)

- **`snyder_suggested`** — a box `b` and digit `d` with `d` confined to exactly two cells `{i, j}` of
  `b`, where the player has not placed the Snyder mark `d` in both `i` and `j`. Report the missing
  cell(s).
- **`snyder_stale`** — a player Snyder mark `d` in cell `i` that is no longer valid: `value[i] != 0`,
  or `d ∉ trueCandidates[i]`, or `d` is no longer confined to exactly two cells of box `b`. Report it
  as a mark to remove.

## 8. Ordering

Every hint gets a `category` and, within the progress group, a numeric `score`. The final list is
sorted by:

1. **Category priority**: `error` (0), then `progress` (1) = placements ∪ techniques, then
   `housekeeping` (2). Errors first because an uncorrected error invalidates downstream advice.
2. **Within `progress`**: ascending `score` (§8.1). This realizes "singles on top, then the path to
   the next placement, with difficulty as the tiebreak."
3. **Within `error`**: `rule_violation` before `contradiction`, then canonical tiebreak (§9).
4. **Within `housekeeping`**: `forgotten_candidate`, then `impossible_candidate`, then
   `snyder_suggested`, then `snyder_stale`; then canonical tiebreak.

### 8.1 Progress score (decides the difficulty-vs-path question)
```
score = placementDistance * 1000 + difficultyRank
```
Lower sorts earlier. `placementDistance` is the primary key (path to placement), `difficultyRank`
the secondary key (easiest first).

`difficultyRank` (relative ranks, spaced to allow inserting future techniques):

| kind                         | rank |
|------------------------------|------|
| naked_single                 | 10   |
| hidden_single                | 20   |
| locked_candidates_pointing   | 30   |
| locked_candidates_claiming   | 35   |
| naked_pair                   | 40   |
| hidden_pair                  | 50   |
| x_wing                       | 70   |

### 8.2 `placementDistance` (one-step lookahead)
- A placement hint (naked/hidden single) has `placementDistance = 0`.
- A technique hint has `placementDistance = 1` if applying its eliminations to a copy of the
  `trueCandidates` grid makes a **new** single appear that is attributable to the elimination —
  specifically: some cell that *lost* a candidate becomes a naked single, **or** some unit containing
  an eliminated cell gains a hidden single. Otherwise `placementDistance = FAR` (use the value `2`).

This is the only lookahead; it does not recurse.

Net effect: all singles first (score 10/20), then techniques that immediately unlock a placement
ordered by difficulty (1030, 1035, 1040, …), then techniques that only make indirect progress
(2030, …).

## 9. Determinism & comparison

The list MUST be a total, reproducible order. After the sort keys above, break remaining ties by:
`(kind, sorted cells ascending, sorted digits ascending)`.

**Canonical fields** (must match across implementations): for each hint —
`category, kind, cells, digits, units, eliminations, placement, difficultyRank,
placementDistance, score`, and the **position** of each hint in the list.

**Free fields** (prose, may differ): all human-readable `text`. Comparison of two implementations is
done on the canonical fields only.

## 10. Progressive disclosure

Each hint carries a ladder of disclosure levels — "one tiny bit, then more". Each level has free
`text` and a structured `reveals` payload naming which canonical fields become known at that level.
The UI shows level 1 first and reveals further levels on demand.

| level | name     | reveals (structured)                                  | example prose (free)                                  |
|-------|----------|-------------------------------------------------------|-------------------------------------------------------|
| 1     | Nudge    | `category`, `kind`                                    | "A cell can be placed right now."                     |
| 2     | Locate   | `units` (+ `digits` for placements)                   | "Look at box 2 — for the digit 7."                    |
| 3     | Identify | `cells`, `digits`                                     | "Cell r3c5, digit 7."                                 |
| 4     | Resolve  | `placement` or `eliminations` + the reasoning         | "Place 7 in r3c5: only cell in box 2 that can be 7."  |

Error and housekeeping hints use the same ladder: e.g. `contradiction` L1 "The board can no longer
be completed", L2 "look at box 4", L3 "cell r4c5 has no remaining candidates", L4 "every digit is
already used by a peer — an earlier placement must be wrong".

## 11. Output: the report

```
Report {
  hints: [ Hint ]              // fully sorted per §8/§9
  meta: {
    progressAvailable:  bool   // any progress hint (placement or technique) exists
    boardContradictory: bool   // any `contradiction` error hint exists
    note: string | null        // e.g. "no known technique applies" (prototype's set exhausted)
  }
}

Hint {
  category:          "error" | "progress" | "housekeeping"
  kind:              one of the kinds named in §5/§6/§7
  cells:             [cellIndex]          // primary cells involved (defining cells / target cell)
  digits:            [1..9]               // digit(s) involved
  units:             [ {kind, index} ]    // units involved (may be empty)
  eliminations:      [ {cell, digit} ]    // candidates to remove (techniques); else []
  placement:         {cell, digit} | null // the placement, for singles; else null
  difficultyRank:    int                  // §8.1 table; 0 for non-progress kinds
  placementDistance: int                  // §8.2; 0 for non-progress kinds is fine
  score:             int                  // §8.1; for non-progress kinds set to 0
  disclosure:        [ {level, text, reveals} ]   // §10
}
```

Note: `category` here is `progress` (placements and techniques merged), distinct from the internal
detector grouping in §5. Singles are `progress` with `placementDistance = 0`.

The engine returns **all** detected hints, fully ordered. Choosing how many to show and driving the
level-by-level reveal is the UI's job, not the engine's.

## 12. Prototype scope (read this)

**Implement a prototype, not the full engine.** Build exactly the technique set in §5.1–§5.7
(naked single, hidden single, locked candidates pointing & claiming, naked pair, hidden pair,
X-Wing), the full error suite (§6), and the full housekeeping suite (§7), with the ordering (§8),
determinism (§9), disclosure (§10), and report shape (§11).

The single-only cases are already handled well by the existing hints; the cases worth prototyping are
the harder ones — naked/hidden pairs and X-Wing — together with the mark-reconciliation
(housekeeping) and error behavior that today's hints lack entirely. Stop at X-Wing; everything in
§5.8 is explicitly deferred.

## 13. Non-goals

- Not a full solver; no chains/coloring/ALS; no multi-step planning beyond the one-step lookahead.
- No solution oracle and no uniqueness assumption; the engine never computes a solution. Detecting
  unsolvability is limited to the immediate contradictions of §6 (deeper unsolvability needs search).
- No uniqueness-based techniques (unique rectangles, BUG, …); they are unsound without a uniqueness
  guarantee, which the engine does not have.
- No UI, no persistence, no animation; the engine only returns the report.
- No wording requirements — prose is illustrative and free to differ.
- No performance target beyond "interactive"; clarity wins over speed.
