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

# Print one actual puzzle (feed to core's CLI/verifier to confirm the spec):
cargo run --release -p generator-lab --example find -- --mode train --seed 1
cargo run --release -p generator-lab --example find -- --mode drill --seed 1

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
- `src/prober.rs` — the `bitboard-simd` existence prober (uniqueness gate).
- `src/verify.rs` — spec verification reduced to a bool.
- `src/generator.rs` — the random strip-generate pipeline.
- `examples/bench.rs` `examples/find.rs` — native bench / single-puzzle.
- `web/` — node + SpiderMonkey runners, two-button browser page, LAN server.
