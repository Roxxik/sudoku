# Web frontend — test specification

This is the behavioral spec for the Sudoku web app (`web/`). It defines what a
real automated suite must verify. It is implementation-agnostic: write it with
Playwright, WebDriver, or a CDP harness — whatever the team standardizes on. The
ad-hoc CDP scripts used during development are **not** the deliverable; this doc
is.

Each case is written as **Given / When / Then** against the real DOM and the
real wasm bridge. Selectors and storage keys are part of the contract (they are
referenced throughout the app); if you change them, update this doc.

---

## 1. Architecture under test

- Single-page app, three+ views swapped via the `hidden` attribute on
  `section.view`: `#homeView`, `#campaignView`, `#puzzlesView`, `#playView`,
  `#statsView`. Exactly one is visible at a time.
- Two wasm binaries (Trunk): the **main bridge** (`window.wasmBindings`:
  `curriculum()`, `hint(board)`, `Board`, `specMasks(target, drill)`,
  `generate*`) and a **generation web worker** (`gen_worker_loader.js`) driven by
  `gen.js`.
- Persistence in `localStorage` under key **`sudoku.games.v1`** (array of game
  records). Cheat flag under **`sudoku.cheat`** (`"1"` | `"0"` | absent).
- The campaign taxonomy is a static module `curriculum.js` (`export default
  [...]`), pregenerated at build from Rust; the app also has a `curriculum()`
  wasm fallback.

### Game record shape (`sudoku.games.v1` is an array of these)

```
{ id, kindIndex, mode: "train"|"drill",
  puzzle, solution,          // 81-char strings, '.' = empty
  givens,                    // clue count
  seed,                      // decimal string of the u64 generator seed,
                             //   or null on records made before seeds existed
  value: number[81],         // player entries, 0 = empty
  centerMarks: number[][81], // centred "usual" pencil notes per cell
  cornerMarks: number[][81], // Snyder corner notes per cell
  // (legacy records may carry `marks`; play.js loads it as centerMarks)
  elapsedMs, status: "active"|"solved",
  createdAt, lastPlayedAt, solvedAt|null }
```

### Curriculum (what the tree contains)

- Tiers shown: **Beginner**, **Intermediate**, **Expert**. (Master is omitted —
  no kinds yet.)
- Beginner = Hidden Single, **train only** (no drill).
- Intermediate = flat tier: Naked Single, Locked Candidates (Pointing), Locked
  Candidates (Claiming). Train + Drill each.
- Expert = three branches: **Single-digit (Fish)** [X-Wing, Swordfish,
  Jellyfish], **Subsets** [Naked/Hidden Pair, Naked/Hidden Triple, Naked/Hidden
  Quad], **Bivalue chains** [XY-Wing, XYZ-Wing, W-Wing]. Train + Drill each.
- Each curriculum entry: `{ kindIndex, id, difficulty, tier, branch, hasDrill }`.
  `hasDrill === (tier !== "beginner")`.

---

## 2. Test harness, conventions, and gotchas

These are load-bearing. Several are non-obvious and were the source of real bugs.

1. **Serve the built app.** `trunk serve` (dev) or serve `dist/` from `trunk
   build`. Tests drive a real browser against it.
2. **Cheat is ON by default on private/loopback hosts.** `cheatOn()` is true when
   the hostname is `localhost`, `127.0.0.1`, `10.*`, `192.168.*`, or
   `172.16–31.*`, OR `localStorage['sudoku.cheat'] === "1"`. It is OFF when the
   flag is `"0"`. **Therefore, when testing against localhost, cheat is on** —
   tests that need the non-cheat UI must `window.cheat(false)` first; tests that
   need cheat on a public host must `window.cheat(true)`.
3. **Reset between tests** by clearing storage and reloading:
   `localStorage.clear()` then reload, and wait for `#tierButtons` to have
   children before proceeding.
4. **Solve puzzles via the keyboard, never the digit pad.** The pad has a 280 ms
   same-digit double-tap gesture (pen-lock); programmatic rapid taps trigger it.
   To place a digit: select the cell, then dispatch a `keydown` with the digit.
   Selecting: dispatch a `pointerdown` event on the `.cell`.
5. **Open the hint via a real `click`** on `#hint` (the open handler is on
   `click`, not `pointerup`). While the hint panel is open, `#hint` is hidden
   together with the pad — **close the hint via its `#hintClose` (X)**, not by
   clicking `#hint` again.
