//! 手动输入题库：在终端中显示题目，由用户输入答案（批量或逐题）。

use std::sync::LazyLock;

use regex::Regex;

use super::check::{BUILTIN_FALSE, BUILTIN_TRUE, cut};
use crate::config::Section;
use crate::console::{CONSOLE, read_line};
use crate::models::{Question, QuestionType};

static LETTER_SEPARATORS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s,，;；、]+").unwrap());
static OPTION_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]\s*[.、:：)?）]?\s*").unwrap());

const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Batch,
    Single,
}

pub struct TikuManual {
    mode: Mode,
    separator: String,
}

impl TikuManual {
    pub fn new(section: &Section) -> Self {
        let mode = match section
            .get_or("manual_mode_default", "batch")
            .to_lowercase()
            .as_str()
        {
            "single" => Mode::Single,
            _ => Mode::Batch,
        };
        let raw = section.get_or("manual_mode_separator", ";");
        let separator = match raw.to_lowercase().as_str() {
            "\\n" | "newline" | "换行" => "\n",
            "space" | "空格" => " ",
            "tab" | "制表符" => "\t",
            _ => raw,
        }
        .to_owned();
        Self { mode, separator }
    }

    pub async fn query_all(&self, questions: &[Question]) -> Vec<Option<String>> {
        let questions = questions.to_vec();
        let mode = self.mode;
        let separator = self.separator.clone();
        CONSOLE
            .interact(move || {
                println!(
                    "\n{} 手动输入题库 (共 {} 题) {}",
                    "=".repeat(20),
                    questions.len(),
                    "=".repeat(20)
                );
                match mode {
                    Mode::Batch => batch_flow(&questions, &separator),
                    Mode::Single => questions.iter().map(single_query).collect(),
                }
            })
            .await
    }
}

fn parse_options(options: &str) -> Vec<String> {
    let parts: Vec<String> = options
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if parts.len() <= 1
        && let Some(cut_parts) = cut(options)
    {
        return cut_parts;
    }
    parts
}

fn valid_keys(parts: &[String]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|p| p.chars().next())
        .filter(|c| c.is_alphabetic())
        .map(|c| c.to_uppercase().to_string())
        .collect()
}

fn option_letters(answer: &str) -> Vec<String> {
    let cleaned = LETTER_SEPARATORS_RE.replace_all(answer, "");
    if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_ascii_alphabetic()) {
        return Vec::new();
    }
    cleaned
        .chars()
        .map(|c| c.to_ascii_uppercase().to_string())
        .collect()
}

fn question_line(q: &Question) -> String {
    let kind = q.q_type.display();
    match q.q_type {
        QuestionType::Single | QuestionType::Multiple if !q.options.is_empty() => {
            format!(
                "【{kind}】 {} 选项: {}",
                q.title,
                parse_options(&q.options).join("  ")
            )
        }
        QuestionType::Judgement => format!("【{kind}】 {} 选项: 正确 / 错误", q.title),
        _ => format!("【{kind}】 {}", q.title),
    }
}

