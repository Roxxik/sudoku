//! Shared command-line spec construction for the `find` / `findpar` / `findpar-bench`
//! examples: the `--force NAME[:COUNT]` + `--toolbox train|drill|full` interface (a
//! strict superset of the old single-`--target`/`--mode` interface — `--target X --mode
//! train` is just `--force X --toolbox train`), plus the [`FindArgs`] parser that `find`
//! and `findpar` share so the two tools take identical arguments.
//!
//! Living in the library (rather than duplicated per example) keeps the spec semantics in
//! one tested place; the union-reduction contract (`train_union(&[t]) == Spec::train(t)`)
//! means a single `--force` behaves exactly like the old single-target spec.

use crate::spec::Spec;
use crate::spec::kinds::{NAMES, NUM};
use crate::{drill_union, train_union};

/// Which toolbox surrounds the forced kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Toolbox {
    /// Union of each target's train-scope (Trunk + simpler-or-equal same-branch peers).
    Train,
    /// Union of each target's drill-scope: Trunk allowed, simpler same-branch peers
    /// CONCEDED (avoid-walk only) -- so each forced kind fires in the baseline trace.
    Drill,
    /// The entire 16-kind ladder allowed (more substitutes => rarer).
    Full,
}

impl Toolbox {
    /// Parse a `--toolbox` value. Unknown / missing falls back to [`Train`](Self::Train),
    /// the default.
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("full") => Toolbox::Full,
            Some("drill") => Toolbox::Drill,
            _ => Toolbox::Train,
        }
    }

    /// The toolbox name as printed in bench headers.
    pub fn label(self) -> &'static str {
        match self {
            Toolbox::Train => "train-union",
            Toolbox::Drill => "drill-union",
            Toolbox::Full => "full",
        }
    }

    /// The short `--toolbox` token, the exact inverse of [`parse`](Self::parse): round-trips
    /// through the CLI (unlike [`label`](Self::label), whose `*-union` suffix `parse` would
    /// silently read back as `Train`). Used when a tool re-emits a spec it can re-ingest.
    pub fn label_short(self) -> &'static str {
        match self {
            Toolbox::Train => "train",
            Toolbox::Drill => "drill",
            Toolbox::Full => "full",
        }
    }
}

/// Parse one `NAME[:COUNT]` force token into a `(kind index, count)` pair. `COUNT`
/// defaults to 1. On an unknown kind the `Err` is a ready-to-print message that lists the
/// known names.
pub fn parse_force(arg: &str) -> Result<(usize, u16), String> {
    let (name, count) = match arg.split_once(':') {
        Some((n, c)) => (n, c.parse().unwrap_or(1u16)),
        None => (arg, 1u16),
    };
    NAMES
        .iter()
        .position(|&n| n == name)
        .map(|idx| (idx, count))
        .ok_or_else(|| format!("unknown kind {name:?}; known: {}", NAMES.join(", ")))
}

/// Build the toolbox spec around the forced kinds. train/drill use the library union
/// builders (the multi-force generalizations of [`Spec::train`]/[`Spec::drill`]); full
/// allows the entire ladder. The union builders force at count 1, so re-force afterwards
/// to honor any requested COUNT > 1 (force overrides the slot).
pub fn build_spec(forces: &[(usize, u16)], toolbox: Toolbox) -> Spec {
    let idxs: Vec<usize> = forces.iter().map(|&(i, _)| i).collect();
    let mut spec = match toolbox {
        Toolbox::Train => train_union(&idxs),
        Toolbox::Drill => drill_union(&idxs),
        Toolbox::Full => {
            let mut s = Spec::explicit();
            for idx in 0..NUM {
                s = s.allow(idx);
            }
            s
        }
    };
    for &(idx, count) in forces {
        spec = spec.force(idx, count);
    }
    spec
}

/// Human-readable label for a force set, e.g. `w-wing + naked-triplex2`.
pub fn spec_label(forces: &[(usize, u16)]) -> String {
    forces
        .iter()
        .map(|&(idx, c)| if c == 1 { NAMES[idx].to_string() } else { format!("{}x{c}", NAMES[idx]) })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// The shared `--force`/`--toolbox`/`--seed`/`--count`/`--max` argument set for the `find`
/// (scalar) and `findpar` (SIMT) examples, so the two tools take identical arguments.
/// One seed = one attempt: both consume seeds from `base_seed` upward, a single strip walk
/// each, until `count` puzzles have been produced (most seeds yield nothing, so far more
/// than `count` seeds are tried — and which seeds yield is a pure function of the seed,
/// identical across the scalar and SIMT paths). `max` caps the total attempts (= seeds
/// tried) so a low- or zero-yield spec can't run forever; it defaults to unlimited.
pub struct FindArgs {
    /// The forced kinds with their counts, in `--force` order.
    pub forces: Vec<(usize, u16)>,
    /// The toolbox surrounding the forced kinds.
    pub toolbox: Toolbox,
    /// First seed tried; seeds are consumed `base_seed, base_seed + 1, ...`.
    pub base_seed: u64,
    /// How many puzzles to produce (seeds are consumed until this many succeed).
    pub count: u64,
    /// Safety cap on total attempts (= seeds tried) before giving up; default unlimited.
    pub max: usize,
}

impl FindArgs {
    /// Parse from `std::env::args()`. Exits with code 2 on an unknown kind or when no
    /// `--force` is given (matching `findpar-bench`'s require-a-force contract).
    pub fn from_env() -> Self {
        let mut forces: Vec<(usize, u16)> = Vec::new();
        let mut toolbox = Toolbox::Train;
        let mut base_seed = 1u64;
        let mut count = 1u64;
        let mut max = usize::MAX; // unlimited by default; --max sets a finite attempt cap
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--force" => match parse_force(&it.next().unwrap_or_default()) {
                    Ok(f) => forces.push(f),
                    Err(msg) => {
                        eprintln!("{msg}");
                        std::process::exit(2);
                    }
                },
                "--toolbox" => toolbox = Toolbox::parse(it.next().as_deref()),
                "--seed" => base_seed = it.next().and_then(|s| s.parse().ok()).unwrap_or(base_seed),
                "--count" => count = it.next().and_then(|s| s.parse().ok()).unwrap_or(count),
                "--max" => max = it.next().and_then(|s| s.parse().ok()).unwrap_or(max),
                _ => {}
            }
        }
        if forces.is_empty() {
            eprintln!("need at least one --force NAME[:COUNT]");
            std::process::exit(2);
        }
        FindArgs { forces, toolbox, base_seed, count, max }
    }

    /// The spec these arguments describe.
    pub fn spec(&self) -> Spec {
        build_spec(&self.forces, self.toolbox)
    }

    /// The `--force` set label, e.g. `w-wing + naked-triple`.
    pub fn label(&self) -> String {
        spec_label(&self.forces)
    }
}
