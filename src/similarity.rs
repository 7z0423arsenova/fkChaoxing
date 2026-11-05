//! 通过编辑距离比较两个答案文本。

/// 返回 [0, 1] 范围内的相似度；完全相同为 1。
pub fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(left != right));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    1.0 - previous[b.len()] as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::ratio;

    #[test]
    fn similarity_uses_edit_distance() {
        assert_eq!(ratio("abcd", "bcde"), 0.5);
        assert_eq!(ratio("", ""), 1.0);
        assert_eq!(ratio("abc", ""), 0.0);
        assert!((ratio("上海", "上海市") - 2.0 / 3.0).abs() < 1e-12);
    }
}