6. **Generation cost varies wildly.** Beginner/Intermediate are fast (< ~1 s);
   Naked/Hidden Quad (esp. drill) can take many seconds and occasionally exhaust
   the attempt budget. Give generation a long timeout (≥ 120 s for quads) or
   prefer fixtures (see §3).
7. **Solve time can be `0:00`** for an instant programmatic solve — assert the
   format, not a nonzero value, unless you deliberately wait.
8. **First-paint timing must be measured from inside the page** (an observer
   installed *before* navigation). Polling from the test driver underreports —
   it cannot see a render that already happened.
9. The board has 81 `.cell`s, row-major (index = `row*9 + col`).

### Recommended page-object selectors

| Area | Selector |
|---|---|
| View containers | `#homeView`, `#campaignView`, `#puzzlesView`, `#playView`, `#statsView` |
| Home tiers | `#tierButtons .nav-btn` (Beginner, Intermediate, Expert in order) |
| Home continue | `#continueLastBtn`, `#continueListBtn` |
| Home stats entry | `#statsBtn` (in the Home top bar; hidden until earned) |
| Campaign | `#campaignBack`, `#campaignTitle`, `#campaignBody` |
| Branch buttons | `#campaignBody .nav-btn` |
| Technique row | `#campaignBody .tech-row`, name `.tech-name`, badge `.count-badge` |
| Mode buttons | `.mode-btn` (`.mb-label`, `.mb-sub`, `.mode-btn.solved`) |
| Explainer | `#campaignBody .mode-explainer` (`p` lines) |
| Puzzles list | `#puzzlesBack`, `#puzzlesList .continue-item` |
| Board | `#board .cell` (`.given`, `.filled`, `.selected`, `.peer`, `.same`, `.hl-unit`, `.hl-cell`, `.hl-place`, `.hl-elim`) |
| Pad | `#playPad`, `#undo`, `#redo`, `#notes`, `#erase`, `#hint`, `.key[data-digit]` |
| Hint panel | `#hintPanel`, `#hintTitle`, `#hintClose`, `#hintBody` |
| Hint content | `.hint-banner.ok/.bad`, `.banner-icon`, `.hint-actions`, `.hint-bigbtn`, `.hint-list`, `.hint-row`, `.hint-row.hint-child`, `.hint-row.focused`, `.hint-row-text` (`.hint-primary`, `.hint-secondary`, `.hint-deduction`), `.hint-more`, `.hint-apply`, `.hint-harder`, `.hint-note` |
| Solved screen | `#solvedDialog`, `#solvedTime`, `#solvedHome`, `#solvedNew` |
| Generation overlay | `#loadingOverlay`, `#loadingText`, `#loadingRetry` (error-state "Keep searching"), `#loadingCancel` |
| Stats | `#statsBack`, `#statsBody`, `.stats-summary`, `.stats-table`, `.stats-tier` |
| Stats history | `.hist-list`, `.hist-item`, `.mini` (thumbnail), `.hist-seed` (cheat only), `.hist-copy` |
| Play menu | `#menuBtn`, `#menuList`, `.menu-item[data-action="restart"|"copy"|"fillCandidates"]` (fillCandidates = `#menuFill`, cheat only) |
| Play seed (cheat) | `#playSeed` (under the board; hidden unless cheat + record has a seed) |

---

## 3. Fixtures and helpers (strongly recommended)

To keep the suite deterministic and fast, do **not** generate puzzles for most
tests. Instead seed `localStorage` directly.

- **`seedGames(records[])`** — set `sudoku.cheat`/`sudoku.games.v1` and reload.
- **`makeGame(overrides)`** — a valid game record. Keep a couple of canned
  (puzzle, solution) pairs of known difficulty/spec so you don't depend on the
  generator. At minimum: a Beginner (hidden-single) puzzle and one mid-solve
  Expert/Subset puzzle.
- **`solve(page)`** — read the active game's `solution`, then for each empty cell
  select it (`pointerdown` on the `.cell`) and `keydown` the solution digit.
- **`openHint(page)`** — real click on `#hint`; wait for `#hintPanel:not([hidden])`.
- **`setCheat(page, bool)`** — `window.cheat(bool)`.
- **`drillRow(li)`** — repeatedly click the row's `.hint-more` to the end,
  capturing `.hint-row-text` and the board highlight counts at each step.

Reserve **live generation** (driving the worker) for the dedicated generation /
cancellation suite (§5) and one happy-path end-to-end (§4).

