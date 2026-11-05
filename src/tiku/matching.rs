//! 把题库答案匹配到具体选项，以及没有答案时的随机作答。

use std::sync::LazyLock;

use rand::Rng;
use rand::seq::{IndexedRandom, SliceRandom};
use regex::Regex;
use tracing::{debug, error, info, warn};

use super::TikuSettings;
use super::check::{cut, judge};
use crate::models::{Question, QuestionType};
use crate::similarity;

static OPTION_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z](?:\s*[.、:：)）]\s*|\s+|$)").unwrap());
static CLEAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z](?:\s*[.、:：)）]\s*|\s+|$)|[.,!?;:，。！？；：]").unwrap()
});
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static NORMALIZE_PUNCT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[，。！？；：,.!?;:()（）\[\]【】"“”‘’\-_/\\|]"#).unwrap());
static LETTER_ANSWER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z])\s*[.、:：)）]?$").unwrap());

/// 相似度兜底匹配的最低得分。
const SIMILARITY_THRESHOLD: f64 = 0.65;

/// 去掉答案开头的选项字母与标点（长度为 1 的答案保持不变，防止误删单个字母答案）。
pub fn clean_answer_items(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|c| {
            if c.chars().count() > 1 {
                CLEAN_RE.replace_all(c, "").trim().to_owned()
            } else {
                c.trim().to_owned()
            }
        })
        .collect()
}

/// 去掉选项开头的字母编号，例如 "A. 北京" -> "北京"。
pub fn strip_option_prefix(option: &str) -> String {
    OPTION_PREFIX_RE.replace(option, "").trim().to_owned()
}

/// 用于比较的归一化文本：统一异体字、去掉字母编号、空白与标点，转小写。
pub fn normalize_text(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(|c| match c {
            '⻛' => '风',
            '⻔' => '门',
            '⻋' => '车',
            '⻢' => '马',
            other => other,
        })
        .collect();
    let s = OPTION_PREFIX_RE.replace(&mapped, "");
    let s = WHITESPACE_RE.replace_all(&s, "");
    NORMALIZE_PUNCT_RE.replace_all(&s, "").to_lowercase()
}

/// `a` 是否为 `o` 的子序列（忽略大小写）。
pub fn is_subsequence(a: &str, o: &str) -> bool {
    let o = o.to_lowercase();
    let mut it = o.chars();
    a.to_lowercase().chars().all(|c| it.any(|x| x == c))
}

fn first_char(option: &str) -> String {
    option.chars().next().map(String::from).unwrap_or_default()
}

/// 相似度兜底：返回相似度最高且不低于阈值的选项字母。
pub fn match_best_option_by_similarity(
    target: &str,
    options: &[String],
    threshold: f64,
) -> Option<String> {
    let target = normalize_text(target);
    if target.is_empty() {
        return None;
    }
    let mut best: Option<(String, f64)> = None;
    for option in options {
        let norm = normalize_text(&strip_option_prefix(option));
        if norm.is_empty() {
            continue;
        }
        let score = similarity::ratio(&target, &norm);
        if best.as_ref().is_none_or(|(_, s)| score > *s) {
            best = Some((first_char(option), score));
        }
    }
    let (letter, score) = best?;
    if score >= threshold {
        info!("相似度兜底匹配成功: {letter} (score={score:.2}, threshold={threshold:.2})");
        Some(letter)
    } else {
        None
    }
}

/// 匹配一个答案项对应的选项字母。依次尝试：内容完全一致、选项字母、归一化后一致、子序列、相似度。
fn match_option(raw: &str, cleaned: &str, options: &[String]) -> Option<String> {
    let raw = raw.trim();
    if !raw.is_empty()
        && let Some(o) = options
            .iter()
            .find(|o| strip_option_prefix(o).eq_ignore_ascii_case(raw))
    {
        return Some(first_char(o));
    }
    if let Some(c) = LETTER_ANSWER_RE.captures(raw)
        && let Some(o) = options
            .iter()
            .find(|o| first_char(o).eq_ignore_ascii_case(&c[1]))
    {
        return Some(first_char(o));
    }
    let target = normalize_text(raw);
    if !target.is_empty()
        && let Some(o) = options
            .iter()
            .find(|o| normalize_text(&strip_option_prefix(o)) == target)
    {
        return Some(first_char(o));
    }
    if !cleaned.is_empty()
        && let Some(o) = options.iter().find(|o| is_subsequence(cleaned, o))
    {
        return Some(first_char(o));
    }
    match_best_option_by_similarity(cleaned, options, SIMILARITY_THRESHOLD)
}

