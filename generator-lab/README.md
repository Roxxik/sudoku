# generator-lab

Temporary, clean-slate testing grounds for the **performance-critical puzzle
generation path** — built only to benchmark and tune, on native and mobile
(wasm), before deciding what to change in `core`.

## Why it exists

`core`'s structures pull dual duty: they generate hints during play (not perf
critical) and generate new puzzles (very perf critical). Mixing the two cost
profiles makes the hot path impossible to tune in isolation, and `core`'s
generator is further weighed down by paths that turned out not to help (local
search, construction). This crate is the hot path **alone**, on a clean slate:

- **Random method only** — no local search, no construction (which also biases
  the puzzle distribution away from random generation).
- **No play-time baggage** — no `Step`/`Deduction`/`focus_cells`, no hint
  reporting. Techniques mutate the board and report only which *kind* fired,
  which is all the spec gates need.
- **Self-contained** — copies the minimal `grid`/`rng`/`util` primitives from
  core (like solver-lab) so the hot path benches in isolation. `core` is a
  **dev-dependency only**, for the faithfulness cross-check.
- **PoC scope** — the technique ladder up to **HiddenQuad** (rare, high on the
  ladder → fewest techniques to check, still hard to generate). Not for backport
  yet; first make it fast, then decide what core can repurpose.

## What it generates

Spec-driven `train`/`drill` for HiddenQuad, faithful to core's `Spec`:

- **train(HiddenQuad)** = allow everything up to HiddenQuad, force HiddenQuad ≥1
  in the baseline trace.
- **drill(HiddenQuad)** = baseline is singles only; LC + subsets are *conceded*
  (granted to the verify avoid-walk but not the solvability baseline); HiddenQuad
  must be forced even against that whole in-between toolbox.

Per attempt: random full grid → strip cells in random order, keeping a strip iff
the puzzle stays unique (`bitboard-simd` prober) AND baseline-solvable (spec
toolbox); remember the most-stripped state whose trace meets the requirement; if
one exists and passes `verify` (irreplaceability), the attempt succeeds. This is
core's exact gate sequence with the bookkeeping reduced to bools + counts.

Faithfulness: core's verifier accepts generated puzzles, and the per-attempt
requirement/verify split + attempts-per-puzzle match core's `bench_gen`.

## Running

```
# Native bench (train + drill, fixed attempts) + wasm benches under any installed
# engines (node/V8, js140/SpiderMonkey, wasmtime):
generator-lab/check.sh [attempts=2000] [seed=1]

# Native only:
cargo run --release -p generator-lab --example bench -- --attempts 4000 --seed 1

# Print one actual puzzle (scalar, single seed; feed to core's CLI/verifier):
cargo run --release -p generator-lab --example find -- --mode train --seed 1
cargo run --release -p generator-lab --example find -- --mode drill --seed 1

# Harvest N puzzles via the packed SIMT prober (races W=8 seed streams in parallel
# until N are found); puzzle lines on stdout, summary on stderr:
cargo run --release -p generator-lab --example findpar -- --mode train --count 10

# Real-device ARM numbers (desktop wasm is a POOR proxy): serve the page over the
# LAN, open on a phone, tap Run train / Run drill — results POST back to the
# terminal and append to web/results.jsonl.
generator-lab/web/serve.sh [port=8000]
```

## Layout

- `src/grid.rs` `src/rng.rs` `src/util.rs` — primitives copied from core.
- `src/techniques.rs` — gateable up-to-HiddenQuad engine: `solve_tracked`
  (baseline gate + requirement counts) and `min_target_uses` (verify avoid-walk).
- `src/spec.rs` — compact `train`/`drill` spec.
- `src/bb.rs` — the dual-banded bitboard core: the baseline technique engine, and
  the lean single-layout no-LC `ProberBoard` existence prober (the uniqueness gate
  + the shipped/wasm scalar path + the packed prober's correctness oracle).
- `src/packed.rs` / `src/warp.rs` (native only) — the **packed/SIMT prober**: a warp
  of W=8 per-lane DFS searches (gather-free smear+ALU kernel) with streaming refill,
  and the host driver that batches the strip loop's uniqueness gates onto it. ~2.75x
  per-core prober / ~1.55x end-to-end on native AVX. See `SIMT-ROADMAP.md`.
- `src/verify.rs` — spec verification reduced to a bool.
- `src/generator.rs` — the random strip-generate pipeline (scalar, per-lane reference).
- `examples/bench.rs` `examples/find.rs` — native bench / scalar single-puzzle.
- `examples/findpar.rs` — harvest N puzzles via the packed SIMT prober (`warp::find_puzzles`, W=8 seed streams in parallel).
- `examples/probebench.rs` `examples/packbench.rs` — packed prober speedup (isolated
  / end-to-end) vs the scalar prober. The rest of `examples/` are the SIMT design
  microbenches indexed in `SIMT-ROADMAP.md`.
- `tests/equiv_warp.rs` — pins each warp lane byte-identical to the sequential run.
- `web/` — node + SpiderMonkey runners, two-button browser page, LAN server.

## SIMT prober

The uniqueness gate is the generator's hot path. `src/packed.rs` + `src/warp.rs`
pack W=8 independent existence searches per SIMD register on native AVX, fed by a
streaming refill driver. `SIMT-ROADMAP.md` is the full design (cost model, layout,
kernel, the measurement suite that validated it). The scalar `ProberBoard` is kept
deliberately — it is the wasm/mobile path (SIMT is a native-only win), the packed
prober's correctness oracle, and the perf bar.