### Testability gaps to flag to the team
- The worker **self-seeds** its RNG (still no _input_ seed from the UI, so output
  isn't asserted exactly), but it now **returns** the seed it drew; it is stored
  on the record (`seed`) and shown under cheat. Reproducing a puzzle from
  `(seed, target, drill)` is possible via `generateLab` but not yet wired to a UI
  control.
- There is no app-level "reset all" beyond clearing `localStorage`.

---

## 4. Suite: navigation & first paint

**N1 — Boot renders Home.** Given a cleared app, When it loads, Then `#homeView`
is visible and `#tierButtons` has exactly **3** buttons (Beginner, Intermediate,
Expert), and `#campaignView`/`#playView`/`#statsView` are hidden.

**N2 — Home paints without waiting for wasm (regression).** Given a cold load
with the wasm download throttled/slow, When the page loads, Then the campaign
tree (`#tierButtons` children) is present **before** the main `*_bg.wasm`
finishes downloading. (Measure with an in-page observer installed via a
pre-navigation script; assert tree-render time ≪ wasm-finish time. The two must
be decoupled — Home must not block on `await init()`.)

**N3 — Tier drill-down.**
- Beginner / Intermediate (single-branch tiers): clicking the tier button opens
  `#campaignView` showing technique rows directly (no branch level).
- Expert: clicking opens `#campaignView` showing **3 branch buttons**
  (`#campaignBody .nav-btn`). Clicking a branch shows its technique rows.

**N4 — Back navigation.** From an Expert technique list, `#campaignBack` returns
to the branch list; from there, `#campaignBack` returns to `#homeView`. From a
Beginner/Intermediate technique list, `#campaignBack` returns straight to Home.

**N5 — Stats button visibility (Home top bar).** `#statsBtn` is **hidden** with
zero solved games and **visible** once **≥ 1** puzzle is solved (Stats also hosts
the solved-puzzle history, so it appears as soon as there is anything to show).
(Seed solved games to test both sides.) Clicking it opens `#statsView`.

---

## 5. Suite: generation (web worker) & cancellation

These require the live worker.

**G1 — Launch generates and opens Play.** Given Home, When you pick a technique
+ mode (e.g. Beginner → Train), Then `#loadingOverlay` appears, then `#playView`
becomes visible with a board whose givens match a generated puzzle, and a new
`active` game record exists in storage.

**G2 — Cancellation.** Given a slow target (Expert → Subsets → Hidden Quad →
Drill), When `#loadingOverlay` is shown and you click `#loadingCancel`
mid-generation, Then the overlay closes, you remain on Home (`#playView` stays
hidden), and **no** new game record was created. Afterwards a fresh, fast
generation still works (the worker respawns).

**G3 — Budget exhaustion (best effort).** If generation fails (attempt budget),
the overlay enters its error state (`#loadingOverlay.error`, spinner hidden),
`#loadingText` shows the failure message, `#loadingRetry` ("Keep searching")
becomes visible, and `#loadingCancel` reads "Close". Clicking Close dismisses the
overlay; no game is created.

**G4 — Keep searching (uncapped retry).** Given the error state from G3, When you
click `#loadingRetry`, Then the overlay returns to its loading state (spinner
back, `#loadingText` "Still searching…", `#loadingRetry` hidden) and generation
restarts for the **same** (technique, mode) uncapped — it runs
until it finds a puzzle (then opens `#playView` with a new game) or you cancel it
via `#loadingCancel` (which terminates the worker, same as G2). To exercise this
deterministically, force the first attempt to fail (e.g. build the worker with a
tiny `MAX_ATTEMPTS`) so the capped request gives up and the uncapped retry
succeeds.

---

## 6. Suite: persistence, Continue, previews

Use fixtures (§3) — no generation needed.

**P1 — Continue visibility & ordering.**
- 0 active games: `#continueLastBtn` and `#continueListBtn` both hidden; only
  Campaign shows.
- 1 active game: `#continueLastBtn` visible, `#continueListBtn` hidden.
- ≥ 2 active games: both visible.
- Order is fixed top-to-bottom: Campaign, then Continue-last, then Continue-list
  (entries hold their position regardless of which are shown).

**P2 — `lastPlayedAt` ordering (not creation order).** Given games A then B
created (B newer), Then "Continue last" targets B. When A is opened/resumed and
exited, Then "Continue last" targets A. (Continue sorts by `lastPlayedAt`, which
is bumped on open and on every move — not by `createdAt`.)

**P3 — Continue-last preview.** `#continueLastBtn` contains a mini board preview
(`.mini`) with **81** cells, showing givens + the player's entries. The
**Continue-a-puzzle link** (`#continueListBtn`) has **no** preview (it's a link,
not a single puzzle).

**P4 — Continue page (`#puzzlesView`).** `#continueListBtn` opens it; it lists
all active games (`.continue-item`), each with a **larger** (~2×) preview.
Resuming an item opens it in Play. `#puzzlesBack` returns Home.

**P5 — Solved games are retained.** A solved game stays in storage with
`status:"solved"`, `solvedAt`, and final `elapsedMs`; it feeds Stats and the
solved badges but does **not** appear in Continue. The history list on the Stats
page is covered in §14 (ST3).

---

## 7. Suite: campaign tree presentation

**T1 — Mode buttons present per `hasDrill`.** Beginner technique rows have only a
**Train** button; all other rows have **Train** and **Drill**.

**T2 — Train/Drill buttons are equal height regardless of solved state.** A
button with a best time and one without render the same height (the `.mb-sub`
line is always present, empty when unsolved — no placeholder dash).

**T3 — Per-mode solved marker.** After solving a Train of a technique (seed a
solved game), the **Train** `.mode-btn` has class `solved` and `.mb-sub` reads
`"{count} solved · {best time}"`; the **Drill** button is unmarked with an empty
sub-line. (Train and Drill are tracked independently.)

**T4 — Solved-count rollups.** Solved counts roll up: a `.count-badge` shows on
the technique row, the branch button, and the tier button, summing the solves
beneath. Unsolved levels show no badge.

**T5 — Mode explainer (techniques pages).** Each techniques page ends with a
plain (non-card) `.mode-explainer`:
- Beginner: a single descriptive line (train-only).
- Intermediate: a shared line ("…hidden singles are also needed throughout the
  puzzle.") + a **Train** line + a **Drill** line. Train mentions "the other
  techniques on this page"; Drill says "only the technique you pick is required;
  the other techniques on this page won't be needed to finish the puzzle."
- Expert branch (e.g. Fish): shared line mentions "all the Beginner and
  Intermediate techniques are also needed throughout the puzzle."; Train mentions
  "the simpler fishes above the one you picked"; Drill the same peers "won't be
  needed to finish the puzzle." (Subset → "subsets", Bivalue → "wings".)

**T6 — Expert branch-ordering note.** The Expert page (the 3 branch buttons)
shows a `.mode-explainer` note: branches are only roughly ordered by difficulty,
difficulty mainly climbs within a branch, work easier-in-a-branch first.

---

## 8. Suite: play view (timer & win)

**W1 — No running timer.** During play there is no visible running clock (the
elapsed time is tracked in the background only).

**W2 — Win detection & solved screen.** When the board is filled to match the
stored `solution`, the `#solvedDialog` opens showing `#solvedTime` formatted as
`m:ss` (or `h:mm:ss`), the game record flips to `status:"solved"` with `solvedAt`
set. `#solvedHome` returns Home; `#solvedNew` generates another of the same spec.

**W3 — Restart.** The play menu (`#menuBtn` → `.menu-item[data-action="restart"]`)
clears the player's entries/marks and resets the elapsed time to 0; the game
returns to `status:"active"`.

**W4 — Pause on leave / tab hidden.** Elapsed time accrues only while Play is the
active view and the tab is visible (pauses on Home / `visibilitychange` hidden).

**W5 — Copy puzzle (export).** The play menu (`#menuBtn` →
`.menu-item[data-action="copy"]`) copies the active game's `puzzle` line (the
clue string, '.' = empty — **never** the `solution`) to the clipboard. Available
regardless of cheat. The item briefly reads "Copied!" then the menu closes.

---

## 9. Suite: hints — error stage

Open the hint with the board in each state.

**H-E1 — Logical contradiction outranks solved-state.** Given a placed digit
that duplicates within a row/column/box, When you open the hint, Then `#hintTitle`
is "Mistake" and a **red** banner (`.hint-banner.bad` + `.banner-icon`) reads a
contradiction message — even if there are also solution mismatches (the logical
error subsumes them).

**H-E2 — Solved-state mistake.** Given a player entry that does not match the
solution but creates no duplicate, When you open the hint, Then the red banner
reads "One of your entries doesn't match the solution."

**H-E3 — "Show me where".** Clicking the error stage's "Show me where"
(`.hint-bigbtn`) highlights the offending cells on the board (`.hl-elim`).

**H-E4 — Erase mistakes (cheat).** With cheat on, an "Erase mistakes"
(`.hint-bigbtn.cheat`) button clears the wrong entries (undoable), then the hint
re-evaluates.

---

## 10. Suite: hints — status (first) layer

Board is error-free and unsolved.

**H-S1 — Reassurance banner.** Opening the hint shows `#hintTitle` "Hint" and a
**green** banner (`.hint-banner.ok` + `.banner-icon`) reading "No mistakes so
far." with **no** technique rows yet — it does not reveal what is possible.

**H-S2 — Big stacked actions.** The actions are large, full-width, and stacked
vertically (`.hint-actions` is a column of `.hint-bigbtn`): "Show available
techniques", plus (cheat) "Apply easiest".

**H-S3 — Reveal.** Clicking "Show available techniques" replaces the body with
the technique tree and sets `#hintTitle` to "Available moves".

**H-S4 — Solved board.** If the board is already complete & correct, opening the
hint shows title "Solved" and a note, no banner/tree.

---

## 11. Suite: hints — technique tree (spec-aware)

The tree is organized by the puzzle's **spec** (`window.wasmBindings.specMasks(
kindIndex, mode==="drill")`), not by raw difficulty.

**H-T1 — In-scope shown, others hidden.** The primary list (`.hint-list`)
contains exactly the **Allowed + Forced** techniques (spec `baseline`) currently
applicable. A "**Show other techniques (N)**" toggle (`.hint-harder`) reveals the
rest (Conceded + out-of-scope, including techniques the lab doesn't model).

**H-T2 — Train vs Drill membership.**
- train(target): the simpler same-branch techniques are Allowed → shown in the
  primary list (e.g. train(Naked Triple): Naked Pair / Hidden Pair appear up
  front).
- drill(target): those simpler same-branch peers are **Conceded** → they appear
  only under "Show other techniques" (e.g. drill(Naked Triple): Naked Pair /
  Hidden Pair are hidden).
- Verify directly against `specMasks`: `baseline` = Allowed|Forced,
  `inScope` = Allowed|Forced|Conceded, `forced` = the target. (You may unit-test
  `specMasks(target, drill)` independently of the board.)

**H-T3 — Spec beats difficulty.** An out-of-scope but *easier* technique stays in
"other": e.g. on train(Naked Quad), X-Wing (difficulty 38) is in "other" even
though the in-scope Naked Triple (50) is harder.

**H-T4 — Beginner board.** On a Beginner (train Hidden Single) board, the primary
list is just **Hidden Single**; everything else is under "Show other techniques".

**H-T5 — Grouping by kind.** Multiple instances of one technique collapse to a
single header row labelled `"<Name> · N spots"`. Distinct kinds are separate
rows. Rows are sorted by difficulty within a group.

**H-T6 — Tier labels.** Each row's first reveal level reads "A <Tier>
technique" where tier is derived from the step's player-facing difficulty
(< 15 Beginner, < 30 Intermediate, < 100 Expert, else Master). Locked Candidates
must read **Intermediate** (not "Advanced") — i.e. the tier comes from
difficulty, not an id lookup.

**H-T7 — Stuck.** Error-free, unsolved, no implemented technique applies →
revealing shows a "No technique … applies here yet." note.

---

## 12. Suite: hints — staggered reveal & board highlights

A row reveals progressively via its `.hint-more` ("Reveal more"); the focused row
drives the board highlight.

**H-R1 — Single technique levels.** A single-instance row reveals in order:
`A <Tier> technique` → `<Name>` → (if it has a house) `in <unit>` → (if it has
focus cells) `at <cells>` → the deduction (`R#C# = d` / `R#C# ≠ d`, monospace
`.hint-deduction`). Text is cumulative.

