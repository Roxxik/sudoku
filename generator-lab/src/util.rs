//! k-combination iterator, copied from `core::util` (specialized loops for the
//! small known k — subset techniques call this on the hot path).

pub fn for_each_combination<T: Copy, F: FnMut(&[T]) -> bool>(
    items: &[T],
    k: usize,
    mut f: F,
) -> bool {
    let n = items.len();
    if k == 0 || n < k {
        return true;
    }
    match k {
        2 => {
            for i in 0..n {
                for j in i + 1..n {
                    if !f(&[items[i], items[j]]) {
                        return false;
                    }
                }
            }
        }
        3 => {
            for i in 0..n {
                for j in i + 1..n {
                    for l in j + 1..n {
                        if !f(&[items[i], items[j], items[l]]) {
                            return false;
                        }
                    }
                }
            }
        }
        4 => {
            for i in 0..n {
                for j in i + 1..n {
                    for l in j + 1..n {
                        for m in l + 1..n {
                            if !f(&[items[i], items[j], items[l], items[m]]) {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        _ => unreachable!("solver-lab only uses k in 2..=4 (subset techniques)"),
    }
    true
}
