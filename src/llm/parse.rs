//! 解析大模型的回答。
//!
//! 模型输出并不总是规范的 JSON：可能包裹在 Markdown 代码块中、带有 `<think>` 思考过程、
//! 在 JSON 前后夹杂说明文字，或者干脆只回答"B"。这里尽量稳健地提取出答案列表，
//! 再根据题型转换成题库流程使用的答案文本。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::prompt::ChoiceOption;
use crate::models::{Question, QuestionType};
use crate::tiku::check::{BUILTIN_FALSE, BUILTIN_TRUE};

static THINK_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<think(?:ing)?>.*?</think(?:ing)?>").unwrap());
static CODE_FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[A-Za-z0-9_-]*\s*(.*?)```").unwrap());
static ANSWER_LETTERS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:答案|answer|选择|应选|选项)\s*(?:是|为|应为)?\s*[:：]?\s*([A-H](?:[\s,，、;；和]*[A-H])*)(?:[^A-Za-z]|$)").unwrap()
});
static LETTERS_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\(（【\[]?([A-Za-z](?:[\s,，、;；/]*[A-Za-z])*)[\)）】\]]?[.．。、:：]?$")
        .unwrap()
});

/// 去掉推理模型输出中的思考过程。
pub fn strip_reasoning(text: &str) -> String {
    let without_blocks = THINK_BLOCK_RE.replace_all(text, "");
    // 部分服务只输出了闭合标签
    let tail = match without_blocks.rfind("</think>") {
        Some(pos) => &without_blocks[pos + "</think>".len()..],
        None => &without_blocks,
    };
    tail.trim().to_owned()
}

/// 从模型输出中提取 `answer` 列表；无法识别为 JSON 时返回 `None`。
pub fn extract_answer_items(text: &str) -> Option<Vec<String>> {
    let text = strip_reasoning(text);
    let mut candidates: Vec<String> = CODE_FENCE_RE
        .captures_iter(&text)
        .map(|c| c[1].trim().to_owned())
        .collect();
    candidates.push(text.clone());
    for candidate in &candidates {
        if let Ok(value) = serde_json::from_str::<Value>(candidate)
            && let Some(items) = answer_from_value(&value)
        {
            return Some(items);
        }
    }
    // 在文本中寻找内嵌的 JSON 对象
    for (start, _) in text.match_indices('{') {
        if let Some(end) = matching_brace(&text[start..])
            && let Ok(value) = serde_json::from_str::<Value>(&text[start..start + end])
            && let Some(items) = answer_from_value(&value)
        {
            return Some(items);
        }
    }
    None
}

fn answer_from_value(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Object(map) => {
            let answer = map
                .iter()
                .find(|(k, _)| matches!(k.to_lowercase().as_str(), "answer" | "answers"))?
                .1;
            let items = value_to_items(answer);
            (!items.is_empty()).then_some(items)
        }
        Value::Array(_) => {
            let items = value_to_items(value);
            (!items.is_empty()).then_some(items)
        }
        _ => None,
    }
}

fn value_to_items(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().flat_map(value_to_items).collect(),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                Vec::new()
            } else {
                vec![s.to_owned()]
            }
        }
        Value::Bool(b) => vec![if *b { "正确" } else { "错误" }.to_owned()],
        Value::Number(n) => vec![n.to_string()],
        _ => Vec::new(),
    }
}

/// 返回与开头 `{` 匹配的 `}` 之后的位置（考虑字符串与转义）。
fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// 模型没有按 JSON 输出时的兜底解析（保守处理，识别不了就放弃）。
pub fn fallback_items(text: &str, q_type: QuestionType) -> Option<Vec<String>> {
    let text = strip_reasoning(text);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match q_type {
        QuestionType::Single | QuestionType::Multiple => {
            if let Some(c) = LETTERS_ONLY_RE.captures(text) {
                return Some(vec![c[1].to_owned()]);
            }
            ANSWER_LETTERS_RE
                .captures(text)
                .map(|c| vec![c[1].to_owned()])
        }
        QuestionType::Judgement => {
            let lower = text.to_lowercase();
            let negative = ["错误", "不正确", "不对", "不是", "×", "false", "错", "否"];
            let positive = ["正确", "对", "√", "true", "是"];
            if negative.iter().any(|k| lower.contains(k)) {
                Some(vec!["错误".to_owned()])
            } else if positive.iter().any(|k| lower.contains(k)) {
                Some(vec!["正确".to_owned()])
            } else {
                None
            }
        }
        QuestionType::Completion | QuestionType::ShortAnswer | QuestionType::Unknown => {
            (text.chars().count() <= 2000).then(|| vec![text.to_owned()])
        }
    }
}

/// 把答案列表转换为题库流程使用的答案文本：
/// - 选择题：字母换成选项内容，多个答案以换行分隔；
/// - 判断题：统一为"正确"/"错误"；
/// - 填空/简答：各项以换行连接。
pub fn finalize_answer(
    question: &Question,
    options: &[ChoiceOption],
    items: &[String],
) -> Option<String> {
    match question.q_type {
        QuestionType::Single | QuestionType::Multiple => {
            let single = question.q_type == QuestionType::Single;
            let mut letters: Vec<char> = Vec::new();
            let mut texts: Vec<String> = Vec::new();
            for item in items {
                match letters_of(item, options, single) {
                    Letters::Valid(found) => letters.extend(found),
                    Letters::Invalid => {}
                    Letters::NotLetters => texts.push(item.trim().to_owned()),
                }
            }
            let mut answers: Vec<String> = options
                .iter()
                .filter(|o| letters.contains(&o.letter))
                .map(|o| {
                    if o.text.is_empty() {
                        o.letter.to_string()
                    } else {
                        o.text.clone()
                    }
                })
                .collect();
            answers.extend(texts.into_iter().filter(|t| !t.is_empty()));
            if single {
                answers.truncate(1);
            }
            (!answers.is_empty()).then(|| answers.join("\n"))
        }
        QuestionType::Judgement => {
            let item = items.first()?.trim();
            let lower = item.to_lowercase();
            Some(if BUILTIN_TRUE.contains(&lower.as_str()) {
                "正确".to_owned()
            } else if BUILTIN_FALSE.contains(&lower.as_str()) {
                "错误".to_owned()
            } else {
                item.to_owned()
            })
        }
        QuestionType::Completion | QuestionType::ShortAnswer | QuestionType::Unknown => {
            let joined = items
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
    }
}

enum Letters {
    /// 由有效的选项字母构成
    Valid(Vec<char>),
    /// 形如字母答案但不是有效选项（如三个选项时回答 "Z"），丢弃
    Invalid,
    /// 不是字母答案，按选项内容处理
    NotLetters,
}

/// 判断答案项是否只由选项字母构成（如 "B"、"A、C"、"(D)"、"ACD"）。
fn letters_of(item: &str, options: &[ChoiceOption], single: bool) -> Letters {
    let item = item.trim();
    // 选项内容本身就是字母（如英语题选项 "a"、"ABC"）时按内容处理
    if options.iter().any(|o| o.text.eq_ignore_ascii_case(item)) {
        return Letters::NotLetters;
    }
    let Some(caps) = LETTERS_ONLY_RE.captures(item) else {
        return Letters::NotLetters;
    };
    let letters: Vec<char> = caps[1]
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if letters
        .iter()
        .all(|l| options.iter().any(|o| o.letter == *l))
    {
        Letters::Valid(if single {
            letters.into_iter().take(1).collect()
        } else {
            letters
        })
    } else if letters.len() <= options.len().max(1) {
        Letters::Invalid
    } else {
        // 较长的纯字母串（如 "DNA"、"ATP"）更可能是选项内容
        Letters::NotLetters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::prompt::parse_options;

    fn q(q_type: QuestionType, options: &[&str]) -> Question {
        let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        Question::new("t", q_type, &options)
    }

    #[test]
    fn extracts_json_from_various_formats() {
        let cases = [
            (r#"{"answer": ["B"]}"#, vec!["B"]),
            ("```json\n{\"Answer\": [\"A\", \"C\"]}\n```", vec!["A", "C"]),
            (
                "<think>先分析……{\"answer\":[\"X\"]}</think>\n{\"answer\": [\"D\"]}",
                vec!["D"],
            ),
            (
                "好的，答案如下：{\"answer\": \"正确\"} 希望有帮助",
                vec!["正确"],
            ),
            (r#"{"answer": true}"#, vec!["正确"]),
            (r#"["北京", "上海"]"#, vec!["北京", "上海"]),
            (
                r#"{"answer": ["含有}括号的\"内容"]}"#,
                vec!["含有}括号的\"内容"],
            ),
            ("思考过程</think>{\"answer\":[\"A\"]}", vec!["A"]),
        ];
        for (input, expected) in cases {
            assert_eq!(
                extract_answer_items(input),
                Some(expected.iter().map(|s| s.to_string()).collect()),
                "{input}"
            );
        }
        assert_eq!(extract_answer_items("我不知道"), None);
        assert_eq!(extract_answer_items(r#"{"answer": []}"#), None);
    }

    #[test]
    fn fallback_parsing_is_conservative() {
        assert_eq!(
            fallback_items("B", QuestionType::Single),
            Some(vec!["B".into()])
        );
        assert_eq!(
            fallback_items("答案：A、C", QuestionType::Multiple),
            Some(vec!["A、C".into()])
        );
        assert_eq!(
            fallback_items("这句话是不正确的", QuestionType::Judgement),
            Some(vec!["错误".into()])
        );
        assert_eq!(
            fallback_items("正确", QuestionType::Judgement),
            Some(vec!["正确".into()])
        );
        assert_eq!(fallback_items("我认为选第三个", QuestionType::Single), None);
        assert_eq!(
            fallback_items("牛顿", QuestionType::Completion),
            Some(vec!["牛顿".into()])
        );
    }

    #[test]
    fn letters_are_mapped_to_option_text() {
        let single = q(QuestionType::Single, &["A. 上海", "B. 北京", "C. ABC"]);
        let opts = parse_options(&single);
        assert_eq!(
            finalize_answer(&single, &opts, &["B".into()]),
            Some("北京".into())
        );
        assert_eq!(
            finalize_answer(&single, &opts, &["(B)".into()]),
            Some("北京".into())
        );
        // 选项内容本身是字母时按内容处理
        assert_eq!(
            finalize_answer(&single, &opts, &["ABC".into()]),
            Some("ABC".into())
        );
        // 模型直接返回了选项内容
        assert_eq!(
            finalize_answer(&single, &opts, &["上海".into()]),
            Some("上海".into())
        );
        // 无效字母被丢弃，较长的纯字母串按内容处理
        assert_eq!(finalize_answer(&single, &opts, &["Z".into()]), None);
        assert_eq!(
            finalize_answer(&single, &opts, &["DNAX".into()]),
            Some("DNAX".into())
        );
        assert_eq!(
            fallback_items("答案是B选项", QuestionType::Single),
            Some(vec!["B".into()])
        );
        assert_eq!(fallback_items("答案是Because", QuestionType::Single), None);

        let multi = q(
            QuestionType::Multiple,
            &["A. 甲", "B. 乙", "C. 丙", "D. 丁"],
        );
        let opts = parse_options(&multi);
        assert_eq!(
            finalize_answer(&multi, &opts, &["D".into(), "A".into()]),
            Some("甲\n丁".into())
        );
        assert_eq!(
            finalize_answer(&multi, &opts, &["ACD".into()]),
            Some("甲\n丙\n丁".into())
        );
        assert_eq!(
            finalize_answer(&multi, &opts, &["A、B".into()]),
            Some("甲\n乙".into())
        );
    }

    #[test]
    fn judgement_and_completion_answers() {
        let judge = q(QuestionType::Judgement, &[]);
        assert_eq!(
            finalize_answer(&judge, &[], &["对".into()]),
            Some("正确".into())
        );
        assert_eq!(
            finalize_answer(&judge, &[], &["False".into()]),
            Some("错误".into())
        );
        let blank = q(QuestionType::Completion, &[]);
        assert_eq!(
            finalize_answer(&blank, &[], &["牛顿".into(), " 第二定律 ".into()]),
            Some("牛顿\n第二定律".into())
        );
        assert_eq!(finalize_answer(&blank, &[], &[]), None);
    }
}