**H-R2 — Highlight: first blue, then both.** As a row drills:
- name/tier: no cell highlight.
- focus-cells step: focus cells go **blue** (`.hl-cell`), **no red**.
- deduction step: focus cells **stay blue** AND the affected cells light up
  **red** `.hl-elim` (or **green** `.hl-place` for a placement) — i.e. blue
  first, then both, never red-before-blue and never red without the focus.
- A unit (when present) shows as a faint always-on context (`.hl-unit`).

**H-R3 — Non-unit techniques (fish/wings) split focus vs affected.** A technique
with no single house (e.g. X-Wing, Skyscraper, XY-Wing) still has a focus-cells
step then a deduction step — i.e. it does not jump straight to the answer. There
is **no** separate "affected cells" text step (the deduction step carries that).
Example sequence for an X-Wing: `X-Wing` → `at R…,R…,R…,R…` (blue) → `R…≠d, …`
(blue + red).

**H-R4 — Multi-instance expansion is reveal-driven.** For a `"<Name> · N spots"`
header: revealing it to the name then clicking "**Show the N spots**" inserts
**N** child rows (`.hint-row.hint-child`). Immediately after expansion the
children show only the technique name with **no** board highlight. The **first
"Reveal more"** on a child triggers its highlight (highlights are reveal-driven,
not produced by tapping/selecting the row). Children show the bare name (no
"· N spots" suffix).

