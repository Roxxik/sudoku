//! The seed -> puzzle harvest fixture format, shared by the writers (the `harvest` and
//! `scan` examples) and the reader (the `harvest_reconstructs` test), so all three agree on
//! one definition.
//!
//! A fixture file is a sequence of blank-line-separated [`Record`]s; `#` lines and blank
//! lines are ignored. Each record is one spec plus the seed -> puzzle pairs it was harvested
//! with:
//!
//! ```text
//! force xy-wing            # one or more; NAME[:COUNT]
//! toolbox train            # train | drill | full
//! window 100000 2000       # OPTIONAL: seeds [base, base+span) were ALL tried
//! hit 100003 ....5.14...    # a yielder inside the window (only with `window`)
//! sample 146012 ...6...     # a known yielder, positive-check only (any seed)
//! ```
//!
//! `window` present => `hit`s are the COMPLETE set of yielders in `[base, base+span)`, so the
//! test can assert an exact bijection (yields <=> recorded). `window` absent => the record is
//! positive-only: the test just reconstructs each `sample`, which is cheap (one attempt per
//! seed) and scales to the hundreds of records the rarity scan emits.

use std::fmt::Write as _;

use crate::cli::{Toolbox, build_spec, spec_label};
use crate::spec::Spec;
use crate::spec::kinds::NAMES;

/// One spec's harvested seed -> puzzle mapping. See the module docs for the on-disk shape.
pub struct Record {
    /// The forced kinds with counts, in force order (`build_spec` input).
    pub forces: Vec<(usize, u16)>,
    /// The toolbox surrounding the forced kinds.
    pub toolbox: Toolbox,
    /// `Some((base, span))`: the seeds `[base, base+span)` were all tried and `hits` is the
    /// complete set of yielders in them (an exact bijection). `None`: positive-only.
    pub window: Option<(u64, u64)>,
    /// Yielders inside the window (the negative-control half, plus dense positives).
    pub hits: Vec<(u64, String)>,
    /// Extra known yielders (positive checks only); the bulk of the rarity-scan corpus.
    pub samples: Vec<(u64, String)>,
}

impl Record {
    /// The [`Spec`] this record pins.
    pub fn spec(&self) -> Spec {
        build_spec(&self.forces, self.toolbox)
    }

    /// Human-readable label, e.g. `swordfish + naked-triple`.
    pub fn label(&self) -> String {
        spec_label(&self.forces)
    }

    /// Canonical `NAME[:COUNT]` token per force (the form [`parse_block`] reads back).
    pub fn force_tokens(&self) -> Vec<String> {
        self.forces
            .iter()
            .map(|&(i, c)| if c == 1 { NAMES[i].to_string() } else { format!("{}:{c}", NAMES[i]) })
            .collect()
    }

    /// Whether this record reconstructs at least one puzzle. A record with neither a `hit`
    /// nor a `sample` only ever exercises the negative half, so a generator that silently
    /// stopped yielding would still pass it -- the test rejects such records.
    pub fn has_positive(&self) -> bool {
        !self.hits.is_empty() || !self.samples.is_empty()
    }

    /// Serialize this record's block (no trailing blank line; the caller separates records).
    pub fn write_block(&self, buf: &mut String) {
        for t in self.force_tokens() {
            let _ = writeln!(buf, "force {t}");
        }
        let _ = writeln!(buf, "toolbox {}", self.toolbox.label_short());
        if let Some((base, span)) = self.window {
            let _ = writeln!(buf, "window {base} {span}");
        }
        for (seed, line) in &self.hits {
            let _ = writeln!(buf, "hit {seed} {line}");
        }
        for (seed, line) in &self.samples {
            let _ = writeln!(buf, "sample {seed} {line}");
        }
    }
}

/// Parse a whole file into its records, skipping `#`/blank lines and splitting records on
/// blank lines. Returns a per-record error string (with no location) on a malformed line; the
/// caller adds the filename.
pub fn parse_records(text: &str) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            if let Some(r) = parse_block(&cur)? {
                records.push(r);
            }
            cur.clear();
        } else {
            cur.push(line);
        }
    }
    if let Some(r) = parse_block(&cur)? {
        records.push(r);
    }
    Ok(records)
}

/// Parse one record's worth of content lines (already `#`/blank-stripped). `Ok(None)` if the
/// block has no `force` line (e.g. a trailing comment-only chunk).
fn parse_block(lines: &[&str]) -> Result<Option<Record>, String> {
    use crate::cli::parse_force;

    let mut forces = Vec::new();
    let mut toolbox = Toolbox::Train;
    let mut window = None;
    let mut hits = Vec::new();
    let mut samples = Vec::new();

    for &line in lines {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap(); // non-empty by construction
        match key {
            "force" => {
                let tok = it.next().ok_or("'force' needs a NAME[:COUNT]")?;
                forces.push(parse_force(tok)?);
            }
            "toolbox" => toolbox = Toolbox::parse(it.next()),
            "window" => {
                let base = it.next().and_then(|s| s.parse().ok()).ok_or("bad 'window' base")?;
                let span = it.next().and_then(|s| s.parse().ok()).ok_or("bad 'window' span")?;
                window = Some((base, span));
            }
            "hit" | "sample" => {
                let seed = it.next().and_then(|s| s.parse().ok()).ok_or("bad seed")?;
                let puzzle = it.next().ok_or("missing puzzle")?.to_string();
                if key == "hit" { &mut hits } else { &mut samples }.push((seed, puzzle));
            }
            other => return Err(format!("unknown directive {other:?}")),
        }
    }

    if forces.is_empty() {
        return Ok(None);
    }
    Ok(Some(Record { forces, toolbox, window, hits, samples }))
}
