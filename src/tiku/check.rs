//! 答案格式校验。

use super::TikuSettings;
use crate::models::QuestionType;

pub const BUILTIN_TRUE: &[&str] = &["true", "t", "1", "对", "正确", "√", "是", "yes", "y"];
pub const BUILTIN_FALSE: &[&str] = &[
    "false",
    "f",
    "0",
    "错",
    "错误",
    "×",
    "否",
    "no",
    "n",
    "不对",
    "不正确",
];

/// 按常见分隔符切分答案，返回第一个能切出结果的分隔方式；全为空时返回 `None`。
pub fn cut(answer: &str) -> Option<Vec<String>> {
    const SEPARATORS: &[char] = &[
        '\n', ',', '，', '|', '\r', '\t', '#', '*', '-', '_', '+', '@', '~', '/', '\\', '.', '&',
        ' ', '、',
    ];
    if answer
        .chars()
        .all(|ch| ch.is_whitespace() || SEPARATORS.contains(&ch))
    {
        return None;
    }
    for &sep in SEPARATORS {
        if !answer.contains(sep) {
            continue;
        }
        let parts: Vec<String> = answer
            .split(sep)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    let stripped = answer.trim();
    (!stripped.is_empty()).then(|| vec![stripped.to_owned()])
}

/// 单选答案：句内的逗号属于正常标点，只有出现明显的多段分隔符时才判定为非单选。
pub fn check_single(answer: &str) -> bool {
    let text = answer.trim();
    if text.is_empty() {
        return false;
    }
    const STRONG: &[char] = &['\n', '|', '#', '\t', '\r', '、'];
    !STRONG
        .iter()
        .any(|&sep| text.split(sep).filter(|p| !p.trim().is_empty()).count() > 1)
}

pub fn check_multiple(answer: &str) -> bool {
    cut(answer).is_some_and(|v| !v.is_empty())
}

/// 判断题答案归类：`Some(true)` 正确、`Some(false)` 错误、`None` 无法判断。
pub fn judge(answer: &str, true_list: &[String], false_list: &[String]) -> Option<bool> {
    let val = answer.trim().to_lowercase();
    if BUILTIN_TRUE.contains(&val.as_str()) || true_list.iter().any(|x| x.to_lowercase() == val) {
        Some(true)
    } else if BUILTIN_FALSE.contains(&val.as_str())
        || false_list.iter().any(|x| x.to_lowercase() == val)
    {
        Some(false)
    } else {
        None
    }
}

/// 校验题库返回的答案是否与题型相符。
pub fn check_answer(answer: &str, q_type: QuestionType, settings: &TikuSettings) -> bool {
    let is_judgement = || judge(answer, &settings.true_list, &settings.false_list).is_some();
    match q_type {
        QuestionType::Single => check_single(answer) && !is_judgement(),
        QuestionType::Multiple => check_multiple(answer) && !is_judgement(),
        QuestionType::Completion => !answer.is_empty(),
        QuestionType::Judgement => is_judgement(),
        QuestionType::ShortAnswer | QuestionType::Unknown => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> TikuSettings {
        TikuSettings::default()
    }

    #[test]
    fn cut_uses_first_matching_separator() {
        assert_eq!(
            cut("A. 北京\nB. 上海"),
            Some(vec!["A. 北京".into(), "B. 上海".into()])
        );
        assert_eq!(cut("甲,乙，丙"), Some(vec!["甲".into(), "乙，丙".into()]));
        assert_eq!(cut("  单个  "), Some(vec!["单个".into()]));
        assert_eq!(cut("   "), None);
        assert_eq!(cut(",,"), None);
    }

    #[test]
    fn single_allows_inner_commas() {
        // "、" 视为多段答案的分隔符。
        assert!(!check_single("工人阶级领导的、以工农联盟为基础的"));
        assert!(check_single("中国，一个伟大的国家"));
        assert!(!check_single("A\nB"));
        assert!(!check_single("  "));
    }

    #[test]
    fn answers_are_checked_by_type() {
        let s = settings();
        assert!(check_answer("北京", QuestionType::Single, &s));
        assert!(!check_answer("正确", QuestionType::Single, &s));
        assert!(check_answer("北京\n上海", QuestionType::Multiple, &s));
        assert!(check_answer("√", QuestionType::Judgement, &s));
        assert!(!check_answer("也许", QuestionType::Judgement, &s));
        assert!(check_answer("任意", QuestionType::ShortAnswer, &s));
        assert!(!check_answer("", QuestionType::Completion, &s));
    }

    #[test]
    fn judgement_respects_custom_lists() {
        let true_list = vec!["Right".to_owned()];
        assert_eq!(judge("right", &true_list, &[]), Some(true));
        assert_eq!(judge(" 不正确 ", &[], &[]), Some(false));
        assert_eq!(judge("maybe", &true_list, &[]), None);
    }
}
