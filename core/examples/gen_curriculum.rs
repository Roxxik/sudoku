//! Emit the player-facing curriculum taxonomy as JSON to stdout.
//!
//! This is the build-time twin of the wasm `curriculum()` export: both read the
//! same `lab::kinds` source of truth, so the static `web/curriculum.json` the web
//! frontend renders its campaign tree from can't drift from the Rust taxonomy.
//! Wiring lives in `web/Trunk.toml` (a `pre_build` hook redirects this into
//! `web/curriculum.json`); rendering it instead of waiting for wasm init is what
//! makes the start page appear immediately.
//!
//! Shape mirrors `sudoku_wasm`'s `TechniqueEntry`:
//!   [{ "kindIndex", "id", "difficulty", "tier", "branch", "hasDrill" }, ...]

use sudoku_core::lab::kinds::{self, Branch, Tier};

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::Beginner => "beginner",
        Tier::Intermediate => "intermediate",
        Tier::Expert => "expert",
        Tier::Master => "master",
    }
}

fn branch_str(b: Branch) -> &'static str {
    match b {
        Branch::Trunk => "trunk",
        Branch::Fish => "fish",
        Branch::Subset => "subset",
        Branch::Bivalue => "bivalue",
    }
}

fn main() {
    // The kind ids are simple kebab-case (no JSON-special characters), so a
    // hand-rolled emitter avoids pulling serde into core for one tiny array.
    let mut out = String::from("[");
    for i in 0..kinds::NUM {
        if i > 0 {
            out.push(',');
        }
        let tier = kinds::tier_of(i);
        out.push_str(&format!(
            "{{\"kindIndex\":{},\"id\":\"{}\",\"difficulty\":{},\"tier\":\"{}\",\"branch\":\"{}\",\"hasDrill\":{}}}",
            i,
            kinds::NAMES[i],
            kinds::DIFFICULTY[i],
            tier_str(tier),
            branch_str(kinds::branch_of(i)),
            // Beginner is train-only; every other tier offers a drill.
            tier != Tier::Beginner,
        ));
    }
    out.push(']');
    println!("{out}");
}