**H-R5 — Focused row styling.** The currently focused row has class `focused`;
for a child row the accent (focus) border applies to the left edge too (it must
not be overridden by the child's indent border).

**H-R6 — One focus at a time.** Drilling/focusing a different row moves the
highlight to it (only one row's highlight is active).

---

## 13. Suite: cheat mode

**C1 — Enable/disable resolution (tri-state).** `cheatOn()` = `"1"` forced on /
`"0"` forced off / absent → default to private-host. On localhost (private),
cheat is on by default; `window.cheat(false)` turns it **off** (Apply controls
disappear and `#hint` loses its `cheat` color class); `window.cheat(true)`
restores it.

**C2 — Long-press toggles cheat.** A **≥ 5 s** press-and-hold on `#hint` toggles
cheat (the persistent color flip is the only indicator — no hold animation). The
press that ends the long-hold is **swallowed** (does not open the panel). A
normal short tap opens the panel as usual; afterwards the cheat state reflects
the toggle.

**C3 — Apply easiest (status layer).** With cheat on, the status layer's "Apply
easiest" applies the easiest in-scope move (undoable). After applying you remain
on the **status layer** (banner still shown) — it is spammable; the filled-cell
count increases by one each click.

**C4 — Apply easiest (technique layer) stays on the technique layer.** On the
technique tree, "Apply easiest" applies the easiest in-scope move and the panel
**stays on the technique tree** (title remains "Available moves", no banner) — it
does **not** bounce back to the status layer.

**C5 — Per-row Apply.** Each technique/instance row (cheat on) has an "Apply"
button that applies that specific deduction (undoable) and re-renders the tree.

**C6 — Applied moves are undoable.** Any cheat apply goes through the normal
commit path; `#undo` reverts it.

**C7 — Seed display (play view + history).** With cheat **on**, the play view
shows `#playSeed` under the board reading `"Seed: {record.seed}"` for a record
that has a seed (hidden when the record has none). Toggling cheat off hides it;
toggling on (incl. the long-press of C2) shows it without reloading. The Stats
history shows the same seed per item — see ST3.

**C8 — Fill all candidates (cheat menu).** The play menu (`#menuBtn`) shows a
**"Fill all candidates"** item (`#menuFill` → `.menu-item[data-action="fillCandidates"]`)
**only while cheat is on** — the `<li id="menuFill">` is revealed each time the
menu opens iff `cheatOn()`, hidden otherwise. Clicking it pencils the full
candidate set (every digit not placed by a row/col/box peer) into every empty
cell's **center notes** (`centerMarks`), overwriting them, in **one undoable**
step. Filled and clue cells are untouched; corner notes are not changed.

**C9 — Apply easiest strikes notes.** With candidates filled (C8), an "Apply
easiest" / per-row "Apply" whose deduction is an elimination (`≠`) removes that
digit from the target cell's notes (undoable). Without filled notes the
elimination is a silent no-op (nothing to strike) — eliminations only become
visible once candidates are penned.

**C10 — Hints reason over notes (cheat).** With cheat on, the board handed to
`hint()` carries the player's **center** notes as a candidate mask, so the engine
reasons over the *reduced* set — this is what lets repeated "Apply easiest" make
progress past singles on puzzles needing eliminations (each elimination narrows
the notes the next hint sees). A cell with no center notes stays grid-derived;
corner (Snyder) notes are not forwarded; off cheat no notes are forwarded (hints
reflect the true position).

**C11 — Placing a digit clears peer candidates (cheat).** With cheat on, placing
a digit `d` in cell `c` (manually via pad/keyboard, or via an "Apply" placement)
removes `d` from the center **and** corner notes of every row/col/box peer of
`c`, in the same undoable step. Off cheat, peer notes are left untouched.

---

## 14. Suite: stats page

Seed solved games.

**ST1 — Empty / hidden.** With **zero** solved games, `#statsBtn` is hidden and
the page is unreachable from Home (see N5).

**ST2 — Populated.** With **≥ 1** solved game, `#statsView` shows a
`.stats-summary` (total solved + total time) and per-tier `.stats-table`s with a
row per solved `(technique, mode)` giving count, best, and average time. Times
are formatted `m:ss` / `h:mm:ss`. `#statsBack` returns Home.

**ST3 — Solved-puzzle history.** Below the tables, a "Solved puzzles" section
(`.hist-list`) lists one `.hist-item` per solved game, **most-recently-solved
first** (by `solvedAt`), each with a `.mini` thumbnail of the finished board and
a meta line `"{Train|Drill} · {time} · {date}"`. Active games never appear. With
cheat **off**, no `.hist-seed`; with cheat **on**, each item adds a `.hist-seed`
reading `"Seed: {record.seed}"`, or `"Seed: (not recorded)"` when the record has
no `seed`. Each item also has a `.hist-copy` button (regardless of cheat) that
copies that game's `puzzle` clue line to the clipboard (never the `solution`) and
briefly reads "Copied".

---

## 15. Suite: input & board (carried over, still valid)

**I1 — Digit entry & notes.** Keyboard 1–9 places into the selected cell; `0` /
Backspace / Delete erases; arrow keys move selection; `n` cycles the input mode.
The `#notes` button cycles the mode too — **Normal → Center → Corner → Normal** —
its label naming the current mode and its colour flagging it (blue accent for
Center, amber for Corner). In a note mode, entering a digit toggles that note kind
instead of placing a value. The pad mirrors this; the pad's same-digit double-tap
(within 280 ms) pen-locks a digit (then taps place/note that digit; double-tap on
a cell does the opposite, pairing the value with the active note kind — Center in
Normal/Center mode, Corner in Corner mode).

**I1b — The two note kinds.** Center notes render as one row of the sorted
candidates in the middle of the cell, shrinking to stay on one line as the count
grows. Corner (Snyder) notes fill corner-first (TL, TR, BL, BR, then edges, then
centre) but are laid out in row-major reading order, so the digits always read
ascending left-to-right, top-to-bottom. Both clear when a value is placed; both
are persisted (`centerMarks`/`cornerMarks`) and survive reload.

**I2 — Undo.** `#undo` reverts the last board mutation; it is disabled when there
is nothing to undo; a place→convert double-tap collapses to one undo step.

**I2b — Redo.** `#redo` re-applies the most recently undone move (Ctrl/Cmd+Y or
Ctrl/Cmd+Shift+Z); it is disabled when there is nothing to redo. Making a *new*
move after an undo clears the redo stack (so `#redo` goes disabled). A no-op tap
does not clear it.

**I2c — Undo/redo persist across reopen.** Make a few moves, undo one, leave the
puzzle (Home) and reopen it from Continue: `#undo` and `#redo` are still enabled
exactly as left, and stepping them reproduces the same board states. (The stacks
ride along in the game record's `history` / `redo` fields; old records without
them load as empty.) Restart clears both.

**I3 — Givens are immutable.** Cells with class `given` cannot be changed/erased.

**I4 — Selection highlight cleared on hint open.** When the hint panel opens, any
cell selection highlight (`.selected` / `.peer` / `.same`) is cleared so it does
not mix with the hint's own highlights.

---

## 16. Coverage matrix (quick index)

| Feature | Cases |
|---|---|
| Navigation / first paint | N1–N5 |
| Generation & cancel | G1–G3 |
| Persistence / Continue / previews | P1–P5 |
| Campaign presentation | T1–T6 |
| Timer & win | W1–W5 |
| Hints: error stage | H-E1–H-E4 |
| Hints: status layer | H-S1–H-S4 |
| Hints: technique tree (spec) | H-T1–H-T7 |
| Hints: reveal & highlights | H-R1–H-R6 |
| Cheat mode | C1–C11 |
| Stats | ST1–ST3 |
| Input & board | I1–I4 (incl. I2b redo, I2c persist across reopen) |