/// 由选项字母组成的多选答案（如 "ACD"）
fn parse_multi_choice_letters(raw: &str, options: &[String]) -> Option<Vec<String>> {
    let raw = raw.trim();
    if raw.chars().count() < 2 || !raw.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if options
        .iter()
        .any(|o| strip_option_prefix(o).eq_ignore_ascii_case(raw))
    {
        return None;
    }
    let letters: Vec<String> = raw
        .chars()
        .map(|c| c.to_ascii_uppercase().to_string())
        .collect();
    letters
        .iter()
        .all(|l| options.iter().any(|o| first_char(o).to_uppercase() == *l))
        .then_some(letters)
}

/// 判断题答案转为布尔值；无法判断时随机选择。
pub fn choose_judgement_value(answer: &str, settings: &TikuSettings) -> bool {
    match judge(answer, &settings.true_list, &settings.false_list) {
        Some(v) => v,
        None => {
            error!(
                "无法判断答案 -> {answer} 对应的是正确还是错误，请自行判断并加入配置文件 true_list/false_list，本次将随机选择"
            );
            rand::rng().random_bool(0.5)
        }
    }
}

/// 将题库答案转换为提交用的答案：选择题为排序后的选项字母，判断题为 true/false。
/// 无法匹配时返回空串（调用方会改为随机作答）。
pub fn resolve_answer(question: &Question, res: &str, settings: &TikuSettings) -> String {
    match question.q_type {
        QuestionType::Multiple => {
            let options: Vec<String> = question
                .option_lines()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let (Some(res_list), false) = (cut(res), options.is_empty()) else {
                warn!(
                    "未能正确提取题目选项信息! 选项: {:?}, 答案: {res}",
                    question.options
                );
                return String::new();
            };
            let mut letters: Vec<String> = Vec::new();
            for (raw, cleaned) in res_list.iter().zip(clean_answer_items(&res_list)) {
                if let Some(multi) = parse_multi_choice_letters(raw, &options) {
                    letters.extend(multi);
                } else if let Some(letter) = match_option(raw, &cleaned, &options) {
                    letters.push(letter);
                }
            }
            letters.sort();
            letters.dedup();
            letters.concat()
        }
        QuestionType::Single => {
            let options: Vec<String> = question
                .option_lines()
                .into_iter()
                .map(str::to_owned)
                .collect();
            if options.is_empty() {
                warn!("未能正确提取题目选项信息! 选项: {:?}", question.options);
                return String::new();
            }
            let cleaned = clean_answer_items(&[res.to_owned()]).remove(0);
            match_option(res, &cleaned, &options).unwrap_or_default()
        }
        QuestionType::Judgement => if choose_judgement_value(res, settings) {
            "true"
        } else {
            "false"
        }
        .to_owned(),
        QuestionType::Completion | QuestionType::ShortAnswer | QuestionType::Unknown => {
            res.to_owned()
        }
    }
}

