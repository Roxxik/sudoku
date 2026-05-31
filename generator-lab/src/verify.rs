//! Spec verification — core's `verifier::verify`, reduced to a bool (the
//! generator only needs accept/reject, not the violation list).
//!
//! Two checks:
//!   1. **Positive**: the baseline toolbox (Allowed | Forced) alone solves the
//!      puzzle.
//!   2. **Forcing**: each Forced technique is *irreplaceable* — even granted the
//!      whole in-scope toolbox minus that technique (Allowed + Conceded), the
//!      avoid-target walk is still forced to use it at least `count` times.

use crate::grid::Board;
use crate::spec::Spec;
use crate::techniques::{min_target_uses, solve_tracked};

/// True iff `board` satisfies `spec`: baseline-solvable AND every Forced
/// technique is forced (`min_target_uses >= required`).
#[cfg_attr(feature = "profiling", inline(never))]
pub fn verify(board: &Board, spec: &Spec) -> bool {
    // Positive: baseline alone must solve.
    if !solve_tracked(board, spec.baseline_mask()).solved {
        return false;
    }
    // Forcing: each Forced technique must beat the rest of the in-scope toolbox.
    let scope = spec.in_scope_mask();
    for (idx, need) in spec.forced() {
        if min_target_uses(board, scope, 1 << idx) < need as usize {
            return false;
        }
    }
    true
}
