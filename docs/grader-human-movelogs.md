# Grader — human calibration from the app's own move logs

What the uploaded move logs (`move_logs`, see [`backend-prototype.md`](backend-prototype.md)
*Move-log sync*) say about the grader, and how to re-measure it as more solves come in. The
external-dataset calibration ([`grader-external-calibration.md`](grader-external-calibration.md))
is the sibling: same harness, same one hard rule (rank only *within* a technique bucket), but here
the humans are this app's own players and we see every move, not just a label.

> Status: **MEASURED 2026-10-14** on 1006 logs / 969 solves (2026-06-23 .. 2026-10-13). Tooling:
> [`scripts/movelog-analyze`](../scripts/movelog-analyze) (replay + report + dataset groups) and
> [`core/examples/movelog_probe.rs`](../core/examples/movelog_probe.rs) (solver per board state).
> **No grader change follows from it yet** — the trunk is already at the data's noise ceiling and
> the hard-technique samples are too thin to tune on. One lead: in the hard buckets Pelánek's
> refutation sum ranks these players clearly better than our rating does (§4.3, §6).

## 1. Re-running

```
scripts/movelog-analyze --download          # refresh datasets/movelogs/sudoku-dump.sql, replay, report
cargo run --release -p generator-lab --example datasets_correlate -- --no-pelanek
```

The script writes three groups next to the external ones in `datasets/normalized/` (gitignored,
like all of `datasets/`, so no player data enters the repo) and merges them into `catalog.json`;
`datasets_correlate` picks them up with the rest. One row per puzzle, label averaged over its
solves, `weight` = number of solves:

| group | solves counted | label |
|---|---|---|
| `app/solve_time` | clean: no hint reveal at any stage | mean solve seconds |
| `app/solve_time_hinted` | all, hint reveals allowed | mean solve seconds |
| `app/hint_reveals` | all | mean hint reveals per solve |

Cheat-mode solves and solves that used `applyStep` / `fillCandidates` / `eraseMistakes` are never
counted. The report (`datasets/movelogs/report.txt`) covers what the scoreboard can't: validation,
the noise ceiling, the singles table (§4.2) and the help-level table (§4.4).

## 2. What a log holds, and how it is replayed

Each log is the full `tracker.js` timeline: every committed edit (placements, pencil marks, erases,
undo/redo, stale-mark removal), hint-panel interactions, pauses and the solve, each stamped with
the solve clock. No identifier beyond the game's own random ids, so **players can't be told apart**;
the settings snapshot on the `session` event is too uniform to help (one setting combination covers
72% of solves).

The script replays each solved log through the same undo/redo model `play.js` uses, carrying
placed digits **and both kinds of pencil mark** (center = candidate set, corner = Snyder pin,
combined exactly as `solverBoard` feeds the hint engine). That gives, for every placement, the
board the player was looking at and the candidates the hint engine would have reasoned over. The
probe then reports, per moment, the hardest curriculum technique on the easiest path to the next
placement — once from the player's notes, once from placed digits alone — plus the singles
available.

**Validation** — two independent checks, so a drifting port can't hide behind a noisy log:

- *Port:* the script's Python copies of `wrongMarkCells` / `staleMarkDetail` are run against the
  functions extracted from `play.js` itself (under node) on every hint-open state: 4209/4209 agree.
  A mismatch is a hard failure — it means `play.js` changed.
- *Log:* the replay is compared with what the app logged — the final grid (963/969), every
  `hintOpen`'s health status (4169/4209) and every `removeStaleMarks` cell list (611/611). A solve
  that disagrees anywhere is excluded (12 of 969); below 98% agreement the run fails.

Two client-side gaps the replay works around:

- **The solving placement is never logged.** The win fires inside the commit and
  `discardSession()` runs before the `place` event is tracked, so a correct replay ends one empty
  cell short. Harmless for analysis; the replay accepts it.
- **About 1% of solves lose a pencil-mark change.** In the excluded solves the app reported
  health `ok` where the replayed notes contain a wrong Snyder pin, and the port check proves the
  app's own function flags that board — so the log is missing an edit the app applied. No event in
  those logs explains it; the double-tap rollback (`tracker.dropLast()` paired with
  `history.pop()`) is the likeliest place for the tracker and the undo stack to drift apart. Not
  chased further.

## 3. Who plays: everyone pencils everything, immediately

- 950 of 963 solves reach full pencil marks (≥90% of empty cells marked), the median one **before
  the first placement**, with ~55 empty cells.
- Mostly in **corner** marks used as full candidate sets, not as Snyder pairs: in a typical solve
  every empty cell carries corner marks and 92% of them equal the full candidate set. `cornerPins`
  turns that into the same candidate masks, so the hint engine is unaffected.