/// 没有找到答案时随机作答。
pub fn random_answer(question: &Question) -> String {
    let options = &question.options;
    if options.trim().is_empty() && question.q_type != QuestionType::Judgement {
        return String::new();
    }
    let mut rng = rand::rng();
    let answer = match question.q_type {
        QuestionType::Multiple => {
            let Some(mut list) = cut(options) else {
                error!("选项为空, 未能正确提取题目选项信息! 请反馈并提供以上信息");
                return String::new();
            };
            debug!("当前选项列表 -> {list:?}");
            let n = list.len();
            let count = match n {
                0 | 1 => n,
                2 => 2,
                3 => *[2, 3]
                    .choose_weighted(&mut rng, |&c| if c == 2 { 0.3 } else { 0.7 })
                    .unwrap_or(&3),
                _ => *[2, 3, 4]
                    .choose_weighted(&mut rng, |&c| match c {
                        2 => 0.1,
                        3 => 0.5,
                        _ => 0.4,
                    })
                    .unwrap_or(&3),
            };
            list.shuffle(&mut rng);
            let mut letters: Vec<String> = list.iter().take(count).map(|o| first_char(o)).collect();
            letters.sort();
            letters.concat()
        }
        QuestionType::Single => {
            let lines: Vec<&str> = options.split('\n').collect();
            lines
                .choose(&mut rng)
                .map(|o| first_char(o))
                .unwrap_or_default()
        }
        QuestionType::Judgement => if rng.random_bool(0.5) {
            "true"
        } else {
            "false"
        }
        .to_owned(),
        _ => String::new(),
    };
    info!("随机选择 -> {answer}");
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(q_type: QuestionType, options: &[&str]) -> Question {
        let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        Question::new("t", q_type, &options)
    }

    #[test]
    fn answer_text_helpers() {
        assert_eq!(
            clean_answer_items(&["A. 北京，".into(), "B".into()]),
            vec!["北京", "B"]
        );
        assert_eq!(clean_answer_items(&["Rust语言".into()]), vec!["Rust语言"]);
        assert_eq!(strip_option_prefix("B、 上海"), "上海");
        assert_eq!(normalize_text("A. 中国⻛ 格（ok）"), "中国风格ok");
        assert!(is_subsequence("北京", "A. 北京市"));
        assert!(!is_subsequence("京北", "A. 北京"));
    }

    #[test]
    fn answer_cleanup_preserves_unprefixed_words() {
        assert_eq!(clean_answer_items(&["Rust语言".into()]), vec!["Rust语言"]);
    }

    #[test]
    fn option_prefix_with_space_is_removed() {
        assert_eq!(strip_option_prefix("A 关西"), "关西");
        assert_eq!(clean_answer_items(&["A 关西".into()]), vec!["关西"]);
    }

    #[test]
    fn single_choice_matching() {
        let s = TikuSettings::default();
        let question = q(QuestionType::Single, &["A. Bxx公司", "B. 北京", "C. 上海"]);
        assert_eq!(resolve_answer(&question, "北京", &s), "B");
        assert_eq!(resolve_answer(&question, "B. 北京", &s), "B");
        // 单个字母直接按选项字母处理。
        assert_eq!(resolve_answer(&question, "B", &s), "B");
        assert_eq!(resolve_answer(&question, "上海市", &s), "C");
        assert_eq!(resolve_answer(&question, "广州", &s), "");

        // 选项内容本身是字母时按内容匹配（英语题）
        let english = q(QuestionType::Single, &["A. the", "B. an", "C. a"]);
        assert_eq!(resolve_answer(&english, "a", &s), "C");
        assert_eq!(resolve_answer(&english, "B", &s), "B");
        // 图片选项（只有字母没有内容）
        let images = q(QuestionType::Single, &["A", "B", "C"]);
        assert_eq!(resolve_answer(&images, "B", &s), "B");
    }

    #[test]
    fn multiple_choice_matching() {
        let s = TikuSettings::default();
        let question = q(
            QuestionType::Multiple,
            &["A. 甲方", "B. 乙方", "C. 丙方", "D. 丁方"],
        );
        assert_eq!(resolve_answer(&question, "丁方\n甲方", &s), "AD");
        assert_eq!(resolve_answer(&question, "ACD", &s), "ACD");
        assert_eq!(resolve_answer(&question, "A,C", &s), "AC");
        assert_eq!(resolve_answer(&question, "乙方#乙方", &s), "B");
    }

    #[test]
    fn completion_and_judgement() {
        let s = TikuSettings::default();
        assert_eq!(
            resolve_answer(&q(QuestionType::Judgement, &[]), "对", &s),
            "true"
        );
        assert_eq!(
            resolve_answer(&q(QuestionType::Judgement, &[]), "×", &s),
            "false"
        );
        assert_eq!(
            resolve_answer(&q(QuestionType::Completion, &[]), "牛顿", &s),
            "牛顿"
        );
    }

    #[test]
    fn random_answers_are_valid() {
        let multi = q(QuestionType::Multiple, &["A. 1", "B. 2", "C. 3", "D. 4"]);
        for _ in 0..50 {
            let a = random_answer(&multi);
            assert!((2..=4).contains(&a.len()), "{a}");
            let mut sorted: Vec<char> = a.chars().collect();
            sorted.sort();
            assert_eq!(sorted.iter().collect::<String>(), a);
        }
        let single = q(QuestionType::Single, &["A. 1", "B. 2"]);
        assert!(["A", "B"].contains(&random_answer(&single).as_str()));
        assert!(
            ["true", "false"].contains(&random_answer(&q(QuestionType::Judgement, &[])).as_str())
        );
        assert_eq!(random_answer(&q(QuestionType::Completion, &[])), "");
    }
}
