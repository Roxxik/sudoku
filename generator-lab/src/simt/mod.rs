//! Native-only SIMT generation stack: the packed existence prober and the
//! attempt host that drives it. Both behind `#[cfg(not(target_arch = "wasm32"))]`
//! (declared in [`crate`]'s root) — this is an AVX (W=8) native play; the wasm
//! cdylib ships the scalar [`crate::bb::ProberBoard`] prober instead.
//!
//! - [`prober`]: the packed SoA existence prober — a *warp* of W=8 per-lane DFS
//!   searches in lockstep (the gather-free smear+ALU kernel, [`prober::warp_pass`]),
//!   refilled on demand. The lockstep SIMD lanes are the "warp".
//! - [`host`]: the attempt driver — K independent strip attempts made resumable,
//!   each pausing at its uniqueness gate so [`prober`] can batch eight gates at a
//!   time, plus [`host::find_puzzles`], the per-core throughput harvester.

pub mod host;
pub mod prober;
