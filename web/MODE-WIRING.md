# Mode wiring map

Every place you must touch to add a new **mode** to the frontend, and why each is
awkward. Written as the first step of a frontend refactor: this catalogs the
current scattering so the refactor can collapse it. Line numbers are accurate as of
the daily-puzzle work; treat them as anchors, not gospel.

## What "mode" means here

A *mode* is a puzzle source the player can start and resume — something that needs
its own home entry, its own page, and its own way of being titled/labelled once it's
a saved game. Today there are five:

| Mode | How it's stored | Identified by |
|---|---|---|
| **Campaign** technique games | `kindIndex` + `mode` ("train"/"drill") | `kindIndex` is a number |
| **Custom** puzzles | `mode:"custom"`, a `spec` usage array + `specMasks` + `label` | `mode === "custom"`, no other tag |
| **Review** lessons | a `force_any` custom game | `reviewOf(g)`: its spec matches a lesson's |
| **Daily** puzzles | a `force_any` custom game + `daily:{day,level}` tag | `dailyOf(g)`: the tag |
| **Imported** puzzles | `mode:"custom"`, no spec | `mode === "custom"`, `spec` null |

## The core problem

There is **no first-class mode discriminator and no mode registry.** The data model
has exactly one privileged shape — a *campaign* game keyed by `kindIndex` — and
everything else is a "custom game" carrying a `spec`. New modes (Review, Daily) are
*not* a new kind of record; they're custom games that each display site has to
**re-identify after the fact** by inspecting the spec or an ad-hoc tag.

Three consequences, and they are the whole reason this doc exists:

1. **Duplicated identity.** `reviewOf(g)` and `dailyOf(g)` are copy-pasted into
   three modules (home / stats / play), because each surface re-derives "what kind
   of game is this" independently. There is no `modeOf(g)`.
