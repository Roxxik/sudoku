# Sudoku Generator

Generates Sudoku puzzles that **force** a specific solving technique, on demand
(target: ~1s per puzzle).

## Design principles

**The verifier is the only definition of correct.**
A puzzle satisfies a spec iff `verify` accepts it — every puzzle, every spec, no
exceptions. Necessity is judged by the verifier's avoid-target walk over the
in-scope toolbox (Allowed + Forced + Conceded/tolerated), *not* by the canonical
easiest-first solver. The canonical solver picks the easiest applicable
technique, which says nothing about whether a technique is needed — and a
tolerated technique can stand in for a target.

**Correctness is universal; speed is selective.**
Correctness holds for any spec (all go through the same verifier). The generator
may carry spec-specific fast paths to make the *benchmarked* specs fast — the
tiers, curriculum, and families swept by `core/examples/bench_gen.rs`. That
bench set defines what must be fast enough. Custom user specs stay correct but
carry no speed guarantee.

**Maximize diversity within the spec.**
Real generation, not morphing. Isomorphic variants of a single seed teach a
shortcut that doesn't generalize; the player should learn to spot the technique
in genuinely varied positions. So constrain *minimally*: hard-constrain only the
technique's essence and randomize everything else. A hard constraint that isn't
essential to the spec is a diversity bug.

**The hard part is necessity, not appearance.**
For most techniques the pattern appears readily in a random puzzle; making it
*required* is the rare thing. The metric that matters is whether the puzzle can
be solved without the target — how long the target is merely *available* is
irrelevant if an easier path solves the puzzle. Forcing means driving the
available non-target deductions to zero, so the target is the only way forward.

**Measure, don't guess.**
Design decisions are driven by the bench and the `--probe-*` diagnostics, not
hunches. The bench is the source of truth for "fast enough."
