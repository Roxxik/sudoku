# Pelánek (2014): SiSuS model, Refutation sum, Dependency

Reference: Radek Pelánek, *Difficulty Rating of Sudoku Puzzles: An Overview and
Evaluation*, arXiv:1403.7373 (March 2014), Faculty of Informatics, Masaryk
University Brno. PDF:
<https://www.fi.muni.cz/~xpelanek/publications/sudoku-arxiv.pdf>.

Local copies: [references/pelanek-2014-sudoku-difficulty.pdf](references/pelanek-2014-sudoku-difficulty.pdf)
(original PDF) and [references/pelanek-2014-sudoku-difficulty.txt](references/pelanek-2014-sudoku-difficulty.txt)
(its `pdftotext -layout` extraction, the source this note was written from).

This note is a self-contained, implementation-level summary of three artifacts
from that paper:

1. the **SiSuS** ("Simple Sudoku Solver") computational model of a human solver,
2. the **Refutation sum** difficulty metric, and
3. the **Dependency** difficulty metric.

It pulls everything needed to implement them out of the paper; it does not refer
to any code in this repo.

The paper's central empirical claim is that human Sudoku difficulty has **two
roughly orthogonal sources**, and a good metric needs both:

- **Complexity of individual steps** — how hard the single hardest logical
  deductions are. Captured by Refutation sum (and by the classic Serate /
  Fowler per-technique-rating metrics).
- **Structure of dependency among steps** — at each point in the solve, how many
  independent forced moves are simultaneously available (many parallel options =
  easy; a narrow forced chain = hard). Captured by Dependency.

Refutation sum alone gives Pearson r = 0.68 / 0.83 against mean human solving
time on the two datasets; Dependency alone 0.67 / 0.69; their linear combination
("RD") 0.74 / 0.88; and a 4-way linear model with the Sudoku-specific Serate and
Fowler metrics ("SFRD") 0.84 / 0.95.

---

## 0. Preliminaries: state, simple techniques, contradiction

All three artifacts are built on top of two "simple techniques" and constraint
propagation. Fix this vocabulary first.

**State.** A partial assignment of the 81 cells, plus, for each empty cell, its
**candidate set** = the digits not already used in that cell's row, column, or
box. (Equivalently, maintain candidate sets incrementally as cells are filled.)

**The two simple techniques** (the *only* Sudoku-specific knowledge in the
model; they fall straight out of the rules):

- **Naked single** — an empty cell whose candidate set has exactly one element.
  That digit is forced into the cell.
- **Hidden single** — a unit (row, column, or box) in which some digit `d` has
  exactly one empty cell that can still take `d`. `d` is forced into that cell.

