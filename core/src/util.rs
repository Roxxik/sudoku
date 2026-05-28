/// Iterate every k-combination of `items` and call `f` on it.
/// `f` returns `true` to continue, `false` to stop iteration entirely.
/// Returns `true` if iteration ran to completion, `false` if `f` stopped it.
///
/// Hot path: technique searches call this millions of times. We dispatch
/// on the (small, known) k to specialized iterative loops because the
/// previous recursive `rec` showed up at ~28% of self-time in perf and
/// the compiler doesn't unroll recursion across closure types.
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
        1 => {
            for i in 0..n {
                if !f(&[items[i]]) {
                    return false;
                }
            }
        }
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
        _ => {
            // Generic fallback for k > 4. No technique uses it today
            // (largest is Naked/Hidden Quad and Jellyfish at k=4) but
            // we keep this so callers aren't silently wrong if someone
            // adds a Quintuple-something later.
            let mut idx: [usize; 16] = [0; 16];
            for i in 0..k {
                idx[i] = i;
            }
            let mut buf: [T; 16] = [items[0]; 16];
            loop {
                for i in 0..k {
                    buf[i] = items[idx[i]];
                }
                if !f(&buf[..k]) {
                    return false;
                }
                // Advance to next combination (lexicographic).
                let mut i = k;
                while i > 0 {
                    i -= 1;
                    if idx[i] < n - k + i {
                        idx[i] += 1;
                        for j in i + 1..k {
                            idx[j] = idx[j - 1] + 1;
                        }
                        break;
                    }
                    if i == 0 {
                        return true;
                    }
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_all_pairs() {
        let mut count = 0;
        for_each_combination(&[1u8, 2, 3, 4], 2, |_| {
            count += 1;
            true
        });
        assert_eq!(count, 6);
    }

    #[test]
    fn empty_when_k_exceeds_len() {
        let mut count = 0;
        for_each_combination(&[1u8, 2], 3, |_| {
            count += 1;
            true
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn returns_in_lexicographic_order() {
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for_each_combination(&[1u8, 2, 3], 2, |c| {
            seen.push(c.to_vec());
            true
        });
        assert_eq!(seen, vec![vec![1, 2], vec![1, 3], vec![2, 3]]);
    }

    #[test]
    fn stops_when_callback_returns_false() {
        let mut count = 0;
        let ran_to_end = for_each_combination(&[1u8, 2, 3, 4], 2, |_| {
            count += 1;
            count < 3
        });
        assert!(!ran_to_end);
        assert_eq!(count, 3);
    }
}
