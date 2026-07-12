# Subset combo membership — `contains` scan vs mask test (and why not Gosper)

A small, unapplied cleanup note for the scalar subset techniques, recorded so it is not
re-derived. Seeded by the question *"do we use Gosper's hack for the k-subset iteration,
and should we?"* Answer to both: no — but the digging surfaced one real asymmetry between
the naked and hidden paths worth writing down.

## Where the subset iteration lives

`src/solve/combinations.rs` — `for_each_combination(items, k, f)`. It is **not** Gosper's
hack: it is three explicit nested loops specialised per `k`, over a compact index array,
with `k` outside `2..=4` an `unreachable!`. Callers first gather the eligible slots of a
unit into that array (`cand[..n]` cells / `digits[..n]` digits, `n <= 9`), then enumerate
size-`k` combos of them. Two call sites, both in `src/solve/techniques.rs`:

- `naked_unit` (~line 200): `cand[i]` = a unit cell slot (0..9) with `2..=size`
  candidates; each combo is `size` **cells**.
- `hidden_unit` (~line 691): `digits[i]` = a digit (0..9) with `2..=size` candidate
  cells; each combo is `size` **digits**.

## The asymmetry (the "smell")

Both paths, once a combo is confirmed a real subset (`union` popcount `== size`), must
eliminate against everything **outside** the combo — so both need a per-element "is this
in the combo?" test. They answer it differently:

- **Hidden path** builds a bitmask and tests with one AND:
  `let combo_mask: u16 = combo.iter().fold(0, |a, &di| a | (1 << di));`
  then walks `elig = present & !combo_mask` (`techniques.rs:733-734`). Membership is
  `O(1)` per candidate, and the non-combo set falls straight out of the mask complement.

- **Naked path** tests with `combo.contains(&i)` inside a `for i in 0..9`
  (`techniques.rs:224`). That is an `O(size)` linear scan of the combo slice **per cell**,
  i.e. up to `9 * size` element compares per confirmed subset, to reconstruct information
  (the non-combo cells) that a `combo_mask` would give for free — exactly what the hidden
  path already computes one function down.

It is a micro-cost (the branch only runs on a *confirmed* subset, which is rare relative
to combos tried, and `size <= 4`), so this is a readability/consistency wart more than a
measured hotspot. But it is a pure asymmetry: the two twins solving the same shape of
problem, one already carrying the tidier form.

### The shape of the fix (for whoever picks it up)

Mirror the hidden path in `naked_unit`: cell slots are `0..9`, so

```rust
let combo_mask: u16 = combo.iter().fold(0, |a, &k| a | (1 << k));
// ...
if (combo_mask >> i) & 1 != 0 { continue; } // was: combo.contains(&i)
```

Same `(cell, digit)` elimination set, same fixpoint. Local to one closure, no signature
or `for_each_combination` change. **Not applied here — doc only.** Whoever applies it must
build and run the solver equivalence checks (the fingerprint pins in the solve tests);
this note deliberately does not.

## Why Gosper's hack is the wrong tool here

Gosper's hack steps a bitmask to the next value with the same popcount — the branch-free
way to enumerate fixed-`k` submasks of a large space. This is not that space:

1. **The space is tiny and combo-bounded.** `n <= 9`, usually 2-5; worst case `C(9,4)=126`
   combos, typically a handful. The specialised nested loops are already optimal and
   perfectly branch-predictable; there is no asymptotic term for Gosper to improve. The
   module doc calls the specialisation deliberate.
2. **The gather is already done, so Gosper saves nothing on it.** Candidates are compacted
   into `cand[..n]` / `digits[..n]` up front. A Gosper mask bit `i` still indexes back
   through that array to reach the real cell/digit — you keep the array either way, and the
   per-step Gosper arithmetic plus a bit-extract to recover combo members is net *more*
   work than reading three loop indices.
3. **Restricting submasks to the sparse eligible slots needs PDEP/BMI2** (or a lookup
   table) — the eligible cells are a sparse subset of the 9, not a contiguous `0..n`. That
   is portability cost (wasm/mobile is the shipped scalar target) for no measured gain.

So: Gosper is not used, and adopting it would make the hot path slightly worse for a tiny,
predictable enumeration. The only thing worth carrying forward from the question is the
`combo_mask` membership tidy-up above — which is a plain masking cleanup, not Gosper.

## Status

- Naked/hidden asymmetry: **documented, not fixed.**
- Gosper's hack: **evaluated, NO-GO**, reasons above so it is not re-litigated.
