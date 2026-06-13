//! Off-by-default instrumentation counters (`feature = "count"`).
//!
//! Every diagnostic in the crate — prober anatomy ([`crate::probe`] `PCTR`),
//! band-scan metrics (`BAND_CTR`), and the packed-DFS metrics
//! ([`crate::probe::simt`] `PSTAT`/`DSTAT`) — is the same shape: a fixed `[u64; N]` of
//! tallies that an example snapshots and resets around a measured run. The
//! [`counter_block`] macro generates one such block (the backing static, the
//! `snapshot`/`reset` readout, and the `inc`/`add` mutators), so each is one line
//! instead of fifteen and the single-threaded `static mut` + its `unsafe` live in
//! exactly one place.
//!
//! With the feature off the mutators are no-ops and the static/readouts are
//! absent (callers gate their `snapshot`/`reset` calls behind the feature too),
//! so a production build pays nothing. The macro expands *inside the invoking
//! module*, so the generated `snapshot`/`reset` keep their `crate::probe::…` /
//! `crate::probe::simt::…` paths that the example diagnostics import.

/// Declare a counter block named `$static` of `$len` `u64` tallies. Generates, in
/// the invoking module: `pub(crate)` `inc(i)` / `add(i, v)` mutators (no-ops when the
/// gate feature is off) and, only under the gate, the backing `static mut [u64; $len]`
/// plus `pub snapshot() -> [u64; $len]` and `pub reset()`.
///
/// The gate defaults to `count`; the `feature = "<name>", …` form gates on any feature
/// instead (e.g. the heavyweight `kernel_count`), so a block whose mutators sit in the
/// hottest loop can be opted into separately from the ordinary `count` diagnostics.
///
/// Single-threaded lab, so the static is a plain `static mut` read/written by
/// value — no atomics, no aliasing.
macro_rules! counter_block {
    // Default gate: the off-by-default `count` feature.
    ($static:ident: $len:expr, inc = $inc:ident, add = $add:ident, snapshot = $snap:ident, reset = $reset:ident) => {
        counter_block!(feature = "count", $static: $len, inc = $inc, add = $add, snapshot = $snap, reset = $reset);
    };
    // Explicit gate: any feature name.
    (feature = $feat:literal, $static:ident: $len:expr, inc = $inc:ident, add = $add:ident, snapshot = $snap:ident, reset = $reset:ident) => {
        #[cfg(feature = $feat)]
        static mut $static: [u64; $len] = [0; $len];

        #[cfg(feature = $feat)]
        #[inline(always)]
        #[allow(dead_code)]
        pub(crate) fn $inc(i: usize) {
            // SAFETY: single-threaded lab; by-value array, no aliasing.
            unsafe {
                $static[i] += 1;
            }
        }
        #[cfg(feature = $feat)]
        #[inline(always)]
        #[allow(dead_code)]
        pub(crate) fn $add(i: usize, v: u64) {
            // SAFETY: as above.
            unsafe {
                $static[i] += v;
            }
        }
        #[cfg(feature = $feat)]
        pub fn $snap() -> [u64; $len] {
            // SAFETY: as above.
            unsafe { $static }
        }
        #[cfg(feature = $feat)]
        pub fn $reset() {
            // SAFETY: as above.
            unsafe {
                $static = [0; $len];
            }
        }

        #[cfg(not(feature = $feat))]
        #[inline(always)]
        #[allow(dead_code)]
        pub(crate) fn $inc(_i: usize) {}
        #[cfg(not(feature = $feat))]
        #[inline(always)]
        #[allow(dead_code)]
        pub(crate) fn $add(_i: usize, _v: u64) {}
    };
}

pub(crate) use counter_block;