- The method: swipe each digit across the whole board, then *Remove stale marks* — 547 of 957
  solves open exactly that way, **median 4 s** from the first note to the cleanup. Full candidates
  are nearly free in the app, where on paper they take minutes; this is an artifact of app play,
  not a player trait, so pencil style is no use as a skill signal.

Consequence for everything below: these players see naked singles as readily as hidden ones, and
their notes often already contain eliminations the bare grid doesn't (§4.4).

## 4. Findings

### 4.1 The trunk rating is at the noise ceiling

On `app/solve_time` the trunk bucket (n=535 puzzles) scores **+0.43**; on `app/solve_time_hinted`
+0.50 — in line with the +0.34..+0.57 it reaches on the external sets (calibration §4.5). The
ceiling: 29 puzzles were solved cleanly by two different players, and the second time follows the
first at only **+0.49**. One human predicts another barely better than the grader does.

A human-calibrated alternative was tried and **tied**: fit time-per-placement from the moments
(by singles available × fill stage, §4.2), walk each puzzle 30 times picking a random available
single, sum the fitted costs. Held-out puzzles, 10 random halvings: Spearman 0.443 (model) vs 0.438
(current rating) against solve time. Nothing to gain from tuning the trunk on solve times.

### 4.2 Singles: how many are available is what costs time

Time to the next placement, in units of the player's own median single in that solve (unaided,
singles-gated moments):

| singles available | 1 | 2 | 3 | 4 | 5-6 | 7-9 | 10+ |
|---|---|---|---|---|---|---|---|
| median time | 3.97 | 1.51 | 1.28 | 1.15 | 0.99 | 0.88 | 0.81 |
| moments | 1392 | 3827 | 5302 | 6878 | 14065 | 13521 | 5268 |

The effect holds at every fill stage (a fuller board is also faster, independently). This is the
frontier-width mechanism the trunk rating is built on, seen directly in human play.

**Naked vs hidden singles cost the same** once the number of available singles is fixed: with one
available, naked-only 4.3 vs hidden-only 5.1 (n=187 / 1004); with two, 3.1 vs 3.3. The raw gap
(naked-only *looks* slower) is because naked-only moments usually have fewer options. A cell that is
**both** a naked and a hidden single is found about twice as fast (2.2 / 1.0). CURRICULUM.md's
"hidden easier than naked" is a paper-solving claim (it prices writing the pencil marks); a note
there now says so.

### 4.3 Hard techniques, within the bucket

The hard-puzzle solves are hint-assisted almost without exception (hint reveals per solve: ~4 for
naked pair up to ~27 for naked quad), so solve time there partly measures how the panel was used.
Within-bucket signal still exists — `datasets_correlate`, our rating vs the label:

| bucket | n | `hint_reveals` | `solve_time_hinted` |
|---|---|---|---|
| trunk | 572 | +0.36 | +0.50 |
| naked-pair | 57 | −0.14 | −0.04 |
| hidden-pair | 52 | +0.05 | +0.14 |
| x-wing | 34 | +0.24 | +0.19 |
| xy-wing | 40 | +0.49 | +0.49 |
| xyz-wing | 25 | +0.54 | +0.39 |
| w-wing | 34 | +0.39 | +0.10 |
| naked-triple | 19 | +0.10 | −0.34 |
| jellyfish | 12 | +0.24 | +0.80 |
| **aggregate** (n-weighted) | 873 | **+0.31** | **+0.38** |

**Against the bar, the hard buckets trail.** With Pelánek on (`datasets_correlate` without
`--no-pelanek`), the group aggregates tie (ours − bar = −0.01 on all three groups) only because the
trunk, where we win, carries most of the weight. Over the non-trunk buckets alone, n-weighted:

| label | non-trunk n | ours | Pelánek refutation | Pelánek dependency | refutation better in |
|---|---|---|---|---|---|
| `hint_reveals` | 301 | +0.20 | **+0.35** | −0.20 | 7 / 12 buckets |
| `solve_time_hinted` | 301 | +0.15 | **+0.32** | −0.24 | 10 / 12 buckets |

The gap is widest in the pair buckets, where our rating is flat and refutation is not (naked pair
−0.14 vs +0.24, hidden pair +0.05 vs +0.38 on `hint_reveals`).

Read the per-bucket numbers with their sample size: at n=20..60 a Spearman is uncertain by roughly
±0.15..0.3. The wings agree on both labels; **the two pair buckets — the largest — are flat to
negative**, and since Pelánek's refutation does rank them, the pair puzzles do differ in human
difficulty — our pair rating misses what makes them hard. Whether the player needs the *location*
of a technique does not track the rating at all (pooled −0.03): that depends on the player more
than on the puzzle.