In SiSuS these two are treated as having **equal difficulty** (no numeric
weights). A cell is "fillable by a simple technique" if it is the target of a
naked single or a hidden single in the current state. Call the set of such cells
the **simple-fillable set** (the grey cells in the paper's Fig. 4).

**Contradiction / inconsistency.** A state is inconsistent if either:
- some empty cell has an **empty candidate set** (a cell with nowhere to go), or
- some unit has a digit `d` for which **no** empty cell can take `d` and `d` is
  not yet placed in that unit.

These are the two failure signatures the refutation rollout watches for.

**Assumptions baked into the model** (the paper states these explicitly and
argues they hold for well-posed 9x9 Sudoku):
- the solver never makes a mistake and never has to backtrack;
- the solver can always make progress (simple technique, or a refutation-guided
  fill) without true search;
- the puzzle is well-posed (unique solution). The model is allowed to consult the
  known unique solution when it fills a cell that no simple technique resolves.

---

## 1. The general model

The model repeats, until the grid is solved:

```
loop until solved:
    L := the simplest logic technique that yields *some* result in the current state
    a := an action that technique L can perform now
         (if L has several applicable actions, choose one uniformly at random)
    apply a -> new current state
```

"Simplest technique that yields a result" means: prefer the simple techniques
(naked/hidden single); only when *no* simple technique applies do you fall back
to the harder "refutation" technique of Section 2.

The model is **randomized** in step `a` (which of the currently-available moves
to take). Every metric below is therefore an **average over many independent
runs** of the model.

---

## 2. SiSuS = the general model with simple techniques + refutation

**SiSuS** ("Simple Sudoku Solver") is the general model instantiated with:

- two hard-wired equal-difficulty simple techniques: **naked single** and
  **hidden single**; and
- a fallback, used only when neither simple technique applies, that picks the
  next cell via a **refutation score**.

Crucially, SiSuS has **no numeric per-technique parameters** of the
Serate/Fowler kind (Table 1 in the paper). The "difficulty" of a hard step is
derived, not assigned.

### 2.1 One SiSuS step

```
if simple-fillable set is non-empty:
    pick one cell from it uniformly at random
    fill it with its forced digit         # naked/hidden single
else:
    # stuck: no simple technique applies
    for every empty cell c: compute refutation_score(c)        # Section 3
    c* := the empty cell with the smallest refutation_score
          (ties broken at random)
    fill c* with its correct digit (from the known unique solution)
    record refutation_score(c*) as the difficulty of this hard step
```

Interpretation: when stuck, the human is imagined to do *what-if* reasoning —
to convince themselves of `c*`'s value by refuting every other candidate for it.
The cell that is cheapest to settle this way is the one a human is most likely to
crack next, and the cost of settling it is the difficulty of that step.

For all 9x9 puzzles the authors tried, a stuck state always had at least one cell
with a finite refutation score, so the fallback always makes progress. (For
harder CSPs you would need a rule for the all-infinite case.)

---

## 3. Refutation score (per cell) and `ref_v` (per candidate)

Assume we are in a stuck state (simple techniques give nothing) and we are
scoring an empty cell `c`. Let `c`'s candidate set be `{v*, v1, v2, ...}` where
`v*` is the correct digit and the `vi` are the **wrong candidates**.

For each wrong candidate value `v`, define:

> **`ref_v`** = the smallest number of **simple steps** needed to demonstrate
> that assigning `v` to `c` is inconsistent. If `v` cannot be refuted using only
> simple steps, `ref_v = infinity`.

i.e. tentatively place `v` in `c`, then propagate using only naked/hidden
singles; `ref_v` is how many single-placements it takes before a contradiction
(Section 0) surfaces. If propagation **stalls** (no more simple steps available)
without producing a contradiction, then `v` is *not* refutable by simple steps
and `ref_v = infinity`.

Then:

> **ideal refutation score of `c`** = sum over all wrong candidates `v` of
> `ref_v`.
> If *any* wrong candidate has `ref_v = infinity`, the cell's score is
> `infinity`.

A cell is cheap (easy to settle) when *all* of its wrong candidates die quickly
under pure single-propagation.

### 3.1 Exact vs. randomized `ref_v`

- **Exact** `ref_v` = the *minimum* number of simple steps to a contradiction. It
  can be computed by breadth-first search over reachable states, but that is
  expensive and, the paper argues, does not match human behavior (humans don't do
  systematic minimal search).

- **Randomized** `ref_v` (what SiSuS actually uses): place `v` in `c`, then
  repeatedly apply **one randomly chosen applicable simple step** and count steps
  until a contradiction appears. The resulting count is a single random rollout's
  estimate of `ref_v`. If the rollout stalls with no applicable simple step and
  no contradiction, that rollout reports `ref_v = infinity`.

  Because the surrounding metric already averages over many model runs (and the
  rollouts themselves are random), this randomized estimate is what feeds the
  metric — no separate BFS needed.

```
ref_v(c, v):                       # randomized rollout
    s := copy of current state
    place v in cell c in s; update candidate sets
    steps := 0
    loop:
        if s is inconsistent:      # empty candidate set, or unit-digit with no home
            return steps
        moves := all simple-technique placements available in s   # naked + hidden singles
        if moves is empty:
            return infinity        # stalled without contradiction -> not refutable by singles
        m := a uniformly random element of moves
        apply m to s; steps := steps + 1

refutation_score(c):
    total := 0
    for each wrong candidate v of c (every candidate of c except the solution digit v*):
        r := ref_v(c, v)
        if r == infinity: return infinity
        total := total + r
    return total
```

Notes for implementers:
- The rollout works on a **scratch copy** of the state; it must not mutate the
  real solve state.
- "Wrong candidate" needs the known solution digit `v*` of `c` to exclude it.
  Since the puzzle is well-posed and solved offline, `v*` is available.
- The candidate set used is the current (stuck-state) candidate set of `c`, which
  by construction has >= 2 members (else `c` would have been a naked single).

---

## 4. Refutation sum metric

The Refutation sum metric measures the **total step-complexity** of a puzzle: how
much expensive what-if reasoning a solve requires.

> **Refutation sum** = mean, over **30 randomized runs** of the SiSuS model, of
> the **sum of the refutation scores recorded during a run**.

Within a single run, the recorded refutation scores are exactly the
`refutation_score(c*)` values logged at each *stuck* step in Section 2.1 (the
difficulty of each hard step). Sum them across the whole run; that is the run's
refutation sum. Average over 30 runs.

```
refutation_sum(puzzle):
    acc := 0
    repeat 30 times:
        run SiSuS to completion
        acc := acc + (sum of refutation_score(c*) over the run's stuck steps)
    return acc / 30
```

Properties / sanity checks:
- **Higher = harder.** Positively correlated with human time (r = 0.68 / 0.83).
- For a **"simple Sudoku"** (one solvable purely by naked + hidden singles) the
  simple-fillable set is never empty, the fallback never fires, no refutation
  score is ever recorded, and the metric is ~0. This is exactly why Refutation
  sum *cannot* discriminate among easy puzzles — and why the Dependency metric
  (Section 5), which *does* vary across simple Sudokus, is a needed complement.

(The paper's phrasing "mean sum of refutation scores over 30 runs" is terse; the
reading above — sum the per-stuck-step minimal scores within a run, then average
over runs — is the one consistent with the simple-Sudoku-gives-~0 behavior the
paper relies on.)

---

## 5. Dependency metric

The Dependency metric measures the **width of the forced-move frontier** over the
course of a solve: at each step, how many independent simple moves are available.
Many simultaneous options means the steps are largely independent (parallelizable
in the solver's head) and the puzzle feels easy; few options means a narrow,
sequentially-dependent chain and the puzzle feels hard — even when every
individual step is trivial.

### 5.1 Per-step possibility count

During a SiSuS run, at each step record:

> **possibilities(step) = the number of cells in the simple-fillable set** at
> that step (the count of distinct cells currently resolvable by a naked or
> hidden single).

(In the paper's 4x4 example the first three steps have 3, 4, 4 possibilities.)
For a hard puzzle this count is small (often 1-2) through the middle of the
solve; for an easy puzzle it is large throughout.

### 5.2 Averaging into a single number

Because the run is randomized, do several runs and, **for each step index `i`**,
average `possibilities(i)` across runs to get a smooth per-step curve (the
paper's Fig. 7). Then collapse that curve to one scalar:

> **Dependency = the mean of the per-step possibility counts over the first `k`
> steps** (averaged over runs as above).

```
dependency(puzzle, k):
    runs := several SiSuS runs, each logging possibilities(0..)
    for i in 0 .. k-1:
        mean_i := average over runs of possibilities(i)
    return average of mean_0 .. mean_{k-1}
```

### 5.3 Choice of `k`

Only the **early** part of the solve carries signal: late in any solve there are
many forced moves regardless of difficulty (the curves converge), so those steps
add noise, not information. Hence cap at the first `k` steps.

From the paper's Table 2 (Pearson r vs. human time, by `k`):

| k                       |  5   | 10   | 15   | 20   | 25   | 30   | 35   | 40   |
|-------------------------|------|------|------|------|------|------|------|------|
| fed-sudoku.eu (all)     | 0.42 | 0.57 | 0.65 | 0.67 | 0.64 | 0.58 | 0.51 | 0.47 |
| fed-sudoku.eu (simple)  | 0.57 | 0.64 | 0.70 | 0.73 | 0.74 | 0.73 | 0.70 | 0.66 |
| sudoku.org.uk (all)     | 0.31 | 0.54 | 0.62 | 0.70 | 0.74 | 0.76 | 0.76 | 0.73 |
| sudoku.org.uk (simple)  | 0.62 | 0.71 | 0.76 | 0.79 | 0.80 | 0.80 | 0.78 | 0.75 |

The optimum is mildly dataset-dependent but lands in **k = 20..30**, and the
metric is insensitive to the exact value inside that band. Use **k ~ 25** as a
default.

### 5.4 Directionality (important)

The raw Dependency value is **mean number of options**, so **larger = easier**.
Its relationship to *difficulty* (solving time) is therefore **inverse**: fewer
options ⇒ harder ⇒ longer time. Table 2 reports the correlation *magnitude*
(~0.67-0.69 overall); the sign against time is negative. When you feed Dependency
into a linear difficulty model, expect it to take a **negative coefficient** (or
negate / take the reciprocal of the value first so that "bigger = harder" holds
uniformly).

Dependency is the weakest single metric overall, but it is the **best metric on
simple Sudokus** specifically (where Refutation sum / Serate / Fowler are all
flat), and it is **partly orthogonal** to the step-complexity metrics — which is
why adding it to a combination lifts the correlation.

---

## 6. Combining them (for context)

The paper's combined metrics are plain linear models, with coefficients fit on a
training half and evaluated on a test half:

- **RD** = linear combination of `Refutation sum` and `Dependency`
  (model-only; no Sudoku-specific per-technique tables). r = 0.74 / 0.88.
- **SFRD** = linear model over `Serate`, `Fowler`, `Refutation sum`,
  `Dependency`. r = 0.84 / 0.95 — the paper's best.

(`Serate` = max difficulty of any technique used, per Sudoku Explainer's
per-technique ratings; `Fowler` = G. Fowler's tool's per-technique-count
expression. Both are the classic "complexity of individual steps" family that
Refutation sum approximates without hand-set parameters.)

So a minimal, parameter-light implementation is **RD**: run SiSuS, accumulate
Refutation sum and Dependency, and fit a 2-term linear model. Adding the two
Sudoku-specific per-technique metrics is what buys the final jump to r = 0.95.

---

## 7. Implementation checklist

1. Constraint-propagation core with incremental candidate sets, plus detectors
   for **naked single**, **hidden single**, and the two **contradiction**
   signatures (Section 0).
2. A **randomized `ref_v` rollout** on a scratch state (Section 3.1) and a
   per-cell **refutation_score** that sums `ref_v` over wrong candidates, short-
   circuiting to infinity (Section 3).
3. The **SiSuS step** (Section 2.1): random pick from the simple-fillable set;
   on stalls, score all empty cells and fill the min-score cell with its known
   solution digit, logging that score.
4. Drive SiSuS to completion repeatedly; per run log (a) the sequence of recorded
   refutation scores and (b) the per-step possibility counts.
5. **Refutation sum** = mean over 30 runs of the per-run sum of recorded scores
   (Section 4).
6. **Dependency** = per-step-averaged possibility counts, meaned over the first
   `k ~ 25` steps (Section 5); remember it is inverse to difficulty.
7. Optionally fit RD / SFRD linear models (Section 6).

Knobs and defaults pinned by the paper: simple techniques = {naked single,
hidden single} at **equal** weight; `ref_v` via **random rollout** (not BFS);
**30** runs for Refutation sum; **k = 20..30 (~25)** for Dependency; the model
has **no other numeric parameters**.
