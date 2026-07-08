# Mode wiring map

How a game **mode** is wired, after the mode refactor. A *mode* is a puzzle source
the player can start and resume — its own home entry, page, and way of being
titled/labelled once it is a saved game. There are five:

| Mode | Identified by | Sub-key |
|---|---|---|
| **Campaign** technique games | `kind: "campaign"` | `kindIndex` + `mode` ("train"/"drill") |
| **Review** lessons | `kind: "review"` | `lesson` (a REVIEW_LESSONS key) + `mode` |
| **Daily** puzzles | `kind: "daily"` | `daily: { day, level }` |
| **Custom** puzzles | `kind: "custom"` | `spec` + `specMasks` + `label` |
| **Imported** puzzles | `kind: "imported"` | `label` (no spec) |

## The model

Every game record carries a persisted **`kind`** discriminator (`web/store.js`), and
**`web/modes.js`** is the single registry that answers "what kind of game is this, and
how does it title / label / bucket?" `modeOf(g)` is a field read on `g.kind` (with a
curriculum-free inference fallback for the rare unmigrated record). Each mode is one
descriptor:

```
{ id, title(g), line(g), statsLabel(g), statsBucket(g) }
```

- `title(g)` — the short card / header title.
- `line(g)` — the full one-line identity (Continue meta + play header), no time suffix.
- `statsLabel(g)` — the "mode" column in the Stats history list.
- `statsBucket(g)` — `{ group, key }` for the stats aggregator, or `null` (custom /
  imported are headline totals only).

Every display site dispatches through the descriptor instead of re-sniffing the record:

- `home.js` `gameTitle`/`continueMeta` → `modeOf(g).title` / `.line`
- `play.js` `setTitle` → `modeOf(g).line`
- `stats.js` `historyCard` → `modeOf(g).title` / `.statsLabel`
- Card menus ("Open spec") → `isCustom(g)` (only custom exposes its spec)

There is no per-surface `daily → review → custom → campaign` ladder, and no
`reviewOf`/`dailyOf` sniffers. Old records (saved before `kind`) are stamped once at
boot by `migrateModes()` (`web/app.js` → `web/modes.js`), which also recovers a Review
game's `lesson`+`mode` and backfills campaign `specMasks`.

## Stats

`modes.statsBuckets(store.solvedGames())` buckets solved games by `modeOf(g).statsBucket(g)`
into `group → key → { count, sumMs, bestMs, lastMs, lastAt }`, at fine granularity (a
plain Play and a Play-from-Forced apart). Consumers:

- Campaign technique page (`playButton`) and Review buttons (`reviewButton`) badge each
  start via `campaignStats` / `reviewStats`.
- Tree / chapter rollups sum a group with `countOf`.
- The Stats **screen** folds the forced pair into one row (`mergeBuckets`) and adds a
  Review section (per lesson/mode) and a Daily section (per level).

## Generation & the wasm boundary

Every mode generates from the same explicit **usage array** built in JS:

- Campaign — `campaign.js` `campaignUsages(curriculum, kindIndex, mode)` (a JS mirror of
  Rust's `Spec::train_isolated`/`drill_isolated`).
- Review / Daily — `review.js` `lessonUsages` / `daily.js` `dailyUsages`.

`gen.js` translates the positional usage array into an **id-keyed** `spec` object
(`{ "<kebab-id>": code }`) before posting to the worker, which maps ids back to its
kinds via `kinds::NAMES`. So the JS `kindIndex` ordering never crosses the boundary and
may drift from Rust (see `web/curriculum.js`). The spec masks for the hint tree fall out
of the usages (`spec.js` `masksFromUsages`) and are stored on every generated record, so
`play.js` reads `game.specMasks` uniformly. The main-thread wasm bridge is now only the
solver (`hint`, `solveLine`); the old `specMasks(kindIndex)` roundtrip and the worker's
campaign `target` path are gone.

## Adding a mode

1. **Spec builder** — a module that builds the mode's usage array (like `review.js` /
   `daily.js`), if it isn't a plain campaign/custom spec.
2. **Descriptor** — one entry in `modes.js` `MODES` (+ a `statsBucket` if it should have
   per-mode stats), and, if migrating old records, a branch in `classifyLegacy`.
3. **Launch** — tag `kind` (and any sub-key) on the `createGame` call (`app.js` `launch`,
   or a direct create like `importPuzzle`).
4. **Home entry + page** — a `homeView` entry and a `<section>` view; register the view id
   in `app.js` `VIEWS` and wire its navigation in `home.js`.
5. **Win screen** — only if it needs a bespoke dialog (`play.js` `showSolved`).
6. **Build** — add any new module to the `modulepreload` + `data-trunk copy-file` lists in
   `index.html` (miss the copy-file and it 404s in prod).

Steps 1–3 are the mode's identity and behaviour; 4–6 are its surface.