### 4.4 The technique ladder: how often humans need help

Per placement whose notes-aware gate is a technique beyond singles, the strongest help used since
the previous placement. Help is staged in the panel: opening it shows only board health; the
technique list, a free name reveal and the paid location reveal come after.

| technique | grader | moments | none | name only | location |
|---|---|---|---|---|---|
| pointing | 22 | 86 | 55% | 24% | 12% |
| claiming | 26 | 41 | 56% | 20% | 17% |
| naked pair | 32 | 78 | 19% | 42% | 35% |
| x-wing | 38 | 32 | 9% | 59% | 31% |
| hidden pair | 44 | 57 | 4% | 53% | 42% |
| naked triple | 50 | 20 | 0% | 45% | 50% |
| hidden triple | 62 | 11 | 0% | 45% | 55% |
| xy-wing | 68 | 64 | 2% | 28% | 67% |
| xyz-wing | 72 | 24 | 4% | 42% | 54% |
| w-wing | 76 | 49 | 0% | 24% | 76% |
| jellyfish | 86 | 10 | 0% | 30% | 70% |

(Rows omitted: swordfish and the quads, 3-10 moments each. "Panel opened" and "list shown" make up
the rest of each row.)

- Beyond naked pair the players almost never find the technique unaided.
- The **name** is the dominant level of help and worth keeping separate: X-wing is usually found
  once named (hard to think of, easy to see), the wings and jellyfish mostly still need the
  location (hard to spot even when you know what to look for).
- The location column follows the grader's ladder closely. Two departures: **X-wing (38)** needs
  help about as often as hidden pair, and **naked triple (50)** needs the location as often as the
  hidden triple. Not enough data to move scores on.

Judging against the player's **notes**, not the bare grid, matters: 104 of 479 moments that are
hard on the bare grid were already reduced to singles by notes made earlier. The notes are taken at
the start of each interval (right after the previous placement), so eliminations made *during* the
interval don't mask the technique being looked for.

## 5. Caveats

- **Few players.** The repeat-puzzle pairs are almost all daily puzzles; everything else could be
  dominated by one or two people. Moments within a solve share a player, so the effective sample
  is smaller than the moment counts.
- **Selection.** Players choose the technique they train, so a bucket's solves come from people
  practising that technique; "needed help" mixes *hard to spot* with *not learned yet*.
- **The hint menu can be rebuilt but not its split.** The listed techniques follow from the board
  and the notes (`all_techniques` on the `solverBoard`); which of them sat under "Other" depends on
  `game.specMasks`, which isn't logged. After one look at the list, players revealed the easiest
  listed technique first 80% of the time. 81% of menus also list turbot and finned fish, which no
  generated puzzle needs; name reveals of those are browsing, not need.
- **Abandoned puzzles** (37) are too few to say where people give up.
- **Not every player shares move logs.** Passive upload (`solves`, `move_logs`) is opt-in; a daily
  submission (`daily_solves`) bypasses that, since tapping Submit is the consent. So 99 of 534
  daily solves have no move log and no `solves` row, only total time, level and paid-hint count,
  and the share is growing (Oct 2026: 50 of 115). They look like different players (paid hints
  flat across levels, 0.5-1.0 per solve, where the logged players climb from 0.1 to 2.4), so the
  logged population is not everyone. Not analysed yet: too few to stand alone. Once there are more,
  they are the best source for the noise ceiling (§4.1), since 95 daily puzzles were solved both by
  a logged and an unlogged player. Note their `hints` counts paid hints only (location reveals,
  mistake checks), so "0 hints" there is weaker than `app/solve_time`'s "no reveal at any stage".
  (Of the 975 `solves` rows only 6 lack a log, all from 2026-06-23/24, before log upload.)

## 6. What this means for the grader

- **Trunk:** leave it. It is at the noise ceiling of solve-time data (§4.1).
- **Within-technique ratings:** positive but behind the bar — Pelánek's refutation sum beats our
  rating in most hard buckets on these players (+0.35 vs +0.20), most clearly for pairs. That is
  the one lead here worth following: find what refutation sees that the pair signals lack. (It
  prices each stall by how many single-steps it takes to refute every wrong candidate of the
  cheapest cell, summed over the stalls, so it reads the whole puzzle's stall structure, where our
  rating reads the bottleneck technique's instances.) Keep these groups as a
  regression check alongside the external sets; buckets this size are too small to fit weights on.
- **Ladder (`difficulty`):** the human help rates broadly confirm it; revisit X-wing and naked
  triple when the moment counts have roughly tripled.