fn validate_input(answer: &str, q: &Question) -> Result<(), String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(());
    }
    match q.q_type {
        QuestionType::Judgement => {
            let val = answer.to_lowercase();
            if BUILTIN_TRUE.contains(&val.as_str()) || BUILTIN_FALSE.contains(&val.as_str()) {
                Ok(())
            } else {
                Err(format!(
                    "无法识别的判断词 '{answer}'，请输入：对/错、正确/错误、T/F、1/0"
                ))
            }
        }
        QuestionType::Single | QuestionType::Multiple => {
            let parts = parse_options(&q.options);
            let keys = valid_keys(&parts);
            if keys.is_empty() {
                return Ok(());
            }
            let letters = option_letters(answer);
            if letters.is_empty() {
                return validate_text_match(answer, &parts);
            }
            let invalid: Vec<&String> = letters.iter().filter(|l| !keys.contains(l)).collect();
            if !invalid.is_empty() {
                return Err(format!(
                    "输入包含无效的选项字母 {invalid:?}，当前题目的可用选项为: {}",
                    keys.join(", ")
                ));
            }
            if q.q_type == QuestionType::Single && letters.len() > 1 {
                return Err("当前是单选题，但输入了多个选项字母！".to_owned());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_text_match(answer: &str, parts: &[String]) -> Result<(), String> {
    for item in cut(answer).unwrap_or_default() {
        let item_lower = item.trim().to_lowercase();
        let matched = parts.iter().any(|p| {
            let norm = OPTION_PREFIX_RE.replace(p, "").trim().to_lowercase();
            norm.contains(&item_lower) || item_lower.contains(&norm)
        });
        if !matched {
            return Err(format!(
                "输入的文本 '{item}' 在所有选项中均无法匹配，请输入合法的选项文本或字母"
            ));
        }
    }
    Ok(())
}

fn normalize_input(answer: &str, q: &Question) -> Option<String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return None;
    }
    Some(match q.q_type {
        QuestionType::Judgement => {
            let val = answer.to_lowercase();
            if BUILTIN_TRUE.contains(&val.as_str()) {
                "正确".to_owned()
            } else if BUILTIN_FALSE.contains(&val.as_str()) {
                "错误".to_owned()
            } else {
                answer.to_owned()
            }
        }
        QuestionType::Single | QuestionType::Multiple => {
            let keys = valid_keys(&parse_options(&q.options));
            let letters = option_letters(answer);
            if !letters.is_empty() && letters.iter().all(|l| keys.contains(l)) {
                let mut unique: Vec<String> = Vec::new();
                for l in letters {
                    if !unique.contains(&l) {
                        unique.push(l);
                    }
                }
                unique.join("\n")
            } else {
                cut(answer)
                    .map(|parts| parts.join("\n"))
                    .unwrap_or_else(|| answer.to_owned())
            }
        }
        _ => answer.to_owned(),
    })
}

fn single_query(q: &Question) -> Option<String> {
    println!("\n{}", question_line(q));
    loop {
        let answer = read_line("请输入答案 (直接回车表示跳过/无答案): ").unwrap_or_default();
        let answer = answer.trim();
        if answer.is_empty() {
            println!("  [已记录] 题目: {} ---> 答案: [跳过/随机]", q.title);
            return None;
        }
        if let Err(msg) = validate_input(answer, q) {
            println!("  {RED}[输入错误] {msg}{RESET}");
            continue;
        }
        let normalized = normalize_input(answer, q);
        println!(
            "  [已记录] 题目: {} ---> 答案: {}",
            q.title,
            normalized.as_deref().unwrap_or_default()
        );
        return normalized;
    }
}

fn separator_description(separator: &str) -> String {
    match separator {
        "\n" => "换行 (每题一行)".to_owned(),
        " " => "空格".to_owned(),
        "\t" => "Tab制表符".to_owned(),
        other => other.to_owned(),
    }
}

/// 按分隔符拆分批量输入的答案，并补齐/截断到题目数量。
fn split_batch_answers(raw: &str, expected: usize, separator: &str) -> Vec<String> {
    if raw.is_empty() {
        return vec![String::new(); expected];
    }
    let mut answers: Vec<String> = match separator {
        ";" | "；" => raw
            .replace('；', ";")
            .split(';')
            .map(|s| s.trim().to_owned())
            .collect(),
        "," | "，" => raw
            .replace('，', ",")
            .split(',')
            .map(|s| s.trim().to_owned())
            .collect(),
        sep => raw.split(sep).map(|s| s.trim().to_owned()).collect(),
    };
    answers.resize(expected, String::new());
    answers
}

fn batch_flow(questions: &[Question], separator: &str) -> Vec<Option<String>> {
    for (i, q) in questions.iter().enumerate() {
        println!("\n[{}] {}", i + 1, question_line(q));
    }
    let desc = separator_description(separator);
    println!("\n{}", "=".repeat(50));
    println!("请依次输入每道题的答案。");
    println!("格式要求：当前配置要求使用【{desc}】分割各题的答案。");
    println!("如果是多选题，答案中的多个选项直接连着写即可（例如：AB 或 AC）。");
    println!("直接按回车或输入空格跳过的题，对应的答案将为空（会触发随机答题）。");
    if separator == "\n" {
        println!("粘贴多行时，每行会被解析为对应一题的答案。");
    } else {
        println!("示例输入: A{separator} B{separator} 正确{separator} 答案1, 答案2{separator} 错");
    }
    println!("{}", "=".repeat(50));

    loop {
        let answers: Vec<String> = if separator == "\n" {
            println!(
                "请直接粘贴或依次输入各题答案（每行一个，共 {} 行）：",
                questions.len()
            );
            (1..=questions.len())
                .map(|i| {
                    read_line(&format!("  第 {i} 题答案: "))
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                })
                .collect()
        } else {
            let raw = read_line(&format!(
                "\n请一次性输入所有题目的答案 (使用 '{desc}' 分割): "
            ))
            .unwrap_or_default();
            split_batch_answers(raw.trim(), questions.len(), separator)
        };

        println!("\n--- 解析答案结果 ---");
        let mut has_error = false;
        let mut parsed = Vec::with_capacity(questions.len());
        for (i, (q, answer)) in questions.iter().zip(&answers).enumerate() {
            match validate_input(answer, q) {
                Err(msg) => {
                    has_error = true;
                    println!(
                        "第 {} 题: {} ---> {RED}[错误: {msg}]{RESET}",
                        i + 1,
                        q.title
                    );
                    parsed.push(None);
                }
                Ok(()) => {
                    let normalized = normalize_input(answer, q);
                    println!(
                        "第 {} 题: {} ---> 答案: {}",
                        i + 1,
                        q.title,
                        normalized.as_deref().unwrap_or("[跳过/随机]")
                    );
                    parsed.push(normalized);
                }
            }
        }
        println!("-------------------");
        if has_error {
            println!("{RED}检测到存在不合规的答案，已拒绝确认，请重新输入！{RESET}");
            continue;
        }
        let confirm = read_line("确认使用上述答案？[Y/n]: ")
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        match confirm.as_str() {
            "" | "y" | "yes" => return parsed,
            "switch" => return questions.iter().map(single_query).collect(),
            _ => println!("已取消，请重新输入，或输入 'switch' 切换为单题输入模式。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(q_type: QuestionType, options: &[&str]) -> Question {
        let options: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        Question::new("题目", q_type, &options)
    }

    #[test]
    fn choice_input_validation() {
        let single = q(QuestionType::Single, &["A. 北京", "B. 上海"]);
        assert!(validate_input("a", &single).is_ok());
        assert!(
            validate_input("C", &single)
                .unwrap_err()
                .contains("无效的选项字母")
        );
        assert!(
            validate_input("AB", &single)
                .unwrap_err()
                .contains("单选题")
        );
        assert!(validate_input("北京", &single).is_ok());
        assert!(validate_input("广州", &single).is_err());
        assert_eq!(normalize_input("b", &single).as_deref(), Some("B"));

        let multi = q(QuestionType::Multiple, &["A. 甲", "B. 乙", "C. 丙"]);
        assert_eq!(normalize_input("c a, c", &multi).as_deref(), Some("C\nA"));
    }

    #[test]
    fn judgement_input() {
        let judge = q(QuestionType::Judgement, &[]);
        assert!(validate_input("也许", &judge).is_err());
        assert_eq!(normalize_input("T", &judge).as_deref(), Some("正确"));
        assert_eq!(normalize_input("不对", &judge).as_deref(), Some("错误"));
        assert_eq!(normalize_input("  ", &judge), None);
    }

    #[test]
    fn batch_answers_are_split_and_padded() {
        assert_eq!(
            split_batch_answers("A；B;  ", 4, ";"),
            vec!["A", "B", "", ""]
        );
        assert_eq!(split_batch_answers("A,B,C", 2, "，"), vec!["A", "B"]);
        assert_eq!(split_batch_answers("", 2, " "), vec!["", ""]);
    }
}
