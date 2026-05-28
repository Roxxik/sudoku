pub fn for_each_combination<T: Copy, F: FnMut(&[T])>(items: &[T], k: usize, mut f: F) {
    if k == 0 || items.len() < k {
        return;
    }
    let mut current: Vec<T> = Vec::with_capacity(k);
    rec(items, k, 0, &mut current, &mut f);
}

fn rec<T: Copy, F: FnMut(&[T])>(
    items: &[T],
    k: usize,
    start: usize,
    current: &mut Vec<T>,
    f: &mut F,
) {
    if current.len() == k {
        f(current);
        return;
    }
    let needed = k - current.len();
    if items.len() < start + needed {
        return;
    }
    let max_start = items.len() - needed;
    for i in start..=max_start {
        current.push(items[i]);
        rec(items, k, i + 1, current, f);
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_all_pairs() {
        let mut count = 0;
        for_each_combination(&[1u8, 2, 3, 4], 2, |_| count += 1);
        assert_eq!(count, 6);
    }

    #[test]
    fn empty_when_k_exceeds_len() {
        let mut count = 0;
        for_each_combination(&[1u8, 2], 3, |_| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn returns_in_lexicographic_order() {
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for_each_combination(&[1u8, 2, 3], 2, |c| seen.push(c.to_vec()));
        assert_eq!(seen, vec![vec![1, 2], vec![1, 3], vec![2, 3]]);
    }
}