2. **Replicated precedence.** Expert I's daily spec is byte-identical to Review
   Lesson 1's, so *every* site must check `dailyOf` **before** `reviewOf` or a daily
   mislabels as a review lesson. That ordering is an invariant with no single owner —
   it was the source of two bugs in the daily work (play header said "Custom · Daily
   …"; the continue card showed no daily indication).
3. **If-else ladders per surface.** Title, meta, and label are each computed by a
   per-mode `if (daily) … else if (review) … else if (custom) … else campaign`
   chain, written out once per display site (four of them).

A refactor wants a single `Mode` descriptor (`id`, `title(g)`, `meta(g)`,
`specMasks(req)`, `view`, `winScreen`, `statsBucket`, menu items, home entry) that
every site dispatches through — instead of N sites each re-sniffing the record.

---

## The touch-point catalog

To add one mode today you touch all of the following.

### 1. Spec vocabulary & per-mode builder
- `web/spec.js` — `OFF/ALLOW/FORCE/CONCEDE`, `masksFromUsages`, `forcedIndices`.
  The shared usage-array vocabulary every spec mode builds on.
- A per-mode builder module: `web/review.js` (lessons), `web/daily.js` (difficulties).
  Each defines its sets, a `usages(curriculum, …)` builder, a label, and any
  identity data. **A new mode adds another such module** — fine in isolation, but it
  then has to be re-imported at every display site (see §4).

### 2. Generation
- `web/gen.js` `generate({...})` (gen.js:69) — the worker request builder. New
  per-mode request fields are added to the destructure **and** the message object
  (`seed` was the daily addition).
- `wasm/src/bin/gen_worker.rs` — `main` message parse + `generate` / `generate_custom`
  / `run_spec`. The campaign-vs-custom-spec split lives here; new request fields
  (e.g. the pinned `seed`) thread through all three.

### 3. Launch
- `web/app.js` `launch(req)` (app.js:170) — the central launcher. `req` is either
  `{kindIndex, mode, fromForced}` (campaign) or `{usages, label, specMasks, forceAny,
  seed, daily, fromForced}` (custom-ish). **Every new per-mode field is plumbed here**
  twice: into the `genReq` for the worker and into the `createGame` call.
- `web/app.js` `regenerate(g)` (app.js:252) — the "New puzzle" re-launch; branches on
  `g.spec`. A mode that should *not* be regenerable (daily) relies on the win screen
  simply not offering it, rather than this function knowing about modes.
- `web/app.js` `initHome({...})` callbacks (`onLaunch`, `onLaunchSpec`) — the launch
  entry points home.js calls.

### 4. Identity re-derivation — DUPLICATED ×3
- `web/home.js` `reviewOf` (home.js:323) + `dailyOf` (home.js:331)
- `web/stats.js` `reviewOf` (stats.js:196) + `dailyOf` (stats.js:203)
- `web/play.js` `reviewOf` (play.js:2498) + `dailyOf` (play.js:2506)

Three identical pairs. Each new mode adds a third helper to **all three** files, and
the daily-before-review precedence must be re-stated in each. This is the sharpest
edge.

### 5. Display: titles / metas / labels — one ladder per surface
- `web/home.js` `gameTitle(g)` (home.js:312) — continue/puzzles-list card title.
- `web/home.js` `continueMeta(g)` (home.js:297) — card meta line. Note the hero
  "Continue last puzzle" card uses a **generic** title, so the mode identity must be
  packed into the meta here (the bug: daily meta lacked it).
- `web/play.js` `setTitle(g)` (play.js:2387) — the play-view header.
- `web/stats.js` `historyCard(g)` (stats.js:98) — history card title + mode label.

Four places, each with the same `daily → review → custom → campaign` cascade.

### 6. Card menus / actions
- `web/home.js` `cardMenu(g)` (home.js:197) — the continue card's overflow menu; the
  "Open spec" item is gated `!reviewOf(g) && !dailyOf(g)` (a daily/review isn't a
  user-editable spec). Every mode that shouldn't expose its spec extends this guard.
- `web/stats.js` `cardActions(g)` (stats.js:145) — same "Open spec" guard, duplicated.

### 7. Navigation & views
- `index.html` — a new `<section id="…View" class="view" hidden>` page (campaignView
  index.html:161, dailyView :178, customView :194). Each needs a topbar with a Back
  button + a body container.
- `web/app.js` `VIEWS` array (app.js:19) — **the view id must be added here** or
  `showView` won't hide/show it.
- `web/home.js` — owns the sub-view navigation for everything except play/stats/custom:
  `initHome` wires the Back buttons (home.js:82–89) and the `openX` functions call
  `showView(...)` and set the per-view back target (`campaignBack`, `dailyBack`, …).
  Navigation is split three ways: **app.js** owns the `VIEWS` registry + `goPlay`/
  `goStats`, **home.js** owns home/campaign/daily/puzzles/settings/help/privacy, and
  **custom.js** owns customView. A new mode's page lands in home.js by default but
  the registry entry is in app.js.

### 8. Home entry point
- `index.html` `homeView` — a button/section for the mode (the daily entry
  `#dailyBtn` index.html:127, above the campaign section :136).
- `web/home.js` `renderHome()` (home.js:109) — call the mode's render function
  (`renderDailyEntry()` was added here).
- `web/home.js` `initHome` — wire the entry button's click → `openX`.

### 9. Win screen
- `web/play.js` `showSolved(finalMs)` (play.js:2309) — branches on `game.daily` to
  pick the dialog. A mode with a bespoke win screen adds a branch.
- `web/play.js` `wireSolved()` (play.js:2881) — wires each dialog's buttons once.
- `index.html` — the per-variant `<dialog>` markup (`#solvedDialog` :363,
  `#dailySolvedDialog` :377).

### 10. Hint tree, specMasks & Play-from-Forced
- The hint tree is spec-aware: it classifies steps against `specMasks`
  (`baseline`/`inScope`/`forced`). Campaign games get them from the wasm
  `specMasks(kindIndex, drill)` bridge; custom/review/daily carry `g.specMasks` built
  by `masksFromUsages`. **A mode must supply `specMasks`** or the hint tree falls back
  to "everything is other".
- `web/app.js` `allowedMaskFor(req)` (app.js:93) — Play-from-Forced reads
  `req.specMasks` (custom-ish) or the wasm masks (campaign) to seed the head start.

### 11. Stats aggregation — campaign-only by construction
- `web/store.js` `statsByKind()` (store.js:408) skips any game with
  `typeof g.kindIndex !== "number"`. So **custom / review / daily have no per-mode
  stats, no best/avg times, and no solved-count badges** in the campaign tree. Daily
  tracks its own "solved today" via `store.dailyGame` (store.js:381) instead. Any mode
  wanting real stats needs a bucketing story this function doesn't provide.

### 12. Storage record
- `web/store.js` `createGame({...})` (store.js:246) — every distinguishing field
  (`spec`, `specMasks`, `forceAny`, `label`, `daily`, …) is added to both the
  destructured params and the persisted `meta` object.
- `web/store.js` record doc comment — the schema prose to keep in step.
- Selectors: a mode may need its own (`dailyGame` was added). `activeGames` /
  `solvedGames` are generic and pick up tagged games for free.

### 13. Build wiring (only for a new module file)
- `index.html` — add the module to **both** the `modulepreload` list (index.html:32–49)
  and the `data-trunk copy-file` list (:62–86). Miss the copy-file and it 404s in
  prod (this exact omission bit `tracker.js` once).
- `Trunk.toml` `[watch]` already covers `web/` recursively, so no change there.

---

## Worked example: everything the daily puzzle touched

A real "add one mode" diff, as the size of the problem:

- **New:** `web/daily.js` (the builder + seed derivation).
- **Generation:** `wasm/src/bin/gen_worker.rs` (pinned seed), `web/gen.js` (seed field).
- **Launch:** `web/app.js` (`VIEWS` entry, `launch` seed+daily plumbing).
- **Storage:** `web/store.js` (`daily` field on `createGame`, `dailyGame` selector, doc).
- **Identity ×3:** `dailyOf` in `web/home.js`, `web/stats.js`, `web/play.js`, each with
  the daily-before-review precedence.
- **Display:** `gameTitle` + `continueMeta` (home), `setTitle` (play), `historyCard`
  (stats); menu guards in `cardMenu` (home) + `cardActions` (stats).
- **Navigation/UI:** `dailyView` + home entry + `dailySolvedDialog` markup in
  `index.html`; `openDaily`/`renderDailyEntry`/`dailyLevelCard`/back wiring in home.js.
- **Win screen:** `showSolved` + `wireSolved` branches in `web/play.js`.
- **Styles:** `web/style.css`.
- **Build:** `daily.js` in the two asset lists in `index.html`.

≈ 10 files, and three of them only because identity is duplicated.

## What a refactor would centralize

The recurring shape is "given a game record, what mode is it, and how does this mode
render / behave here." Candidate consolidations:

- **One `modeOf(g)` / mode registry.** A single classifier (with the precedence baked
  in once) replacing the three `reviewOf`/`dailyOf` pairs. Each mode is a descriptor
  object: `{ id, match(g), title(g), meta(g), specMasks(req), view, homeEntry,
  winScreen, allowsRegenerate, exposesSpec, statsBucket }`.
- **Display via the descriptor**, so `gameTitle`/`continueMeta`/`setTitle`/`historyCard`
  call `mode.title(g)` / `mode.meta(g)` instead of re-laddering.
- **A persisted mode discriminator** on the record (e.g. `kind: "daily"`) so identity
  is a field read, not a spec re-match — removing the Expert-I-vs-Lesson-1 collision
  class entirely.
- **A view/registry seam** so a mode declares its page + home entry once, instead of
  the app.js `VIEWS` array, home.js nav, and index.html markup drifting apart.
- **A stats bucketing story** that isn't `kindIndex`-only, if non-campaign modes are to
  have times/badges.

The campaign path being privileged (everything keyed on `kindIndex`: stats, badges,
the wasm `specMasks`, the hint tree) while every other mode is a re-sniffed custom
game is the root; flattening that distinction is the core of the refactor.
